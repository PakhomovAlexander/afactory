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
    ModelUnavailable,
    Network,
    Unknown,
}

impl NativeFailureKind {
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

    /// The closed Task recovery class of an authentication failure. Quota, model, network
    /// and unknown failures have none: they never suspend a Task for login (ADR-0141).
    pub fn task_auth_failure(self) -> Option<review_core::task::auth_recovery::TaskAuthFailureV1> {
        use review_core::task::auth_recovery::TaskAuthFailureV1 as Auth;
        match self {
            Self::AuthMissing => Some(Auth::AuthMissing),
            Self::AuthRevoked => Some(Auth::AuthRevoked),
            Self::AuthExpired => Some(Auth::AuthExpired),
            Self::AuthRefreshContended => Some(Auth::AuthRefreshContended),
            Self::AuthRefreshFailed => Some(Auth::AuthRefreshFailed),
            Self::AuthRejected => Some(Auth::AuthRejected),
            Self::Quota | Self::ModelUnavailable | Self::Network | Self::Unknown => None,
        }
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
            Self::ModelUnavailable => "Provider model is unavailable (model_unavailable)",
            Self::Network => "Provider network request failed (network)",
            Self::Unknown => "Provider returned an unclassified failure (unknown)",
        }
    }

    /// Auth output is not ordinary evidence. Retain the closed classification, while callers
    /// keep parsed usage separately before replacing both streams ahead of CAS capture. Also
    /// suppress an obvious auth challenge/token on a non-auth failure (for example, a failed
    /// network request during refresh), without changing that failure's classification.
    pub fn redact_auth_capture(self, stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) {
        if self.is_auth() || auth_material(stdout) || auth_material(stderr) {
            *stdout = self.diagnostic().as_bytes().to_vec();
            // One summary, not two identical CAS IDs: Attempt raw-evidence references are
            // required to be distinct. Neither original stream survives the replacement.
            stderr.clear();
        }
    }
}

fn auth_material(bytes: &[u8]) -> bool {
    // This is a conservative privacy filter, not endpoint authorization. On a failed
    // invocation even a query-free official device URL can travel beside a one-time code.
    // Ignore case and terminal whitespace/wrapping; the capture is replaced as a whole,
    // never reconstructed into a URL or treated as a usable challenge.
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .map(|character| character.to_ascii_lowercase())
        .collect();
    [
        "auth.openai.com",
        "claude.ai",
        "claude.com",
        "console.anthropic.com",
        "/codex/device",
        "usercode",
        "user_code",
        "user-code",
        "devicecode",
        "device_code",
        "device-code",
        "verificationurl",
        "verification_url",
        "verificationuri",
        "verification_uri",
        "authurl",
        "auth_url",
        "authorizationurl",
        "authorization_url",
        "authorizationuri",
        "authorization_uri",
        "accesstoken",
        "access_token",
        "refreshtoken",
        "refresh_token",
        "idtoken",
        "id_token",
        "codeverifier",
        "code_verifier",
        "codechallenge",
        "code_challenge",
        "authorization:bearer",
        "token=",
        "/oauth/authorize",
        "/oauth/authorization",
        "/authorize?",
        "?code=",
        "&code=",
    ]
    .iter()
    .any(|marker| text.contains(marker))
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
        "model_not_found",
        "model unavailable",
        "model is unavailable",
        "model does not exist",
        "unsupported model",
        "unknown model",
        "model access denied",
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
