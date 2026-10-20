//! The one redaction implementation (ADR-0153 clause 6): the credential rules review-runner
//! applied to Provider-authored text, moved here unchanged and re-exported there, and the
//! structured rules Telemetry adds on top of them.
//!
//! Provider-authored text reaches human output only through [`bounded_cli_line`]: terminal
//! escapes and control characters removed, whitespace collapsed, the line judged whole and
//! replaced by the af-authored [`WITHHELD_LINE`] when it [`looks_like_credential`], and only then
//! bounded. Telemetry adds three things:
//!
//! - **Typed fields.** A digest, an id, a counter, a timestamp or an enumerated value the kernel
//!   produced and validated is written as a typed field and never passes the heuristic, which
//!   would otherwise withhold every digest as a 32-hex run. [`is_digest`], [`is_identity`] and
//!   [`is_canonical_decimal`] are the validations; a value that fails one is untrusted text.
//! - **Text, per line.** [`Redactor::text`] replaces every line that looks like a credential by
//!   [`WITHHELD_LINE`], Provider grant values by `<grant:NAME>` and the home prefix by `~`.
//! - **JSON, key-aware and recursive.** [`Redactor::json`] keeps a document parseable: a member
//!   whose name matches the secret-name rule has its value replaced by [`WITHHELD`] whatever it
//!   is; an object holding a member whose *name* passes for a credential (a token prefix, a JSON
//!   Web Token, a long secret run) becomes the string [`WITHHELD`] as a whole, because a name
//!   cannot be rewritten into one an untrusted document could not also carry; every remaining
//!   string and number is judged whole by the value rules.
//!
//! Every rule change bumps [`REDACTION_VERSION`], so a reader can tell which rules a blob passed.

use serde_json::Value;

/// The version of these rules a blob records (`af/TelemetryBlob@1` `redaction`).
pub const REDACTION_VERSION: &str = "af-redact/1";

/// What af prints in place of a Provider-authored line that looks like credential material.
pub const WITHHELD_LINE: &str = "[line withheld: looks like credential material]";

/// What stands in place of a JSON value, or a whole JSON object, that the rules withheld.
pub const WITHHELD: &str = "[withheld]";

/// What stands in place of a line too long to be judged whole: it is never cut, only withheld.
pub const OVERSIZED_LINE: &str = "[line withheld: too long to judge whole]";

/// Name parts that make an assignment (`NAME=value`, `NAME: value`) or a JSON member name look
/// like a secret.
pub const SECRET_NAME_PARTS: [&str; 7] = [
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "auth",
];

/// Prefixes Providers and forges give their tokens, matched at the start of a word.
const TOKEN_PREFIXES: [&str; 14] = [
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "xoxs-",
    "glpat-",
];

/// Markers of credential or login-challenge material anywhere in a stream, after whitespace is
/// removed and letters are lower-cased.
const AUTH_MARKERS: [&str; 38] = [
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
];

/// A hex run, or a base64 run mixing upper case, lower case and digits, at least this long looks
/// like a key or token.
const MIN_SECRET_RUN: usize = 32;

/// The shortest header and signature a [`json_web_token`] has: shorter runs are ordinary text.
const MIN_JWT_SEGMENT: usize = 16;

/// One Provider-authored line is at most this many characters, plus an ellipsis.
pub const MAX_CLI_LINE_CHARS: usize = 240;

/// Whether native output carries credential or login-challenge material, so that no part of it
/// may be quoted in a diagnostic.
pub fn carries_auth_material(bytes: &[u8]) -> bool {
    // This is a conservative privacy filter, not endpoint authorization. On a failed
    // invocation even a query-free official device URL can travel beside a one-time code.
    // Ignore case and terminal whitespace/wrapping; the capture is replaced as a whole,
    // never reconstructed into a URL or treated as a usable challenge.
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .map(|character| character.to_ascii_lowercase())
        .collect();
    AUTH_MARKERS.iter().any(|marker| text.contains(marker))
}

/// Whether one line of Provider text looks like credential material, so that none of it may be
/// printed: a key, token, secret, password, credential or auth assignment (`API_KEY=…`,
/// `"refresh_token": "…"`, `password: …`, any case and any prefix), `Bearer` and a token, an
/// `Authorization` header, a token prefix (`sk-`, `sk-ant-`, `sk-proj-`, `ghp_`, `xoxb-`, …), a
/// [`secret_run`], a JSON Web Token, or login-challenge material. A run of letters alone is a
/// word, an option or a path segment (`--dangerously-bypass-approvals-…`,
/// `/Users/alice/projects/af`), never a secret, and passes.
pub fn looks_like_credential(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    secret_assignment(&lower)
        || bearer_token(&lower)
        || authorization_header(&lower)
        || token_prefix(&lower)
        || secret_run(line)
        || json_web_token(line)
        || carries_auth_material(line.as_bytes())
}

/// Whether a JSON member name, or an assignment's name, matches the secret-name rule: it holds
/// `key`, `token`, `secret`, `password`, `passwd`, `credential` or `auth`, in any case.
pub fn secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_NAME_PARTS.iter().any(|part| lower.contains(part))
}

/// Whether a JSON member *name* itself passes for a credential under the value rules: a token
/// prefix, a JSON Web Token or a long secret run.
pub fn name_is_credential(name: &str) -> bool {
    token_prefix(&name.to_ascii_lowercase()) || secret_run(name) || json_web_token(name)
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// After a name: an optional closing quote, `=` or `:`, an optional opening quote, and a value.
fn assigned_after(rest: &str) -> bool {
    let rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest).trim_start();
    let Some(value) = rest.strip_prefix(['=', ':']) else {
        return false;
    };
    let value = value.trim_start();
    let value = value.strip_prefix(['"', '\'']).unwrap_or(value);
    value
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '=' | ':'))
}

fn secret_assignment(lower: &str) -> bool {
    let mut rest = lower;
    while let Some(start) = rest.find(is_name_char) {
        let after_start = &rest[start..];
        let end = after_start
            .find(|c: char| !is_name_char(c))
            .unwrap_or(after_start.len());
        let (name, after) = after_start.split_at(end);
        if SECRET_NAME_PARTS.iter().any(|part| name.contains(part)) && assigned_after(after) {
            return true;
        }
        rest = after;
    }
    false
}

fn bearer_token(lower: &str) -> bool {
    lower.match_indices("bearer").any(|(at, word)| {
        let after = &lower[at + word.len()..];
        let token = after.trim_start();
        token.len() < after.len()
            && token
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || "-._~+/".contains(c))
    })
}

fn authorization_header(lower: &str) -> bool {
    lower.match_indices("authorization").any(|(at, word)| {
        let rest = &lower[at + word.len()..];
        let rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest).trim_start();
        rest.starts_with([':', '='])
    })
}

fn token_prefix(lower: &str) -> bool {
    TOKEN_PREFIXES.iter().any(|prefix| {
        lower.match_indices(prefix).any(|(at, _)| {
            let starts_word = lower[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_ascii_alphanumeric());
            starts_word
                && lower[at + prefix.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
        })
    })
}

/// A run of [`MIN_SECRET_RUN`] or more hexadecimal characters, or of [`MIN_SECRET_RUN`] or more
/// characters of the base64 and base64url alphabet (`A-Z`, `a-z`, `0-9`, `+`, `/`, `=`, `_`, `-`)
/// holding at least one upper-case letter, one lower-case letter and one digit.
fn secret_run(line: &str) -> bool {
    let hex_run = line
        .split(|c: char| !c.is_ascii_hexdigit())
        .any(|run| run.len() >= MIN_SECRET_RUN);
    hex_run
        || line
            .split(|c: char| !is_base64_char(c))
            .filter(|run| run.len() >= MIN_SECRET_RUN)
            .any(|run| {
                run.contains(|c: char| c.is_ascii_uppercase())
                    && run.contains(|c: char| c.is_ascii_lowercase())
                    && run.contains(|c: char| c.is_ascii_digit())
            })
}

fn is_base64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')
}

fn is_base64url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

/// A JSON Web Token: exactly three non-empty runs of the base64url alphabet (`A-Z`, `a-z`, `0-9`,
/// `_`, `-`) joined by single dots, the first starting with `eyJ` (a base64url-encoded `{"`) and
/// at least [`MIN_JWT_SEGMENT`] long, the third, its signature, at least [`MIN_JWT_SEGMENT`] long,
/// and the whole bounded by a character outside the alphabet or the line's ends.
fn json_web_token(line: &str) -> bool {
    // Every base64url character is ASCII, so each run ends on a character boundary.
    let run = |from: &str| {
        from.find(|c: char| !is_base64url_char(c))
            .unwrap_or(from.len())
    };
    line.match_indices("eyJ").any(|(at, _)| {
        if line[..at]
            .chars()
            .next_back()
            .is_some_and(is_base64url_char)
        {
            return false;
        }
        let rest = &line[at..];
        let header = run(rest);
        let Some(rest) = rest[header..].strip_prefix('.') else {
            return false;
        };
        let payload = run(rest);
        let Some(rest) = rest[payload..].strip_prefix('.') else {
            return false;
        };
        header >= MIN_JWT_SEGMENT && payload > 0 && run(rest) >= MIN_JWT_SEGMENT
    })
}

/// One line of Provider text made safe to print: terminal escapes and control characters
/// removed, whitespace collapsed, the length bounded, and the whole line replaced by
/// [`WITHHELD_LINE`] when it [`looks_like_credential`].
pub fn bounded_cli_line(line: &str) -> String {
    let mut plain = String::with_capacity(line.len().min(MAX_CLI_LINE_CHARS * 4));
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // A CSI sequence ends at its first final byte; any other escape drops one character.
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else if c.is_control() {
            plain.push(' ');
        } else {
            plain.push(c);
        }
    }
    let collapsed = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    // Judged whole, before the bound could cut a secret into an innocent-looking prefix.
    if looks_like_credential(&collapsed) {
        return WITHHELD_LINE.to_string();
    }
    if collapsed.chars().count() <= MAX_CLI_LINE_CHARS {
        return collapsed;
    }
    let mut bounded: String = collapsed.chars().take(MAX_CLI_LINE_CHARS).collect();
    bounded.push('…');
    bounded
}

// ------------------------------------------------------------------------------------------
// typed fields

/// A digest as af writes one: `sha256:` and 64 lowercase hex digits.
pub fn is_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && is_lower_hex(hex))
}

/// Lowercase hexadecimal digits only, at least one.
pub fn is_lower_hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A kernel identity written as a typed field: a digest, or 1 to 256 characters of
/// `A-Z a-z 0-9 . _ : @ / + -` starting with a letter or digit (run, Task, Attempt, node,
/// segment and span ids, package names as `name@version`).
pub fn is_identity(value: &str) -> bool {
    is_digest(value)
        || (!value.is_empty()
            && value.len() <= 256
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:@/+-".contains(&byte)))
}

/// A canonical unsigned decimal of at most `u128`: `0`, or digits without a leading zero
/// (ADR-0078's `DecimalU128`; timestamps in segments use the same spelling).
pub fn is_canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
        && value.parse::<u128>().is_ok()
}

// ------------------------------------------------------------------------------------------
// structured redaction

/// The structured rules with this machine's replacements: Provider grant values become
/// `<grant:NAME>` and the home directory prefix becomes `~`.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    home: Option<String>,
    grants: Vec<(String, String)>,
    /// Why the home prefix is unknown, when the environment did not give a usable one.
    home_unknown: Option<String>,
}

impl Redactor {
    /// The rules without replacements.
    pub fn new() -> Self {
        Self::default()
    }

    /// The rules with the current process's `HOME` as the home prefix.
    pub fn for_this_process() -> Self {
        Self::with_home_variable(std::env::var_os("HOME").as_deref())
    }

    /// The rules with `home` (the value of `HOME`) as the home prefix. A value that gives no
    /// usable prefix (unset, not UTF-8, not absolute, or `/`) is never silently dropped: the
    /// redactor says so through [`Self::home_unknown`], and the writer records it as one
    /// `redaction` problem.
    pub fn with_home_variable(home: Option<&std::ffi::OsStr>) -> Self {
        let unknown = |why: &str| Self {
            home_unknown: Some(format!(
                "the home directory is unknown ({why}): home paths are not replaced by `~`"
            )),
            ..Self::default()
        };
        match home.map(std::ffi::OsStr::to_str) {
            None => unknown("HOME is not set"),
            Some(None) => unknown("HOME is not UTF-8"),
            Some(Some(home)) if !home.starts_with('/') => unknown("HOME is not absolute"),
            Some(Some(home)) if home.trim_end_matches('/').is_empty() => unknown("HOME is `/`"),
            Some(Some(home)) => Self::new().with_home(home),
        }
    }

    /// Why the home prefix is unknown, when [`Self::with_home_variable`] got no usable one.
    pub fn home_unknown(&self) -> Option<&str> {
        self.home_unknown.as_deref()
    }

    /// Replace `home` (and everything under it) by `~`.
    pub fn with_home(mut self, home: &str) -> Self {
        let home = home.trim_end_matches('/');
        if home.len() > 1 {
            self.home = Some(home.to_string());
            self.home_unknown = None;
        }
        self
    }

    /// Replace a Provider grant's value by `<grant:NAME>`. A value shorter than three
    /// characters would rewrite ordinary text and is ignored.
    pub fn with_grant(mut self, name: &str, value: &str) -> Self {
        if value.len() >= 3 {
            self.grants.push((name.to_string(), value.to_string()));
            // Longest first, so a grant inside another is never replaced inside it.
            self.grants
                .sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        }
        self
    }

    /// Grant and home replacements of a string already judged safe.
    pub fn replace(&self, text: &str) -> String {
        let mut text = text.to_string();
        for (name, value) in &self.grants {
            if text.contains(value.as_str()) {
                text = text.replace(value.as_str(), &format!("<grant:{name}>"));
            }
        }
        if let Some(home) = &self.home {
            let mut out = String::with_capacity(text.len());
            let mut rest = text.as_str();
            while let Some(at) = rest.find(home.as_str()) {
                let after = &rest[at + home.len()..];
                let bounded = after.is_empty() || after.starts_with('/');
                let starts = rest[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')));
                out.push_str(&rest[..at]);
                out.push_str(if bounded && starts { "~" } else { home });
                rest = after;
            }
            out.push_str(rest);
            text = out;
        }
        text
    }

    /// One untrusted string value judged whole: [`WITHHELD`] when it looks like a credential,
    /// else the value with its replacements. The flag says whether it was withheld.
    pub fn value(&self, value: &str) -> (String, bool) {
        if looks_like_credential(value) {
            (WITHHELD.to_string(), true)
        } else {
            (self.replace(value), false)
        }
    }

    /// Untrusted text, per line: each line that looks like a credential becomes
    /// [`WITHHELD_LINE`], the others keep their bytes apart from the replacements; line ends are
    /// kept. Returns the text and how many lines were withheld.
    pub fn text(&self, text: &str) -> (String, u32) {
        let mut out = String::with_capacity(text.len());
        let mut withheld = 0;
        for line in text.split_inclusive('\n') {
            let (body, end) = split_line_end(line);
            if looks_like_credential(body) {
                out.push_str(WITHHELD_LINE);
                withheld += 1;
            } else {
                out.push_str(&self.replace(body));
            }
            out.push_str(end);
        }
        (out, withheld)
    }

    /// A JSON document from any source, redacted recursively and key-aware; nothing is exempt.
    /// Returns how many values (or whole objects) were withheld. The document stays valid JSON.
    pub fn json(&self, value: &mut Value) -> u32 {
        self.json_at(value, "", &|_: &str| false)
    }

    /// A JSON document af itself produced: the positions `exempt` names (JSON pointers, `*`
    /// matching any member or index) are typed fields and skip the heuristic when their value
    /// validates as an identity, a digest, a decimal, a number or a boolean; every other
    /// position is redacted as [`Self::json`] does.
    pub fn kernel_json(&self, value: &mut Value, exempt: &[&str]) -> u32 {
        self.json_at(value, "", &|pointer: &str| {
            exempt
                .iter()
                .any(|pattern| pointer_matches(pattern, pointer))
        })
    }

    fn json_at(&self, value: &mut Value, pointer: &str, exempt: &dyn Fn(&str) -> bool) -> u32 {
        if exempt(pointer) && typed_value(value) {
            return 0;
        }
        match value {
            Value::Object(map) => {
                if map.keys().any(|name| name_is_credential(name)) {
                    *value = Value::String(WITHHELD.to_string());
                    return 1;
                }
                let mut withheld = 0;
                for (name, member) in map.iter_mut() {
                    let child = format!("{pointer}/{}", escape_pointer(name));
                    if secret_name(name) && !member.is_null() {
                        if exempt(&child) && typed_value(member) {
                            continue;
                        }
                        *member = Value::String(WITHHELD.to_string());
                        withheld += 1;
                    } else {
                        withheld += self.json_at(member, &child, exempt);
                    }
                }
                withheld
            }
            Value::Array(items) => items
                .iter_mut()
                .enumerate()
                .map(|(index, item)| self.json_at(item, &format!("{pointer}/{index}"), exempt))
                .sum(),
            Value::String(text) => {
                let (redacted, withheld) = self.value(text);
                *text = redacted;
                u32::from(withheld)
            }
            Value::Number(number) => {
                if looks_like_credential(&number.to_string()) {
                    *value = Value::String(WITHHELD.to_string());
                    1
                } else {
                    0
                }
            }
            Value::Bool(_) | Value::Null => 0,
        }
    }
}

/// A line's body and its terminator (`\n` or `\r\n`, or nothing at the end of the text).
fn split_line_end(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, &line[body.len()..])
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, &line[body.len()..])
    } else {
        (line, "")
    }
}

/// A value that validates as one of the typed kinds: a digest or identity string, a canonical
/// decimal string, a number or a boolean.
fn typed_value(value: &Value) -> bool {
    match value {
        Value::String(text) => is_identity(text) || is_canonical_decimal(text),
        Value::Number(_) | Value::Bool(_) | Value::Null => true,
        Value::Array(_) | Value::Object(_) => false,
    }
}

fn escape_pointer(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

fn pointer_matches(pattern: &str, pointer: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let pointer: Vec<&str> = pointer.split('/').collect();
    pattern.len() == pointer.len()
        && pattern
            .iter()
            .zip(&pointer)
            .all(|(want, have)| *want == "*" || want == have)
}

/// A command line's key/value pairs under the secret-name rule: the argument after an option
/// whose name matches it and carries no `=` (`--api-key VALUE`, `-token VALUE`) is that option's
/// value and is withheld whole as [`WITHHELD`], whatever it looks like. `--name=value` forms are
/// assignments the value rules already judge. Returns the arguments and how many were withheld.
pub fn withhold_option_values(argv: &[String]) -> (Vec<String>, u32) {
    let mut out = Vec::with_capacity(argv.len());
    let mut withheld = 0;
    let mut secret_option = false;
    for argument in argv {
        if secret_option {
            out.push(WITHHELD.to_string());
            withheld += 1;
            secret_option = false;
            continue;
        }
        let name = argument.trim_start_matches('-');
        secret_option = argument.starts_with('-')
            && !name.is_empty()
            && !name.contains('=')
            && secret_name(name);
        out.push(argument.clone());
    }
    (out, withheld)
}

/// [`Redactor::text`] without replacements.
pub fn redact_text(text: &str) -> (String, u32) {
    Redactor::new().text(text)
}

/// [`Redactor::json`] without replacements.
pub fn redact_json(value: &mut Value) -> u32 {
    Redactor::new().json(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn option_values_under_secret_names_are_withheld() {
        let argv: Vec<String> = [
            "af",
            "--api-key",
            "violet",
            "-token",
            "t0k",
            "--model",
            "opus",
            "--password=hunter2",
            "--auth",
        ]
        .map(str::to_string)
        .to_vec();
        let (kept, withheld) = withhold_option_values(&argv);
        assert_eq!(
            kept,
            [
                "af",
                "--api-key",
                WITHHELD,
                "-token",
                WITHHELD,
                "--model",
                "opus",
                "--password=hunter2",
                "--auth"
            ]
        );
        assert_eq!(withheld, 2);
        assert!(looks_like_credential("--password=hunter2"));
    }

    #[test]
    fn typed_field_validations_accept_only_their_shape() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        assert!(is_digest(&digest));
        assert!(!is_digest(&format!("sha256:{}", "AB".repeat(32))));
        assert!(!is_digest("sha256:abc"));
        assert!(is_identity(&digest));
        assert!(is_identity("af-run-1"));
        assert!(is_identity("correctness@1.2.0"));
        assert!(!is_identity(""));
        assert!(!is_identity("has space"));
        assert!(!is_identity("-leading"));
        assert!(is_canonical_decimal("0"));
        assert!(is_canonical_decimal(
            "340282366920938463463374607431768211455"
        ));
        assert!(!is_canonical_decimal(
            "340282366920938463463374607431768211456"
        ));
        assert!(!is_canonical_decimal("01"));
        assert!(!is_canonical_decimal("-1"));
        assert!(!is_canonical_decimal("1.5"));
    }

    #[test]
    fn text_is_judged_per_line_and_keeps_line_ends() {
        let (text, withheld) = redact_text("ok line\r\nAPI_KEY=hunter2\nlast");
        assert_eq!(text, format!("ok line\r\n{WITHHELD_LINE}\nlast"));
        assert_eq!(withheld, 1);
    }

    #[test]
    fn grants_and_the_home_prefix_are_replaced() {
        let redactor = Redactor::new()
            .with_home("/Users/alice")
            .with_grant("CODEX_HOME", "/private/var/af/codex-home");
        assert_eq!(
            redactor.replace("read /Users/alice/x and /private/var/af/codex-home/auth"),
            "read ~/x and <grant:CODEX_HOME>/auth"
        );
        // Another user whose name begins the same is not this home.
        assert_eq!(redactor.replace("/Users/alicette/x"), "/Users/alicette/x");
    }

    #[test]
    fn json_members_named_like_secrets_lose_their_values_whatever_they_are() {
        let mut document = json!({
            "password": "violet",
            "nested": {"api_key": 12345, "auth": {"user": "u"}},
            "plain": "words",
            "empty_token": null
        });
        let withheld = redact_json(&mut document);
        assert_eq!(
            document,
            json!({
                "password": WITHHELD,
                "nested": {"api_key": WITHHELD, "auth": WITHHELD},
                "plain": "words",
                "empty_token": null
            })
        );
        assert_eq!(withheld, 3);
    }

    #[test]
    fn kernel_json_exempts_only_the_named_positions_with_typed_values() {
        let digest = format!("sha256:{}", "cd".repeat(32));
        let mut document = json!({
            "artifact_id": digest,
            "note": digest,
            "refs": [digest, digest],
            "artifact_id_but_text": "has a space and sk-live-123"
        });
        let withheld = Redactor::new().kernel_json(
            &mut document,
            &["/artifact_id", "/refs/*", "/artifact_id_but_text"],
        );
        assert_eq!(document["artifact_id"], digest);
        assert_eq!(document["refs"], json!([digest, digest]));
        assert_eq!(document["note"], WITHHELD);
        assert_eq!(document["artifact_id_but_text"], WITHHELD);
        assert_eq!(withheld, 2);
    }
}
