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
//!   would otherwise withhold every digest as a 32-hex run. An identity is exempt only when it
//!   is exactly one of the kernel's grammars ([`is_kernel_identity`]) in an identity position
//!   the record declares; [`is_canonical_decimal`] validates counters. A value that fails is
//!   untrusted text.
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
pub const REDACTION_VERSION: &str = "af-redact/2";

/// What af prints in place of a Provider-authored line that looks like credential material.
pub const WITHHELD_LINE: &str = "[line withheld: looks like credential material]";

/// What stands in place of a JSON value, or a whole JSON object, that the rules withheld.
pub const WITHHELD: &str = "[withheld]";

/// What stands in place of a line too long to be judged whole: it is never cut, only withheld.
pub const OVERSIZED_LINE: &str = "[line withheld: too long to judge whole]";

/// Name parts that make an assignment (`NAME=value`, `NAME: value`) or a JSON member name look
/// like a secret.
pub const SECRET_NAME_PARTS: [&str; 8] = [
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "credential",
    "auth",
];

/// Whole name segments, between `_`, `-`, `.` or the name's ends, that make a name look like a
/// secret although they are too short to be matched as parts (`--pw`, `DB_PW`).
const SECRET_NAME_SEGMENTS: [&str; 1] = ["pw"];

/// Name parts whose value may follow after whitespace alone (`password hunter2`,
/// `--password hunter2`, a `.netrc` line), not only after `=` or `:`.
const SPACED_SECRET_NAME_PARTS: [&str; 3] = ["password", "passwd", "passphrase"];

/// Words that follow a password name in prose (`password is incorrect`, `password for alice`)
/// rather than being its value.
const PROSE_AFTER_PASSWORD: [&str; 52] = [
    "a",
    "again",
    "an",
    "and",
    "are",
    "as",
    "at",
    "authentication",
    "be",
    "by",
    "can",
    "cannot",
    "change",
    "changed",
    "did",
    "do",
    "does",
    "expired",
    "failed",
    "field",
    "for",
    "from",
    "has",
    "have",
    "if",
    "in",
    "incorrect",
    "invalid",
    "is",
    "manager",
    "may",
    "must",
    "no",
    "not",
    "of",
    "on",
    "or",
    "please",
    "prompt",
    "required",
    "reset",
    "should",
    "the",
    "to",
    "too",
    "was",
    "when",
    "will",
    "with",
    "wrong",
    "you",
    "your",
];

/// Phrases that name the account a login holds (`Logged in as alice@example.com`): an account
/// identity is withheld like a credential.
const ACCOUNT_PHRASES: [&str; 3] = ["logged in as", "signed in as", "authenticated as"];

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
const AUTH_MARKERS: [&str; 42] = [
    "auth.openai.com",
    "claude.ai",
    "claude.com",
    "console.anthropic.com",
    "/codex/device",
    "/login/device",
    "one-timecode",
    "onetimecode",
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
    "#code=",
];

/// A hex run, or a base64 run mixing upper case, lower case and digits, at least this long looks
/// like a key or token.
const MIN_SECRET_RUN: usize = 32;

/// A hex run mixing letters and digits at least this long looks like a key or token.
const MIN_MIXED_HEX_RUN: usize = 20;

/// A run of lower-case letters and digits, holding both, at least this long looks like a key or
/// token.
const MIN_LOWER_RUN: usize = 40;

/// Two words joined by one space are judged as one when both are at least this long: a secret
/// wrapped or pasted with a space in it.
const MIN_JOINED_WORD: usize = 16;

/// The lengths of a git object id (SHA-1) and a SHA-256 digest: a word of exactly this many
/// lowercase hex digits is the id or digest `git` and `af` print, never a secret.
const OBJECT_ID_LENGTHS: [usize; 2] = [40, 64];

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
/// `"refresh_token": "…"`, `password: …`, any case and any prefix) or a password followed by a
/// value after whitespace (`--password hunter2`, `login alice password hunter2`), `mysql -u … -p…`,
/// a URL carrying a password or an `http(s)` user (`https://alice:hunter2@host`), `Bearer` and
/// a token, an `Authorization` header, a token prefix (`sk-`, `sk-ant-`, `sk-proj-`, `ghp_`,
/// `xoxb-`, …), an AWS access key id, a [`secret_run`], a JSON Web Token, a device code beside
/// the word `code`, the account a login holds, login-challenge material, or two long words joined
/// by one space that pass for one of these together. A run of letters alone is a word, an option
/// or a path segment (`--dangerously-bypass-approvals-…`, `/Users/alice/projects/af`), and a git
/// object id or a SHA-256 digest (`sha256:` and 64 hex) is an identity, never a secret; both pass.
pub fn looks_like_credential(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    secret_assignment(&lower)
        || mysql_password(&lower)
        || url_userinfo(&lower)
        || bearer_token(&lower)
        || authorization_header(&lower)
        || token_prefix(&lower)
        || aws_access_key(line)
        || secret_run(line)
        || json_web_token(line)
        || device_code(line, &lower)
        || ACCOUNT_PHRASES.iter().any(|phrase| lower.contains(phrase))
        || carries_auth_material(line.as_bytes())
        || joined_words(line)
}

/// Whether a JSON member name, or an assignment's name, matches the secret-name rule: it holds
/// `key`, `token`, `secret`, `password`, `passwd`, `passphrase`, `credential` or `auth`, or has
/// `pw` as a whole segment, in any case.
pub fn secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_NAME_PARTS.iter().any(|part| lower.contains(part))
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|segment| SECRET_NAME_SEGMENTS.contains(&segment))
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

/// After a password name: whitespace, then a value that is neither an option nor a word of prose.
fn spaced_value(rest: &str) -> bool {
    let value = rest.trim_start();
    if value.len() == rest.len() {
        return false;
    }
    let word = value.split_whitespace().next().unwrap_or_default();
    let bare = word.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '.' | ';' | ')'));
    !bare.is_empty()
        && !word.starts_with("--")
        && !matches!(word.chars().next(), Some('=' | ':'))
        && !PROSE_AFTER_PASSWORD.contains(&bare)
}

fn secret_assignment(lower: &str) -> bool {
    let mut rest = lower;
    while let Some(start) = rest.find(is_name_char) {
        let after_start = &rest[start..];
        let end = after_start
            .find(|c: char| !is_name_char(c))
            .unwrap_or(after_start.len());
        let (name, after) = after_start.split_at(end);
        if secret_name(name) && assigned_after(after) {
            return true;
        }
        let spaced = SPACED_SECRET_NAME_PARTS
            .iter()
            .any(|part| name.contains(part))
            || name
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|segment| SECRET_NAME_SEGMENTS.contains(&segment));
        if spaced && spaced_value(after) {
            return true;
        }
        rest = after;
    }
    false
}

/// A MySQL-style command line: a user option (`-u NAME`, `-uNAME`, `--user`) beside a password
/// written into its short option (`-phunter2`).
fn mysql_password(lower: &str) -> bool {
    let words: Vec<&str> = lower.split_whitespace().collect();
    let short = |word: &str, option: &str| word.starts_with(option) && !word.starts_with("--");
    words
        .iter()
        .any(|word| short(word, "-u") || *word == "--user" || word.starts_with("--user="))
        && words
            .iter()
            .any(|word| short(word, "-p") && word.len() > "-p".len())
}

/// A URL whose authority carries a password (`scheme://user:password@host`), or any user at all
/// in an `http` or `https` URL, where a user name is how a token travels.
fn url_userinfo(lower: &str) -> bool {
    lower.match_indices("://").any(|(at, separator)| {
        let scheme_start = lower[..at]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')))
            .map_or(0, |index| index + 1);
        let scheme = &lower[scheme_start..at];
        let rest = &lower[at + separator.len()..];
        let end = rest
            .find(|c: char| c.is_whitespace() || "/?#\"'<>`".contains(c))
            .unwrap_or(rest.len());
        let Some((userinfo, _host)) = rest[..end].rsplit_once('@') else {
            return false;
        };
        match userinfo.split_once(':') {
            Some((_, password)) => !password.is_empty(),
            None => !userinfo.is_empty() && matches!(scheme, "http" | "https"),
        }
    })
}

fn bearer_token(lower: &str) -> bool {
    lower.match_indices("bearer").any(|(at, word)| {
        let after = &lower[at + word.len()..];
        // `Bearer TOKEN`, and the header's name spelled as a label: `Bearer: TOKEN`.
        let token = after.strip_prefix([':', '=']).unwrap_or(after).trim_start();
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

/// An AWS access key id: `AKIA` (or `ASIA`, a temporary one) and 16 upper-case letters or
/// digits, as a whole word.
fn aws_access_key(line: &str) -> bool {
    ["AKIA", "ASIA"].iter().any(|prefix| {
        line.match_indices(prefix).any(|(at, _)| {
            let rest = &line[at + prefix.len()..];
            let run = rest
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit()))
                .unwrap_or(rest.len());
            starts_word(line, at)
                && run == 16
                && !rest[run..].starts_with(|c: char| c.is_ascii_alphanumeric())
        })
    })
}

/// Whether nothing alphanumeric precedes byte `at` of `line`.
fn starts_word(line: &str, at: usize) -> bool {
    line[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_ascii_alphanumeric())
}

/// A git object id or a SHA-256 digest: exactly one of the [`OBJECT_ID_LENGTHS`] in lowercase
/// hex digits.
fn object_id(run: &str) -> bool {
    OBJECT_ID_LENGTHS.contains(&run.len()) && is_lower_hex(run)
}

/// `line` with every git object id and SHA-256 digest that stands alone between characters that
/// are not letters or digits (`sha256:…`, `commit …`, `blobs/…/x`) blanked out, so that no
/// run rule judges an identity, or an identity joined to its neighbours, as a secret.
fn without_object_ids(line: &str) -> std::borrow::Cow<'_, str> {
    let mut masked: Option<String> = None;
    let mut start = 0;
    for (index, c) in line.char_indices().chain([(line.len(), ' ')]) {
        if c.is_ascii_alphanumeric() {
            continue;
        }
        if object_id(&line[start..index]) {
            masked
                .get_or_insert_with(|| line.to_string())
                .replace_range(start..index, &" ".repeat(index - start));
        }
        start = index + c.len_utf8();
    }
    masked.map_or(std::borrow::Cow::Borrowed(line), std::borrow::Cow::Owned)
}

/// A run of [`MIN_SECRET_RUN`] or more hexadecimal characters, or of [`MIN_MIXED_HEX_RUN`] or
/// more mixing letters and digits; a run of [`MIN_SECRET_RUN`] or more characters of the base64
/// and base64url alphabet (`A-Z`, `a-z`, `0-9`, `+`, `/`, `=`, `_`, `-`) holding at least one
/// upper-case letter, one lower-case letter and one digit; or a run of [`MIN_LOWER_RUN`] or more
/// lower-case letters and digits holding both. A git object id or a SHA-256 digest standing
/// alone is none of these.
fn secret_run(line: &str) -> bool {
    let line = without_object_ids(line);
    let mixed = |run: &str| {
        run.contains(|c: char| c.is_ascii_digit())
            && run.contains(|c: char| c.is_ascii_alphabetic())
    };
    let hex_run = line
        .split(|c: char| !c.is_ascii_hexdigit())
        .any(|run| run.len() >= MIN_SECRET_RUN || (run.len() >= MIN_MIXED_HEX_RUN && mixed(run)));
    let lower_run = || {
        line.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
            .any(|run| run.len() >= MIN_LOWER_RUN && mixed(run))
    };
    hex_run
        || lower_run()
        || line
            .split(|c: char| !is_base64_char(c))
            .filter(|run| run.len() >= MIN_SECRET_RUN)
            .any(|run| {
                run.contains(|c: char| c.is_ascii_uppercase())
                    && run.contains(|c: char| c.is_ascii_lowercase())
                    && run.contains(|c: char| c.is_ascii_digit())
            })
}

/// A device code (`ABCD-EFGH`, `WDJB-MJHT`: two groups of four to six upper-case letters or
/// digits, with at least one letter) on a line that says `code`.
fn device_code(line: &str, lower: &str) -> bool {
    let group = |part: &str| {
        (4..=6).contains(&part.len())
            && part
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    };
    lower.contains("code")
        && line
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .any(|word| {
                word.split_once('-').is_some_and(|(first, second)| {
                    group(first) && group(second) && word.bytes().any(|b| b.is_ascii_uppercase())
                })
            })
}

/// Two words joined by one space that pass for a credential together: a token prefix left
/// dangling before the rest of its token (`sk- ant-api03-…`), or two words of the base64
/// alphabet, neither a path, each at least [`MIN_JOINED_WORD`] long, that make one secret run,
/// token, access key id or JSON Web Token. A git object id or a digest is never joined.
fn joined_words(line: &str) -> bool {
    let words: Vec<&str> = line.split(' ').collect();
    words.windows(2).any(|pair| {
        let (first, second) = (pair[0], pair[1]);
        let lower = first.to_ascii_lowercase();
        let dangling = TOKEN_PREFIXES.iter().any(|prefix| {
            lower.strip_suffix(prefix).is_some_and(|before| {
                before
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_ascii_alphanumeric())
            })
        });
        if dangling && second.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return true;
        }
        let joinable = |word: &str| {
            word.len() >= MIN_JOINED_WORD
                && !word.starts_with(['/', '.', '~'])
                && word.chars().all(is_base64_char)
                && without_object_ids(word).trim().len() == word.len()
        };
        if !joinable(first) || !joinable(second) {
            return false;
        }
        let joined = format!("{first}{second}");
        token_prefix(&joined.to_ascii_lowercase())
            || aws_access_key(&joined)
            || secret_run(&joined)
            || json_web_token(&joined)
    })
}

fn is_base64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')
}

fn is_base64url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

/// A JSON Web Token: three non-empty runs of the base64url alphabet (`A-Z`, `a-z`, `0-9`, `_`,
/// `-`, and the standard alphabet's `+` and `/`) joined by single dots, the first starting with
/// `eyJ` (a base64url-encoded `{"`) and at least [`MIN_JWT_SEGMENT`] long, not preceded by a
/// base64url character; the signature may be short, but not empty.
fn json_web_token(line: &str) -> bool {
    // Every alphabet character is ASCII, so each run ends on a character boundary.
    let run = |from: &str| {
        from.find(|c: char| !(is_base64url_char(c) || matches!(c, '+' | '/')))
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
        header >= MIN_JWT_SEGMENT && payload > 0 && run(rest) > 0
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

/// The shape of an identity-like value: a digest, or 1 to 256 characters of
/// `A-Z a-z 0-9 . _ : @ / + -` starting with a letter or digit (run, Task, Attempt, node,
/// segment and span ids, package names as `name@version`). The shape alone earns no exemption
/// from the credential rules (`sk-proj-…` has it): only [`is_kernel_identity`] in an identity
/// position does.
pub fn is_identity(value: &str) -> bool {
    is_digest(value)
        || (!value.is_empty()
            && value.len() <= 256
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:@/+-".contains(&byte)))
}

/// The fixed lengths of the lowercase-hex ids the kernel and this crate produce: span ids and
/// `af.corr` (16), trace ids (32), Store ids and bare SHA-256 digests (64).
pub const KERNEL_HEX_LENGTHS: [usize; 3] = [16, 32, 64];

/// Whether `value` is exactly one of the kernel's identity grammars, the only values the typed
/// identity exemption keeps:
///
/// - a digest, `sha256:` and 64 lowercase hex digits, and a Task's run id, `task:` and a digest;
/// - a lowercase-hex id of one of the [`KERNEL_HEX_LENGTHS`];
/// - a segment (execution) id, `<started_unix_ms>-<pid>-<nonce>`: at most 20 and 10 decimal
///   digits and 8 lowercase hex digits;
/// - an occurrence id, `<segment_id>:<seq>`;
/// - a decimal ordinal: a canonical decimal of at most `u64`.
///
/// Anything else, `sk-proj-…` and every other identity-shaped string included, is untrusted text
/// and judged whole by the credential rules.
pub fn is_kernel_identity(value: &str) -> bool {
    is_digest(value)
        || value.strip_prefix("task:").is_some_and(is_digest)
        || (KERNEL_HEX_LENGTHS.contains(&value.len()) && is_lower_hex(value))
        || is_kernel_segment_id(value)
        || value
            .rsplit_once(':')
            .is_some_and(|(segment, seq)| is_kernel_segment_id(segment) && is_ordinal(seq))
        || is_ordinal(value)
}

/// A segment id as the writer makes one: `<started_unix_ms>-<pid>-<nonce>`, at most 20 and 10
/// decimal digits and exactly 8 lowercase hex digits.
fn is_kernel_segment_id(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    let digits = |part: &str, max: usize| {
        (1..=max).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    parts.len() == 3
        && digits(parts[0], 20)
        && digits(parts[1], 10)
        && parts[2].len() == 8
        && is_lower_hex(parts[2])
}

/// A canonical decimal of at most `u64`.
fn is_ordinal(value: &str) -> bool {
    is_canonical_decimal(value) && value.parse::<u64>().is_ok()
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
///
/// A grant is a path, and a path has more spellings than its bytes: each grant is also replaced
/// where it is written through the home prefix (`~/.claude-work`), as its canonical path (a
/// link resolved, `/var` as `/private/var`), JSON-escaped (`\/Users\/alice\/.claude-work`) or
/// percent-encoded (`%2FUsers%2Falice%2F.claude-work`), and where a path in the text names it
/// through `.`, `..` or repeated separators (`/Users/alice/./.claude-work`).
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    home: Option<String>,
    grants: Vec<(String, String)>,
    /// Every spelling of every grant with the grant's name, longest first.
    spellings: Vec<(String, String)>,
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
            self.respell();
        }
        self
    }

    /// Replace a Provider grant's value, in every spelling, by `<grant:NAME>`. A value shorter
    /// than three characters would rewrite ordinary text and is ignored.
    pub fn with_grant(mut self, name: &str, value: &str) -> Self {
        if value.len() >= 3 {
            self.grants.push((name.to_string(), value.to_string()));
            self.respell();
        }
        self
    }

    /// The paths a grant names: its value and, for an existing path, its canonical form.
    fn grant_paths(value: &str) -> Vec<String> {
        let mut paths = vec![value.to_string()];
        if value.starts_with('/')
            && let Ok(canonical) = std::fs::canonicalize(value)
            && let Some(canonical) = canonical.to_str()
            && canonical != value
        {
            paths.push(canonical.to_string());
        }
        paths
    }

    fn respell(&mut self) {
        let mut spellings: Vec<(String, String)> = Vec::new();
        for (name, value) in &self.grants {
            for path in Self::grant_paths(value) {
                let mut forms = vec![path.clone()];
                if let Some(home) = &self.home
                    && let Some(rest) = path.strip_prefix(home.as_str())
                    && rest.starts_with('/')
                {
                    forms.push(format!("~{rest}"));
                }
                for form in forms {
                    for spelling in [
                        json_escaped(&form),
                        json_escaped(&form).replace('/', "\\/"),
                        percent_encoded(&form, false),
                        percent_encoded(&form, true),
                        form,
                    ] {
                        if !spellings.iter().any(|(_, known)| *known == spelling) {
                            spellings.push((name.clone(), spelling));
                        }
                    }
                }
            }
        }
        // Longest first, so a grant inside another is never replaced inside it.
        spellings.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        self.spellings = spellings;
    }

    /// Grant and home replacements of a string already judged safe.
    pub fn replace(&self, text: &str) -> String {
        let mut text = text.to_string();
        for (name, spelling) in &self.spellings {
            if text.contains(spelling.as_str()) {
                text = text.replace(spelling.as_str(), &format!("<grant:{name}>"));
            }
        }
        if !self.grants.is_empty() {
            text = self.replace_unnormalized_grants(&text);
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

    /// Every absolute or home-relative path in `text` that is not written in its normal form
    /// (`/./`, `/../`, `//`) and normalizes to a grant, or to a path inside one, has the grant's
    /// part replaced by `<grant:NAME>`. Other text keeps its bytes.
    fn replace_unnormalized_grants(&self, text: &str) -> String {
        let path_char = |c: char| !(c.is_whitespace() || "\"'`<>()[]{},;|".contains(c));
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(['/', '~']) {
            let starts = rest[..at].chars().next_back().is_none_or(|c| !path_char(c));
            let end = rest[at..]
                .find(|c: char| !path_char(c))
                .map_or(rest.len(), |end| at + end);
            let path = &rest[at..end];
            let replaced = starts.then(|| self.grant_in_unnormalized(path)).flatten();
            out.push_str(&rest[..at]);
            out.push_str(replaced.as_deref().unwrap_or(path));
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }

    fn grant_in_unnormalized(&self, path: &str) -> Option<String> {
        let unnormal = path.contains("/./")
            || path.contains("/../")
            || path.contains("//")
            || path.ends_with("/.")
            || path.ends_with("/..");
        if !unnormal {
            return None;
        }
        let absolute = match path.strip_prefix('~') {
            Some(rest) if rest.starts_with('/') => format!("{}{rest}", self.home.as_deref()?),
            Some(_) => return None,
            None => path.to_string(),
        };
        let normal = normalize_path(&absolute);
        self.grants.iter().find_map(|(name, value)| {
            Self::grant_paths(value).into_iter().find_map(|grant| {
                let grant = normalize_path(&grant);
                let rest = normal.strip_prefix(grant.as_str())?;
                (rest.is_empty() || rest.starts_with('/')).then(|| format!("<grant:{name}>{rest}"))
            })
        })
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
    /// is exactly a kernel identity ([`is_kernel_identity`]), a canonical decimal, a number or
    /// a boolean; every other position, and every other value in those positions, is redacted
    /// as [`Self::json`] does.
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

/// `path` as JSON writes it inside a string, without the quotes.
fn json_escaped(path: &str) -> String {
    let quoted = serde_json::to_string(path).expect("a string serializes");
    quoted[1..quoted.len() - 1].to_string()
}

/// `path` with every byte outside the unreserved set (`A-Z a-z 0-9 - . _ ~`) percent-encoded,
/// in upper- or lower-case hex.
fn percent_encoded(path: &str, lower: bool) -> String {
    path.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else if lower {
                format!("%{byte:02x}")
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// An absolute path with `.` segments and repeated separators removed and each `..` taking its
/// parent away, as the kernel would resolve it lexically.
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
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

/// A value that validates as one of the typed kinds: a kernel identity, a canonical decimal
/// string, a number or a boolean.
fn typed_value(value: &Value) -> bool {
    match value {
        Value::String(text) => is_kernel_identity(text) || is_canonical_decimal(text),
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

/// Short options that take a password as their next argument (`mysql -p VALUE`).
const SECRET_SHORT_OPTIONS: [&str; 1] = ["-p"];

/// A command line's key/value pairs under the secret-name rule: the argument after an option
/// whose name matches it and carries no `=` (`--api-key VALUE`, `-token VALUE`, `--pw VALUE`),
/// or after `-p`, is that option's value and is withheld whole as [`WITHHELD`], whatever it looks
/// like. `--name=value` forms are assignments, and every other argument is a value, that the
/// value rules judge whole afterwards. Returns the arguments and how many were withheld.
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
        secret_option = SECRET_SHORT_OPTIONS.contains(&argument.as_str())
            || (argument.starts_with('-')
                && !name.is_empty()
                && !name.contains('=')
                && secret_name(name));
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
            "-p",
            "hunter2",
            "--pw",
            "hunter3",
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
                "-p",
                WITHHELD,
                "--pw",
                WITHHELD,
                "--auth"
            ]
        );
        assert_eq!(withheld, 4);
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
        // Outside an exempt position a digest is judged as text, and a digest is no secret.
        assert_eq!(document["note"], digest);
        assert_eq!(document["artifact_id_but_text"], WITHHELD);
        assert_eq!(withheld, 1);
    }

    /// Every string an independent audit (2026-10-20, M7) found passing the heuristic untouched.
    const AUDIT_MISSED: [&str; 17] = [
        "https://alice:hunter2@git.example.com/org/repo.git",
        "postgres://app:S3cretPw@db.internal:5432/prod",
        "machine api.example.com login alice password hunter2",
        "AKIAIOSFODNN7EXAMPLE",
        "Enter code ABCD-EFGH at https://github.com/login/device",
        "Your one-time code: ABCD-EFGH",
        "sk- ant-api03-Xy7kL9mN2pQ4rS6tU8vW0xY1zA3bC5dE",
        "Ab3dEf6hIj9lMn2pQr5t Uv8xYz1BcD4fGh7jKl0n",
        "mysql -u root -phunter2",
        "--password hunter2",
        "Bearer: ya29.a0AfH6SMBxFIXTURE",
        "session eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl+c2lnbmF0dXJl",
        "session eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln",
        "http://localhost:54545/callback#code=a1b2c3",
        "0123456789abcdef0123456789abcde",
        "q8z2m4k7x1c9v3b6n0p5l8j2h4g7f1d3s6a9w2e5",
        "Logged in as alice@example.com (org: Acme Corp)",
    ];

    /// The rules the audit's fix names, each shown alone.
    const RULES_ADDED: [&str; 6] = [
        "please enter the code WDJB-MJHT",
        "https://x-access-token@github.com/org/repo.git",
        "open https://example.com/login/device now",
        "db_pw: violet",
        "login passphrase violet",
        "ASIAIOSFODNN7EXAMPLE",
    ];

    /// What the same audit found caught, for the record: these stay caught.
    const AUDIT_CAUGHT: [&str; 11] = [
        "aws_secret wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz",
        "slack xoxb-1234-5678-abcdefFIXTURE",
        "https://example.com/callback?code=a1b2c3",
        "using bearer abcdef123",
        "Authorization: Basic dXNlcjpwYXNzd29yZA==",
        "PGPASSWORD=hunter2",
        "stripe sk_live_FIXTURE123",
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlc2lnbmF0dXJl",
        "fetch failed for 3f9a1c0b7e6d5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b",
    ];

    #[test]
    fn the_audit_strings_are_withheld_in_text_and_json() {
        for line in AUDIT_MISSED.iter().chain(&AUDIT_CAUGHT).chain(&RULES_ADDED) {
            assert!(looks_like_credential(line), "{line}");
            assert_eq!(bounded_cli_line(line), WITHHELD_LINE, "{line}");
            let (text, withheld) = redact_text(&format!("ok\n{line}\n"));
            assert_eq!(text, format!("ok\n{WITHHELD_LINE}\n"), "{line}");
            assert_eq!(withheld, 1, "{line}");
            // In a JSON document a value is judged whole whatever its member is named.
            let mut document = json!({"note": line, "list": [line]});
            assert_eq!(redact_json(&mut document), 2, "{line}");
            assert_eq!(
                document,
                json!({"note": WITHHELD, "list": [WITHHELD]}),
                "{line}"
            );
        }
        // The audit's JSON document: a URL with a password, and a password under a short name.
        let mut document =
            json!({"url": "https://alice:hunter2@git.example.com/x", "pw": "hunter2"});
        assert_eq!(redact_json(&mut document), 2);
        assert_eq!(document, json!({"url": WITHHELD, "pw": WITHHELD}));
    }

    /// The identities `git` and `af` print are no credentials: a SHA-256 digest (bare, as
    /// `sha256:…` or a Task run id), a git object id, and two of them side by side, in text and
    /// in JSON, outside any exempt position.
    #[test]
    fn digests_and_git_object_ids_are_not_withheld() {
        let digest = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let commit = "c79f03c0a1b2c3d4e5f60718293a4b5c6d7e8f90";
        assert_eq!(commit.len(), 40);
        for line in [
            format!("artifact sha256:{digest}"),
            format!("task task:sha256:{digest} accepted"),
            format!("sha256 {digest}"),
            format!("blob/{digest}/x"),
            format!("commit {commit}"),
            format!("af 0.12.0 (aarch64-apple-darwin, {commit})"),
            format!("{commit} {commit}"),
            format!("sha256:{digest} sha256:{digest}"),
            format!("/home/alice/state/af/telemetry/v1/blobs/9f/{digest}.gz"),
        ] {
            assert!(!looks_like_credential(&line), "{line}");
            assert_eq!(redact_text(&line), (line.clone(), 0), "{line}");
            let mut document = json!({ "note": line });
            assert_eq!(redact_json(&mut document), 0, "{line}");
        }
        // Any other length or case of hex stays a secret run.
        for line in [
            format!("{commit}a"),
            commit.to_uppercase(),
            digest[..63].to_string(),
            digest[..32].to_string(),
        ] {
            assert!(looks_like_credential(&line), "{line}");
        }
    }

    /// Ordinary Provider and af lines keep passing: prose about passwords and logins, a user name
    /// in an `ssh` URL, two paths, an ADR number beside `code`.
    #[test]
    fn ordinary_lines_still_pass() {
        for line in [
            "Your password is incorrect",
            "password authentication failed for user app",
            "Passwords do not match, please try again",
            "Enter your password:",
            "Not logged in. Please run /login",
            "Logged in using ChatGPT",
            "fatal: could not read from ssh://git@github.com/org/repo.git",
            "cp /Users/alice/Project12345678 /Users/alice/Project87654321",
            "see ADR-0112 for the exit code",
            "git diff -u",
            "codex-cli 0.121.0",
            "test redact::tests::the_audit_strings_are_withheld_in_text_and_json ... ok",
            "Error: Cannot find module '/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js'",
        ] {
            assert!(!looks_like_credential(line), "{line}");
        }
    }

    /// A grant path is replaced however it is spelled: through the home prefix, JSON-escaped,
    /// percent-encoded, or through `.`, `..` and repeated separators.
    #[test]
    fn every_spelling_of_a_grant_path_is_replaced() {
        let redactor = Redactor::new()
            .with_grant("CLAUDE_CONFIG_DIR", "/Users/alice/.claude-work")
            .with_home("/Users/alice");
        for (text, replaced) in [
            (
                "read /Users/alice/.claude-work/x",
                "read <grant:CLAUDE_CONFIG_DIR>/x",
            ),
            ("read ~/.claude-work/x", "read <grant:CLAUDE_CONFIG_DIR>/x"),
            (
                "{\"dir\":\"\\/Users\\/alice\\/.claude-work\\/x\"}",
                "{\"dir\":\"<grant:CLAUDE_CONFIG_DIR>\\/x\"}",
            ),
            (
                "file=%2FUsers%2Falice%2F.claude-work%2Fx",
                "file=<grant:CLAUDE_CONFIG_DIR>%2Fx",
            ),
            (
                "file=%2fUsers%2falice%2f.claude-work",
                "file=<grant:CLAUDE_CONFIG_DIR>",
            ),
            (
                "at /Users/alice/./.claude-work",
                "at <grant:CLAUDE_CONFIG_DIR>",
            ),
            (
                "at /Users/alice//.claude-work/x and ~/./.claude-work",
                "at <grant:CLAUDE_CONFIG_DIR>/x and <grant:CLAUDE_CONFIG_DIR>",
            ),
            (
                "'/Users/bob/../alice/.claude-work/y'",
                "'<grant:CLAUDE_CONFIG_DIR>/y'",
            ),
            // Other paths, normal or not, keep their bytes apart from the home prefix.
            ("at /Users/alice/./other", "at ~/./other"),
        ] {
            assert_eq!(redactor.replace(text), replaced, "{text}");
        }
        // A grant behind a link is also replaced by the path it resolves to.
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().canonicalize().unwrap().join("target");
        let link = directory.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let redactor = Redactor::new().with_grant("CODEX_HOME", link.to_str().unwrap());
        assert_eq!(
            redactor.replace(&format!("{}/auth.json", target.display())),
            "<grant:CODEX_HOME>/auth.json"
        );
    }
}
