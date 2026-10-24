//! Closed, non-secret observations of native Provider failures. These are diagnostic hints,
//! never account identity proof, retry authority, or permission to start authentication.

/// What a native client reported. In particular, a contention report does not prove that
/// another process is still alive, and an unknown failure never implies a login is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeFailureKind {
    AuthMissing,
    AuthRevoked,
    AuthExpired,
    AuthRefreshContended,
    AuthRefreshFailed,
    AuthRejected,
    Quota,
    Capacity,
    ModelUnavailable,
    Network,
    /// The installed CLI refused af's own arguments (`unknown option`, `unexpected argument`, a
    /// usage block): it is older than, or otherwise different from, what af invokes.
    UnsupportedInvocation,
    /// The bound model's safety classifier flagged the request and the model refused it: the
    /// result reports `safety_stops` above zero or `stop_reason` `refusal`, or the client's
    /// error says its safeguards flagged the message. The same input is flagged again, so the
    /// Attempt is never retried automatically.
    ModelRefused,
    /// The machine the Provider runs on failed af, not the model or the login: its CLI cannot
    /// start or serve its protocol, or its identity probe proved nothing within the Attempt's
    /// wall (ADR-0202).
    Environment,
    Unknown,
}

impl NativeFailureKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 14] = [
        Self::AuthMissing,
        Self::AuthRevoked,
        Self::AuthExpired,
        Self::AuthRefreshContended,
        Self::AuthRefreshFailed,
        Self::AuthRejected,
        Self::Quota,
        Self::Capacity,
        Self::ModelUnavailable,
        Self::Network,
        Self::UnsupportedInvocation,
        Self::ModelRefused,
        Self::Environment,
        Self::Unknown,
    ];

    /// Whether `stdout` and `stderr` are what [`Self::redact_capture`] leaves when it replaces
    /// both streams: one closed diagnostic and nothing else. Telemetry's release decision reads
    /// it after the adapter's filter (design §7.3), so it learns that the adapter withheld the
    /// streams without the adapter telling it.
    pub fn replaced_capture(stdout: &[u8], stderr: &[u8]) -> bool {
        stderr.is_empty()
            && Self::ALL
                .iter()
                .any(|kind| kind.diagnostic().as_bytes() == stdout)
    }

    pub fn is_auth(self) -> bool {
        matches!(
            self,
            Self::AuthMissing
                | Self::AuthRevoked
                | Self::AuthExpired
                | Self::AuthRefreshContended
                | Self::AuthRefreshFailed
                | Self::AuthRejected
        )
    }

    /// A closed allowlist: no native text, account identity, URL, code or token is copied.
    pub fn diagnostic(self) -> &'static str {
        match self {
            Self::AuthMissing => "Provider credentials are missing (auth_missing)",
            Self::AuthRevoked => "Provider credentials were revoked (auth_revoked)",
            Self::AuthExpired => "Provider credentials expired (auth_expired)",
            Self::AuthRefreshContended => {
                "Provider reported refresh contention; another refresh may be active or stale (auth_refresh_contended)"
            }
            Self::AuthRefreshFailed => "Provider credential refresh failed (auth_refresh_failed)",
            Self::AuthRejected => "Provider authentication was rejected (auth_rejected)",
            Self::Quota => "Provider quota or rate limit was reached (quota)",
            Self::Capacity => "Provider model at capacity (capacity)",
            Self::ModelUnavailable => "Provider model is unavailable (model_unavailable)",
            Self::Network => "Provider network request failed (network)",
            Self::UnsupportedInvocation => {
                "the installed Provider CLI does not accept af's invocation (unsupported_invocation)"
            }
            Self::ModelRefused => {
                "the bound model's safety classifier flagged the request (model_refused)"
            }
            Self::Environment => {
                "the Provider's environment failed, not the model or the login (environment)"
            }
            Self::Unknown => "Provider returned an unclassified failure (unknown)",
        }
    }

    /// The classification as an Attempt diagnostic spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthMissing => "auth_missing",
            Self::AuthRevoked => "auth_revoked",
            Self::AuthExpired => "auth_expired",
            Self::AuthRefreshContended => "auth_refresh_contended",
            Self::AuthRefreshFailed => "auth_refresh_failed",
            Self::AuthRejected => "auth_rejected",
            Self::Quota => "quota",
            Self::Capacity => "capacity",
            Self::ModelUnavailable => "model_unavailable",
            Self::Network => "network",
            Self::UnsupportedInvocation => "unsupported_invocation",
            Self::ModelRefused => "model_refused",
            Self::Environment => "environment",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the Provider refused this exact input, so that the same input sent again is
    /// refused again: such an Attempt is not retried automatically (ADR-0147).
    pub fn refuses_input(self) -> bool {
        self == Self::ModelRefused
    }

    /// Why an Attempt that ended with this failure and no usage report has unknown usage
    /// (ADR-0143). An unclassified failure tells nothing better than an absent report.
    pub fn unknown_usage_cause(self) -> review_core::task::usage::TaskUnknownUsageCauseV1 {
        use review_core::task::usage::TaskUnknownUsageCauseV1 as Cause;
        match self {
            Self::AuthMissing
            | Self::AuthRevoked
            | Self::AuthExpired
            | Self::AuthRefreshContended
            | Self::AuthRefreshFailed
            | Self::AuthRejected => Cause::Authentication,
            Self::Quota => Cause::RateLimit,
            Self::Capacity => Cause::Capacity,
            Self::ModelUnavailable => Cause::ModelUnavailable,
            Self::Network => Cause::Network,
            // The CLI refused its arguments before any model call: nothing better than an
            // absent report is known about usage. A classifier refusal normally reports its
            // usage and is charged it; when it does not, nothing better is known either
            // (ADR-0147).
            Self::UnsupportedInvocation
            | Self::ModelRefused
            | Self::Environment
            | Self::Unknown => Cause::Unreported,
        }
    }

    /// The transient class of this failure, when the same input may get past it after a wait
    /// (ADR-0202): a network failure or a model at capacity. Quota, authentication, a refused
    /// invocation and a classifier refusal recur, and are never retried in the Attempt.
    pub fn transient(self) -> Option<TransientClass> {
        match self {
            Self::Network => Some(TransientClass::Network),
            Self::Capacity => Some(TransientClass::Capacity),
            _ => None,
        }
    }

    /// Auth output is not ordinary evidence. Retain the closed classification, while callers
    /// keep parsed usage separately before replacing both streams ahead of CAS capture. Also
    /// suppress an obvious auth challenge/token on a non-auth failure (for example, a failed
    /// network request during refresh), without changing that failure's classification.
    pub fn redact_auth_capture(self, stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) {
        self.redact_capture(false, stdout, stderr);
    }

    /// [`Self::redact_auth_capture`] for a failure classified as `self` whose streams a native
    /// classification also read as auth output (`auth_seen`): an invocation error takes the
    /// classification, but the streams stay as sensitive as the auth text made them.
    pub fn redact_capture(self, auth_seen: bool, stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) {
        if auth_seen || self.is_auth() || auth_material(stdout) || auth_material(stderr) {
            *stdout = self.diagnostic().as_bytes().to_vec();
            // One summary, not two identical CAS IDs: Attempt raw-evidence references are
            // required to be distinct. Neither original stream survives the replacement.
            stderr.clear();
        }
    }
}

/// A Provider failure class the same input may get past after a wait (ADR-0202).
pub use review_core::task::provider_event::TransientClassV1 as TransientClass;

/// A transient failure of one native call, and the wait the Provider asked for, if it named one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transient {
    pub class: TransientClass,
    pub retry_after: Option<std::time::Duration>,
}

impl Transient {
    pub fn new(class: TransientClass) -> Self {
        Self {
            class,
            retry_after: None,
        }
    }

    /// This failure with the wait `text` names (`Retry-After: 30`, `retry after 30 seconds`,
    /// `try again in 2 minutes`), when it names one.
    pub fn with_retry_after_in(mut self, text: &str) -> Self {
        self.retry_after = retry_after(text);
        self
    }
}

/// The wait a native failure text asks for, in seconds or minutes: `Retry-After: 30`,
/// `retry-after 30`, `retry after 30 seconds`, `try again in 30s`, `try again in 2 minutes`. A
/// value above one hour is not a wait af honours: None.
pub fn retry_after(text: &str) -> Option<std::time::Duration> {
    let lower = text.to_ascii_lowercase();
    for marker in ["retry-after", "retry after", "try again in"] {
        let mut rest = lower.as_str();
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let tail = rest.trim_start_matches([':', ' ', '"', '=']);
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            let Ok(amount) = digits.parse::<u64>() else {
                continue;
            };
            let unit = tail[digits.len()..].trim_start();
            let seconds =
                if unit.starts_with("min") || unit.starts_with('m') && !unit.starts_with("ms") {
                    amount.saturating_mul(60)
                } else if unit.starts_with("ms") {
                    amount / 1000
                } else {
                    amount
                };
            if (1..=3600).contains(&seconds) {
                return Some(std::time::Duration::from_secs(seconds));
            }
        }
    }
    None
}

/// Whether native output carries credential or login-challenge material, so that no part of it
/// may be quoted in a diagnostic. The rule lives in the one redaction implementation (ADR-0153).
pub use review_telemetry::redact::carries_auth_material;

fn auth_material(bytes: &[u8]) -> bool {
    carries_auth_material(bytes)
}

/// Claude Code's own words, lower-cased, when the bound model's safety classifier flagged a
/// request and no fallback model answered (ADR-0147).
pub const SAFEGUARDS_FLAGGED: &str = "safeguards flagged this message";

/// Whether `message` carries Claude Code's safeguards text, in any letter case.
pub fn says_safeguards_flagged(message: &str) -> bool {
    message.to_ascii_lowercase().contains(SAFEGUARDS_FLAGGED)
}

/// Recognize bounded native failure descriptions, not arbitrary successful model text. A
/// caller must establish failure from the protocol or process status before using this hint.
/// Network, quota and model errors cannot become refresh failures merely because a client
/// mentions its refresh operation around the underlying error.
pub fn classify_native_failure(message: &str) -> NativeFailureKind {
    use NativeFailureKind::*;
    let text = message.to_ascii_lowercase();
    let contains = |patterns: &[&str]| patterns.iter().any(|pattern| text.contains(pattern));
    let refresh = contains(&[
        "refresh token",
        "refresh_token",
        "refresh oauth",
        "credential refresh",
    ]);
    let credentials = refresh
        || contains(&[
            "credential",
            "access token",
            "access_token",
            "oauth token",
            "authentication token",
            "auth token",
            "api key",
            "api_key",
            "session token",
        ]);
    if contains(&["auth_refresh_contended"])
        || refresh
            && contains(&[
                "another",
                "concurrent",
                "already refreshing",
                "refresh in progress",
                "mid-refresh",
                "lock is held",
            ])
    {
        return AuthRefreshContended;
    }
    if contains(&["auth_revoked", "token_revoked", "credentials_revoked"])
        || credentials && contains(&["revoked"])
    {
        return AuthRevoked;
    }
    if contains(&["auth_expired", "token_expired", "expired_token"])
        || credentials && contains(&["expired", "has expired"])
    {
        return AuthExpired;
    }
    if contains(&[
        "auth_missing",
        "not logged in",
        "not authenticated",
        "no credentials",
        "missing credentials",
        "no api key",
        "missing authentication",
        "authentication required",
        "login required",
        "please run /login",
        "please log in",
        "please login",
    ]) || credentials && contains(&["not found", "missing", "not configured"])
    {
        return AuthMissing;
    }
    // Claude Code's own words when the bound model's safety classifier flagged a request and no
    // fallback model answered: "<Model>'s safeguards flagged this message (…/aup). This
    // sometimes happens with safe, normal conversations."
    if contains(&[SAFEGUARDS_FLAGGED]) {
        return ModelRefused;
    }
    if contains(&[
        "rate_limit",
        "rate limit",
        "insufficient_quota",
        "quota exceeded",
        "exceeded your quota",
        "usage limit",
        "credit balance",
        "too many requests",
        "http 429",
        "status 429",
    ]) {
        return Quota;
    }
    if contains(&[
        "at capacity",
        "over capacity",
        "capacity exceeded",
        "overloaded",
        "server_is_overloaded",
    ]) {
        return Capacity;
    }
    if contains(&[
        "model_not_found",
        "model unavailable",
        "model is unavailable",
        "model does not exist",
        "unsupported model",
        "unknown model",
        "model access denied",
        // Claude Code, for a model ID it does not know or the account cannot use: "There's an
        // issue with the selected model (claude-fable-5.1). It may not exist or you may not
        // have access to it. Run --model to pick a different model."
        "issue with the selected model",
        "may not exist or you may not have access",
    ]) {
        return ModelUnavailable;
    }
    if contains(&[
        "network",
        "connection refused",
        "connection reset",
        "connection timed out",
        "request timed out",
        "etimedout",
        "econnreset",
        "enotfound",
        "dns",
        "failed to connect",
        "unable to connect",
        "unable to resolve",
        "fetch failed",
        "tls",
        "certificate",
    ]) {
        return Network;
    }
    if contains(&["auth_refresh_failed"])
        || refresh && contains(&["failed", "failure", "unable", "cannot", "could not"])
    {
        return AuthRefreshFailed;
    }
    if contains(&[
        "auth_rejected",
        "authentication_error",
        "invalid_api_key",
        "invalid api key",
        "invalid_token",
        "invalid token",
        "invalid_grant",
        "authentication failed",
        "unauthorized",
        "unauthorised",
        "http 401",
        "status 401",
        "refresh_token_reused",
    ]) || credentials && contains(&["invalid", "rejected", "already used"])
    {
        return AuthRejected;
    }
    Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_network_and_capacity_failures_are_transient_and_a_named_wait_is_read() {
        for kind in NativeFailureKind::ALL {
            assert_eq!(
                kind.transient().is_some(),
                matches!(
                    kind,
                    NativeFailureKind::Network | NativeFailureKind::Capacity
                ),
                "{kind:?}"
            );
        }
        for (text, seconds) in [
            ("HTTP 529 overloaded; Retry-After: 30", Some(30)),
            ("retry-after 45", Some(45)),
            ("at capacity, retry after 20 seconds", Some(20)),
            (
                "Selected model is at capacity. Try again in 2 minutes.",
                Some(120),
            ),
            ("try again in 1500ms", Some(1)),
            ("try again later", None),
            ("Retry-After: 0", None),
            ("Retry-After: 86400", None),
        ] {
            assert_eq!(
                retry_after(text).map(|wait| wait.as_secs()),
                seconds,
                "{text}"
            );
        }
        let transient =
            Transient::new(TransientClass::Capacity).with_retry_after_in("Retry-After: 9");
        assert_eq!(
            transient.retry_after,
            Some(std::time::Duration::from_secs(9))
        );
    }

    #[test]
    fn every_whole_capture_replacement_is_recognized_and_nothing_else() {
        for kind in NativeFailureKind::ALL {
            let mut stdout = b"device code ABCD-EFGH at https://claude.ai/oauth/authorize".to_vec();
            let mut stderr = b"waiting".to_vec();
            kind.redact_capture(false, &mut stdout, &mut stderr);
            assert!(
                NativeFailureKind::replaced_capture(&stdout, &stderr),
                "{kind:?}"
            );
        }
        assert!(!NativeFailureKind::replaced_capture(
            b"{\"type\":\"result\"}\n",
            b""
        ));
        assert!(!NativeFailureKind::replaced_capture(
            NativeFailureKind::Quota.diagnostic().as_bytes(),
            b"stderr kept"
        ));
    }

    #[test]
    fn distinguishes_authentication_from_other_native_failures() {
        use NativeFailureKind::*;
        for (message, expected) in [
            ("Not logged in. Please run /login", AuthMissing),
            ("OAuth token has been revoked", AuthRevoked),
            ("Your access token has expired", AuthExpired),
            ("Your authentication token has expired", AuthExpired),
            ("No API key was provided", AuthMissing),
            ("Your API key is invalid", AuthRejected),
            ("refresh_token_reused", AuthRejected),
            (
                "Failed to refresh OAuth token: another Claude Code process is refreshing it or exited mid-refresh.",
                AuthRefreshContended,
            ),
            ("Failed to refresh OAuth token", AuthRefreshFailed),
            ("API Error: 401 authentication_error", AuthRejected),
            ("Unexpected status 401 Unauthorized", AuthRejected),
            ("Failed to refresh OAuth token: connection refused", Network),
            ("Failed to refresh OAuth token: HTTP 429 rate limit", Quota),
            ("model_not_found", ModelUnavailable),
            ("model access denied", ModelUnavailable),
            (
                "Selected model is at capacity. Please try a different model.",
                Capacity,
            ),
            ("API Error: 529 overloaded_error", Capacity),
            (
                "There's an issue with the selected model (claude-fable-5.1). It may not exist or you may not have access to it. Run --model to pick a different model.",
                ModelUnavailable,
            ),
            (
                "Claude Fable 5.1's safeguards flagged this message (https://www.anthropic.com/legal/aup). This sometimes happens with safe, normal conversations.",
                ModelRefused,
            ),
            ("HTTP 403 forbidden", Unknown),
            ("Worker failed with status 1", Unknown),
        ] {
            assert_eq!(classify_native_failure(message), expected, "{message}");
            assert_eq!(
                expected.is_auth(),
                matches!(
                    expected,
                    AuthMissing
                        | AuthRevoked
                        | AuthExpired
                        | AuthRefreshContended
                        | AuthRefreshFailed
                        | AuthRejected
                )
            );
        }
    }

    #[test]
    fn auth_diagnostics_and_captured_evidence_never_echo_native_secrets() {
        let message = "OAuth token revoked: access_token=FIXTURE_SECRET https://claude.ai/oauth/authorize?code=FIXTURE_CODE";
        let kind = classify_native_failure(message);
        let mut stdout = message.as_bytes().to_vec();
        let mut stderr = b"unrelated FIXTURE_STDERR_SECRET".to_vec();
        kind.redact_auth_capture(&mut stdout, &mut stderr);
        assert_eq!(stdout, kind.diagnostic().as_bytes());
        assert!(stderr.is_empty());
        assert!(!kind.diagnostic().contains("FIXTURE_"));
        assert!(!kind.diagnostic().contains("https://"));
    }

    #[test]
    fn challenge_redaction_does_not_reclassify_network_or_unknown_errors_as_auth() {
        for (message, expected) in [
            (
                "Connection refused during refresh: access_token=FIXTURE_SECRET",
                NativeFailureKind::Network,
            ),
            (
                "Failed: https://example.test/authorize?code=FIXTURE_CODE",
                NativeFailureKind::Unknown,
            ),
        ] {
            let kind = classify_native_failure(message);
            assert_eq!(kind, expected);
            assert!(!kind.is_auth());
            let mut stdout = message.as_bytes().to_vec();
            let mut stderr = b"FIXTURE_STDERR_SECRET".to_vec();
            kind.redact_auth_capture(&mut stdout, &mut stderr);
            assert_eq!(stdout, kind.diagnostic().as_bytes());
            assert!(stderr.is_empty());
        }
    }

    /// ADR-0202: a refusal that links the Provider's own documentation is kept as it was; the
    /// documentation hosts are never a reason to replace the capture. Their login and key pages
    /// still are, and so is a code in any URL.
    #[test]
    fn provider_documentation_urls_are_never_a_reason_to_redact() {
        for text in [
            "API Error: Claude Code is unable to respond to this request, which appears to violate our Usage Policy (https://www.anthropic.com/legal/aup). See https://platform.claude.com/docs/en/test-and-evaluate/strengthen-guardrails/handle-streaming-refusals",
            "see https://docs.claude.com/en/docs/claude-code/troubleshooting for help",
            "see https://docs.anthropic.com/en/api/errors",
            "429: see https://platform.openai.com/docs/guides/error-codes/api-errors",
            "HTTPS://PLATFORM.CLAUDE.COM/DOCS/EN/ABOUT-CLAUDE/MODELS",
        ] {
            for kind in [NativeFailureKind::ModelRefused, NativeFailureKind::Unknown] {
                let mut stdout = text.as_bytes().to_vec();
                let mut stderr = b"stderr kept".to_vec();
                kind.redact_capture(false, &mut stdout, &mut stderr);
                assert_eq!(stdout, text.as_bytes(), "{text}");
                assert_eq!(stderr, b"stderr kept", "{text}");
            }
        }
        for private in [
            "https://platform.claude.com/login FIXTURE_CODE",
            "https://platform.claude.com/oauth/authorize?client=x",
            "https://platform.claude.com/docs/en/start?code=FIXTURE_CODE",
            "https://claude.ai/docs FIXTURE",
        ] {
            assert!(carries_auth_material(private.as_bytes()), "{private}");
        }
    }

    #[test]
    fn device_challenges_and_official_auth_origins_are_private_on_any_failure() {
        for challenge in [
            "https://auth.openai.com/codex/device FIXTURE_CODE",
            "HTTPS://AUTH.OPENAI.COM/CODEX/DEVICE FIXTURE_CODE",
            "https://auth.openai.\ncom/codex/\r\ndevice FIXTURE_CODE",
            "USER CODE: FIXTURE_CODE",
            "https://claude.ai/login FIXTURE_CODE",
            "https://claude.com/login FIXTURE_CODE",
            "https://platform.claude.com/login FIXTURE_CODE",
            "https://console.anthropic.com/login FIXTURE_CODE",
            r#"{"userCode":"FIXTURE_CODE"}"#,
            r#"{"user_code":"FIXTURE_CODE"}"#,
            r#"{"deviceCode":"FIXTURE_CODE"}"#,
            r#"{"verificationUrl":"https://example.test/continue/FIXTURE_CODE"}"#,
            r#"{"verification_uri_complete":"https://example.test/continue/FIXTURE_CODE"}"#,
            r#"{"authUrl":"https://example.test/continue/FIXTURE_CODE"}"#,
            r#"{"authorization_url":"https://example.test/continue/FIXTURE_CODE"}"#,
            r#"{"accessToken":"FIXTURE_TOKEN"}"#,
            r#"{"refreshToken":"FIXTURE_TOKEN"}"#,
        ] {
            for (kind, detail) in [
                (NativeFailureKind::Network, "network failed"),
                (NativeFailureKind::Quota, "rate limit"),
                (NativeFailureKind::Unknown, "failed"),
            ] {
                for challenge_on_stdout in [true, false] {
                    let sensitive = format!("{detail}: {challenge}").into_bytes();
                    let mut stdout = if challenge_on_stdout {
                        sensitive.clone()
                    } else {
                        detail.as_bytes().to_vec()
                    };
                    let mut stderr = if challenge_on_stdout {
                        b"FIXTURE_STDERR".to_vec()
                    } else {
                        sensitive
                    };
                    kind.redact_auth_capture(&mut stdout, &mut stderr);
                    assert_eq!(stdout, kind.diagnostic().as_bytes(), "{challenge}");
                    assert!(stderr.is_empty(), "{challenge}");
                    assert!(!kind.is_auth());
                }
            }
        }
    }
}
