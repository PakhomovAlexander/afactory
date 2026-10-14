//! What an installed Provider CLI must accept, and how its own words are quoted (ADR-0146).
//!
//! Each adapter crate declares the options its command builder emits as one [`CliInvocation`],
//! next to that builder, and a unit test there holds the two equal, so the list cannot drift from
//! the invocation. The af host judges an installed CLI by the options its `--help` declares, never
//! by a version string: a wrapper or a fork is judged by what it accepts, and a CLI that lacks one
//! is refused before any paid dispatch instead of being run with a weaker invocation.
//!
//! Provider-authored text (a help page, an error line, a stderr tail) reaches human output, probe
//! errors and Attempt diagnostics only through [`bounded_cli_line`] and [`stderr_excerpt`]:
//! terminal escapes and control characters removed, whitespace collapsed, every line and the whole
//! excerpt bounded, and every line that [`looks_like_credential`] replaced as a whole by the
//! af-authored [`WITHHELD_LINE`]. A stream carrying login-challenge material anywhere is withheld
//! as a whole.

use std::collections::BTreeSet;

/// What af prints in place of a Provider-authored line that looks like credential material.
pub const WITHHELD_LINE: &str = "[line withheld: looks like credential material]";

/// Name parts that make an assignment (`NAME=value`, `NAME: value`) look like a secret.
const SECRET_NAME_PARTS: [&str; 7] = [
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

/// A hex run, or a base64 run mixing upper case, lower case and digits, at least this long looks
/// like a key or token.
const MIN_SECRET_RUN: usize = 32;

/// The shortest header and signature a [`json_web_token`] has: shorter runs are ordinary text.
const MIN_JWT_SEGMENT: usize = 16;

/// One Provider-authored line is at most this many characters, plus an ellipsis.
pub const MAX_CLI_LINE_CHARS: usize = 240;
/// A stderr excerpt keeps at most this many lines from the end of the stream...
pub const MAX_STDERR_TAIL_LINES: usize = 6;
/// ...and at most this many characters in all.
pub const MAX_STDERR_TAIL_CHARS: usize = 1024;

/// Install commands a CLI's own error text may suggest; the sentence carrying one is the fix.
const FIX_MARKERS: [&str; 8] = [
    "reinstall",
    "npm install",
    "npm i ",
    "pnpm add",
    "yarn global add",
    "brew install",
    "brew reinstall",
    "brew upgrade",
];

/// What an argument parser prints when it refuses an option or argument it does not know:
/// commander (`error: unknown option '--safe-mode'`), clap (`error: unexpected argument '--stdio'
/// found`), argparse and getopt (`unrecognized arguments`, `unrecognised option`).
const INVOCATION_ERROR_MARKERS: [&str; 7] = [
    "unknown option",
    "unexpected argument",
    "unrecognized option",
    "unrecognized argument",
    "unrecognised option",
    "unrecognised argument",
    "unknown argument",
];

/// One option af passes: the spelling it emits, and the long name a help page lists it under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CliFlag {
    /// Exactly what the command builder writes into argv, short or long.
    pub spelling: &'static str,
    /// The long name, for a report; the same as `spelling` for a long option.
    pub long: &'static str,
}

impl CliFlag {
    /// A long option af emits as itself.
    pub const fn long(name: &'static str) -> Self {
        Self {
            spelling: name,
            long: name,
        }
    }

    /// A short option af emits, with the long name it abbreviates.
    pub const fn short(spelling: &'static str, long: &'static str) -> Self {
        Self { spelling, long }
    }

    /// `-p/--print`, or `--json` for a long option.
    pub fn display(&self) -> String {
        if self.spelling == self.long {
            self.long.to_string()
        } else {
            format!("{}/{}", self.spelling, self.long)
        }
    }
}

/// Everything af's invocation of one Provider CLI requires of it, owned by the adapter crate
/// that builds the command.
#[derive(Debug)]
pub struct CliInvocation {
    /// The program name the adapter drives.
    pub program: &'static str,
    /// Arguments that print the help page listing the options of the invoked (sub)command.
    pub help_args: &'static [&'static str],
    /// Every option the command builder can emit, each in the spelling it emits.
    pub flags: &'static [CliFlag],
    /// The Provider's own upgrade command for an installed CLI too old for `flags`.
    pub upgrade: &'static str,
}

impl CliInvocation {
    /// The required options a help page does not declare, in the order of [`Self::flags`].
    pub fn missing_flags(&self, help: &str) -> Vec<CliFlag> {
        let declared = declared_options(help);
        self.flags
            .iter()
            .filter(|flag| !declared.contains(flag.spelling))
            .copied()
            .collect()
    }

    /// The command that prints the help page, for a report: `codex exec --help`.
    pub fn help_command(&self) -> String {
        std::iter::once(self.program)
            .chain(self.help_args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether `argument` is one of the options this invocation declares.
    pub fn declares(&self, argument: &str) -> bool {
        self.flags.iter().any(|flag| flag.spelling == argument)
    }
}

/// The options a help page declares, short and long, as they would be written in argv.
///
/// A declaration is a line whose first non-blank character is `-`, indented no deeper than the
/// shallowest such line plus four columns (description text wraps deeper), made of one or more
/// comma-separated option names each optionally followed by a value placeholder (`<model>`,
/// `[value]`, `=<key=value>`), and then the end of the line or a gap of two or more spaces before
/// the description. A description that merely mentions `--print and …` is therefore not a
/// declaration, and neither is a wrapped continuation that begins with an option.
pub fn declared_options(help: &str) -> BTreeSet<String> {
    let indent = |line: &str| line.len() - line.trim_start_matches([' ', '\t']).len();
    let candidates: Vec<&str> = help
        .lines()
        .filter(|line| line.trim_start().starts_with('-'))
        .collect();
    let Some(shallowest) = candidates.iter().map(|line| indent(line)).min() else {
        return BTreeSet::new();
    };
    candidates
        .into_iter()
        .filter(|line| indent(line) <= shallowest + 4)
        .flat_map(|line| declaration(line.trim_start()))
        .collect()
}

fn declaration(line: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = line;
    loop {
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ',' | '=' | '[' | '<'))
            .unwrap_or(rest.len());
        let name = &rest[..end];
        if !is_option_name(name) {
            return Vec::new();
        }
        names.push(name.to_string());
        rest = skip_placeholders(&rest[end..]);
        match rest.strip_prefix(',') {
            Some(after) => rest = after.trim_start_matches(' '),
            None => break,
        }
    }
    let described = rest.is_empty()
        || rest.starts_with('\t')
        || rest.starts_with("  ")
        || rest.trim().is_empty();
    if described { names } else { Vec::new() }
}

/// Skip value placeholders after an option name: `=<x>`, `=x`, ` <x>`, ` [x]`, `[=x]`, `...`.
fn skip_placeholders(mut rest: &str) -> &str {
    loop {
        let trimmed = rest.strip_prefix(' ').unwrap_or(rest);
        if let Some(after) = rest.strip_prefix('=') {
            let end = after
                .find(|c: char| c.is_whitespace() || c == ',')
                .unwrap_or(after.len());
            rest = &after[end..];
        } else if let Some(close) = match trimmed.chars().next() {
            Some('<') => Some('>'),
            Some('[') => Some(']'),
            _ => None,
        } {
            let Some(end) = trimmed.find(close) else {
                return rest;
            };
            rest = trimmed[end + 1..].trim_start_matches('.');
        } else {
            return rest;
        }
    }
}

fn is_option_name(name: &str) -> bool {
    if let Some(long) = name.strip_prefix("--") {
        long.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
            && long
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    } else if let Some(short) = name.strip_prefix('-') {
        short.len() == 1 && short.chars().all(|c| c.is_ascii_alphanumeric())
    } else {
        false
    }
}

/// Whether a failed process's own words say it refused af's arguments: an unknown or unexpected
/// option or argument, or a usage block printed in place of any work.
pub fn is_invocation_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    INVOCATION_ERROR_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
        || lower
            .lines()
            .any(|line| line.trim_start().starts_with("usage:"))
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
        || crate::native_failure::carries_auth_material(line.as_bytes())
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

/// The CLI's own first error line — the first line starting with "error", else its first line,
/// stderr before stdout — and the first sentence suggesting an install or upgrade command. A line
/// that looks like credential material is [`WITHHELD_LINE`] and suggests no fix; when the output
/// carries credential or challenge material anywhere, the line is that marker and there is no fix.
pub fn error_and_fix(stderr: &[u8], stdout: &[u8]) -> (Option<String>, Option<String>) {
    if crate::native_failure::carries_auth_material(stderr)
        || crate::native_failure::carries_auth_material(stdout)
    {
        return (Some(WITHHELD_LINE.to_string()), None);
    }
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let lines: Vec<String> = stderr
        .lines()
        .chain(stdout.lines())
        .map(bounded_cli_line)
        .filter(|line| !line.is_empty())
        .collect();
    let error_line = lines
        .iter()
        .find(|line| line.to_ascii_lowercase().starts_with("error"))
        .or_else(|| lines.first())
        .cloned();
    let fix = lines.iter().find_map(|line| {
        line.split(". ")
            .map(|sentence| sentence.trim().trim_end_matches('.'))
            .find(|sentence| {
                let lower = sentence.to_ascii_lowercase();
                FIX_MARKERS.iter().any(|marker| lower.contains(marker))
            })
            .map(str::to_string)
    });
    (error_line, fix)
}

/// A bounded, sanitized excerpt of a failed process's stderr for an Attempt diagnostic: its
/// first error line when one precedes the tail, then its last [`MAX_STDERR_TAIL_LINES`] non-blank
/// lines, joined by ` | ` and cut at [`MAX_STDERR_TAIL_CHARS`]. Each line that looks like
/// credential material is [`WITHHELD_LINE`], and a stream carrying credential or challenge
/// material anywhere is that marker alone. `None` when there is nothing to show.
pub fn stderr_excerpt(stderr: &[u8]) -> Option<String> {
    if crate::native_failure::carries_auth_material(stderr) {
        return Some(WITHHELD_LINE.to_string());
    }
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<String> = text
        .lines()
        .map(bounded_cli_line)
        .filter(|line| !line.is_empty())
        .collect();
    let tail_start = lines.len().saturating_sub(MAX_STDERR_TAIL_LINES);
    let mut kept: Vec<&str> = Vec::new();
    if let Some(error) = lines[..tail_start]
        .iter()
        .find(|line| line.to_ascii_lowercase().starts_with("error"))
    {
        kept.push(error);
        kept.push("…");
    }
    kept.extend(lines[tail_start..].iter().map(String::as_str));
    if kept.is_empty() {
        return None;
    }
    let joined = kept.join(" | ");
    if joined.chars().count() <= MAX_STDERR_TAIL_CHARS {
        return Some(joined);
    }
    let mut bounded: String = joined.chars().take(MAX_STDERR_TAIL_CHARS).collect();
    bounded.push('…');
    Some(bounded)
}

/// How a finished process ended, in a report's words: `exited 2`, `was ended by signal 9`.
pub fn exit_description(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exited {code}"),
        (None, Some(signal)) => format!("was ended by signal {signal}"),
        (None, None) => "exited unsuccessfully".to_string(),
    }
}

/// How a Worker process's transport ended, for a diagnostic: `exited 1`, `was ended by signal
/// 9`, or why it never reached an exit (`reviewer did not answer within …`).
pub fn transport_outcome(status: &Result<std::process::ExitStatus, crate::RunnerError>) -> String {
    match status {
        Ok(status) => exit_description(status),
        Err(error) => error.to_string(),
    }
}

/// What a failed Worker's stderr says that a diagnostic may quote: its bounded, sanitized
/// [`stderr_excerpt`] and the CLI's own upgrade hint. Read before any privacy filter replaces the
/// captured stream, so a recognized failure keeps the Worker's own last words; every line went
/// through the credential filter, and when either stream carries challenge or credential material
/// the excerpt is the marker alone, as the privacy filter then replaces both streams.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerStderr {
    excerpt: Option<String>,
    fix: Option<String>,
}

impl WorkerStderr {
    pub fn read(stderr: &[u8], stdout: &[u8]) -> Self {
        if crate::native_failure::carries_auth_material(stdout) {
            return Self {
                excerpt: Some(WITHHELD_LINE.to_string()),
                fix: None,
            };
        }
        Self {
            excerpt: stderr_excerpt(stderr),
            fix: error_and_fix(stderr, b"").1,
        }
    }
}

fn ended(status: &Result<std::process::ExitStatus, crate::RunnerError>) -> String {
    match status {
        Ok(status) => exit_description(status),
        Err(error) => format!("ended ({error})"),
    }
}

/// How a Worker process that left no answer af can parse ended, with its own words: `exited 1
/// without a parsable result; stderr: …`, or `… and wrote no stderr`. Every diagnostic of such a
/// process carries it, whatever its failure is classified as.
pub fn ended_without_result(
    status: &Result<std::process::ExitStatus, crate::RunnerError>,
    stderr: &WorkerStderr,
) -> String {
    let outcome = ended(status);
    match &stderr.excerpt {
        Some(excerpt) => format!("{outcome} without a parsable result; stderr: {excerpt}"),
        None => format!("{outcome} without a parsable result and wrote no stderr"),
    }
}

/// The diagnostic of a Worker process that ended without an answer af can parse, for every
/// classification `kind` of its failure. `worker` names it (`Claude Worker`). Each carries the
/// exit and the bounded stderr excerpt. When its CLI refused af's own arguments the diagnostic
/// says so, with the fix: the CLI's own upgrade hint when it printed one, else the Provider's
/// upgrade command. A recognized failure leads with its closed diagnostic; an unclassified one
/// with the exit alone.
pub fn startup_failure(
    worker: &str,
    invocation: &CliInvocation,
    status: &Result<std::process::ExitStatus, crate::RunnerError>,
    stderr: &WorkerStderr,
    kind: crate::native_failure::NativeFailureKind,
) -> String {
    use crate::native_failure::NativeFailureKind;
    let outcome = ended(status);
    let said = ended_without_result(status, stderr);
    match kind {
        NativeFailureKind::Unknown => format!("{worker} {said}"),
        NativeFailureKind::UnsupportedInvocation => {
            let fix = stderr
                .fix
                .clone()
                .unwrap_or_else(|| format!("upgrade it with `{}`", invocation.upgrade));
            format!(
                "{worker} failed: the installed {} CLI does not accept af's invocation (unsupported_invocation); it {outcome}: {}; fix: {fix}, then rerun `af provider status`",
                invocation.program,
                stderr.excerpt.as_deref().unwrap_or("(no stderr)"),
            )
        }
        kind => format!("{worker} failed: {}; it {said}", kind.diagnostic()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_failure::NativeFailureKind;

    /// The shape Claude Code's commander help prints (abridged from 2.1.299 on 2026-10-14).
    const COMMANDER_HELP: &str = "\
Usage: claude [options] [command] [prompt]

Options:
  --allowedTools, --allowed-tools <tools...>
                                        Comma or space-separated list of tool
                                        names to allow (e.g. \"Bash(git:*)
                                        Edit\")
  --effort <level>                      Effort level for the current session
  --include-partial-messages            Include partial message chunks (only
                                        works with --print and
                                        --output-format=stream-json)
  -p, --print                           Print response and exit
  --safe-mode                           Start with all customizations disabled
";

    /// The shape codex-cli's clap help prints (abridged from 0.162.1 on 2026-10-14).
    const CLAP_HELP: &str = "\
Usage: codex exec [OPTIONS] [PROMPT]

Options:
  -c, --config <key=value>
          Override a configuration value. Use a dotted path (`foo.bar.baz`)
          --not-an-option mentioned in a description

      --strict-config
          Error out on unknown fields

  -s, --sandbox <SANDBOX_MODE>
          Select the sandbox policy

  -o, --output-last-message <FILE>
          Specifies file where the last message from the agent should be written
";

    #[test]
    fn declarations_are_read_from_the_option_column_only() {
        let declared = declared_options(COMMANDER_HELP);
        for option in [
            "--allowedTools",
            "--allowed-tools",
            "--effort",
            "--include-partial-messages",
            "-p",
            "--print",
            "--safe-mode",
        ] {
            assert!(declared.contains(option), "{option}: {declared:?}");
        }
        // Mentioned in a wrapped description, never declared.
        assert!(!declared.contains("--output-format"), "{declared:?}");
        assert_eq!(declared.len(), 7, "{declared:?}");

        let declared = declared_options(CLAP_HELP);
        for option in [
            "-c",
            "--config",
            "--strict-config",
            "-s",
            "--sandbox",
            "-o",
            "--output-last-message",
        ] {
            assert!(declared.contains(option), "{option}: {declared:?}");
        }
        assert!(!declared.contains("--not-an-option"), "{declared:?}");
        assert!(declared_options("no options here\n").is_empty());
    }

    #[test]
    fn placeholders_and_separators_of_common_parsers_are_understood() {
        for (line, expected) in [
            (
                "  --output-format=<format>  Output",
                vec!["--output-format"],
            ),
            ("  --color[=WHEN]  Colorize", vec!["--color"]),
            ("  -m, --model <MODEL>...", vec!["-m", "--model"]),
            ("  --json", vec!["--json"]),
            ("  --tools <tools...>\tList", vec!["--tools"]),
            ("  --print and --output-format", vec![]),
            ("  --x=", vec!["--x"]),
            ("  -- the end of options", vec![]),
        ] {
            let declared: Vec<String> = declared_options(line).into_iter().collect();
            let mut expected: Vec<String> = expected.into_iter().map(String::from).collect();
            expected.sort();
            assert_eq!(declared, expected, "{line}");
        }
    }

    #[test]
    fn missing_flags_are_judged_in_the_spelling_af_emits() {
        const INVOCATION: CliInvocation = CliInvocation {
            program: "claude",
            help_args: &["--help"],
            flags: &[
                CliFlag::short("-p", "--print"),
                CliFlag::long("--safe-mode"),
                CliFlag::long("--restricted"),
                CliFlag::long("--allowedTools"),
            ],
            upgrade: "npm install -g @anthropic-ai/claude-code@latest",
        };
        let invocation = &INVOCATION;
        let missing = invocation.missing_flags(COMMANDER_HELP);
        assert_eq!(missing, [CliFlag::long("--restricted")]);
        assert_eq!(invocation.help_command(), "claude --help");
        assert_eq!(CliFlag::short("-p", "--print").display(), "-p/--print");
        // A CLI that lists only `--print` does not accept the `-p` af writes.
        let long_only = "  --print  Print\n  --safe-mode\n  --restricted\n  --allowedTools <t>\n";
        assert_eq!(
            invocation.missing_flags(long_only),
            [CliFlag::short("-p", "--print")]
        );
    }

    #[test]
    fn argument_errors_of_common_parsers_are_recognized() {
        for text in [
            "error: unknown option '--safe-mode'",
            "error: unexpected argument '--stdio' found\n\nUsage: codex app-server [OPTIONS]",
            "codex: unrecognized arguments: --frobnicate",
            "Usage: claude [options] [command] [prompt]",
        ] {
            assert!(is_invocation_error(text), "{text}");
        }
        for text in [
            "Provider model at capacity",
            "error: request failed",
            "the usage limit was reached",
        ] {
            assert!(!is_invocation_error(text), "{text}");
        }
    }

    #[test]
    fn provider_text_is_bounded_stripped_and_withheld_when_it_carries_credentials() {
        let (line, fix) = error_and_fix(
            b"\x1b[31mwarning\x1b[0m: something\nerror: broken install\x07\nRun `npm i -g @openai/codex` to repair.\n",
            b"",
        );
        assert_eq!(line.as_deref(), Some("error: broken install"));
        assert_eq!(
            fix.as_deref(),
            Some("Run `npm i -g @openai/codex` to repair")
        );
        let (line, fix) = error_and_fix(b"", b"panic: nothing useful here\n");
        assert_eq!(line.as_deref(), Some("panic: nothing useful here"));
        assert_eq!(fix, None);
        // Not a hex run: that would be withheld whole, never bounded.
        let long = "x".repeat(1000);
        assert_eq!(
            bounded_cli_line(&long).chars().count(),
            MAX_CLI_LINE_CHARS + 1
        );
        assert_eq!(
            bounded_cli_line("\x1b[1;31mError:\x1b[0m\tgone"),
            "Error: gone"
        );
        assert_eq!(
            error_and_fix(b"error: refresh_token=FIXTURE_SECRET\n", b""),
            (Some(WITHHELD_LINE.to_string()), None)
        );
        assert_eq!(
            stderr_excerpt(b"access_token=FIXTURE_SECRET\n").as_deref(),
            Some(WITHHELD_LINE)
        );
    }

    #[test]
    fn a_stderr_excerpt_keeps_the_error_line_and_a_bounded_tail() {
        assert_eq!(
            stderr_excerpt(b"error: unknown option '--safe-mode'\n").as_deref(),
            Some("error: unknown option '--safe-mode'")
        );
        let mut noisy = b"error: unexpected argument '--stdio' found\n".to_vec();
        for line in 0..20 {
            noisy.extend(format!("line {line}\x1b[0m\n").as_bytes());
        }
        let excerpt = stderr_excerpt(&noisy).unwrap();
        assert_eq!(
            excerpt,
            "error: unexpected argument '--stdio' found | … | line 14 | line 15 | line 16 | line 17 | line 18 | line 19"
        );
        const INVOCATION: CliInvocation = CliInvocation {
            program: "claude",
            help_args: &["--help"],
            flags: &[CliFlag::long("--safe-mode")],
            upgrade: "npm install -g @anthropic-ai/claude-code@latest",
        };
        let invocation = &INVOCATION;
        use std::os::unix::process::ExitStatusExt;
        let exited_1 = Ok(std::process::ExitStatus::from_raw(256));
        assert_eq!(
            startup_failure(
                "Claude Worker",
                invocation,
                &exited_1,
                &WorkerStderr::read(b"error: unknown option '--safe-mode'\n", b""),
                NativeFailureKind::UnsupportedInvocation,
            ),
            "Claude Worker failed: the installed claude CLI does not accept af's invocation (unsupported_invocation); it exited 1: error: unknown option '--safe-mode'; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`"
        );
        assert_eq!(
            startup_failure(
                "Claude Worker",
                invocation,
                &exited_1,
                &WorkerStderr::read(b"", b""),
                NativeFailureKind::Unknown
            ),
            "Claude Worker exited 1 without a parsable result and wrote no stderr"
        );
        let flood = format!("{}\n", "x".repeat(10_000)).repeat(10);
        let excerpt = stderr_excerpt(flood.as_bytes()).unwrap();
        assert!(excerpt.chars().count() <= MAX_STDERR_TAIL_CHARS + 1);
        assert_eq!(stderr_excerpt(b"\n \n"), None);
    }

    /// Lines shaped like credential material in every spelling issue #259's review named, each
    /// with a secret marker that must never be printed.
    const CREDENTIAL_LINES: [&str; 4] = [
        "error: unknown option '--safe-mode'; API_KEY=sk-secret",
        "Authorization: Bearer abcFIXTUREdefFIXTUREghi",
        "fetch failed for 3f9a1c0b7e6d5a4f3e2d1c0b9a8f7e6d5c4b3a2f",
        "config: OPENAI_API_KEY=\"sk-proj-FIXTURE\"",
    ];

    #[test]
    fn every_credential_spelling_is_withheld_as_a_whole_line() {
        for line in CREDENTIAL_LINES.into_iter().chain([
            "anthropic_api_key: sk-ant-FIXTURE",
            "ANTHROPIC_API_KEY=FIXTURE",
            "{\"refresh_token\": \"FIXTURE\"}",
            "db password = FIXTURE",
            "client_secret:'FIXTURE'",
            "x-auth: FIXTURE",
            "CREDENTIALS=FIXTURE",
            "using bearer FIXTURE",
            "authorization=FIXTURE",
            "token ghp_FIXTURE from the environment",
            "slack xoxb-FIXTURE",
            "key aGVsbG8gd29ybGQgdGhpcyBpcyBhIGZpeHR1cmU=",
            "visit https://auth.openai.com/codex/device",
        ]) {
            assert!(looks_like_credential(line), "{line}");
            assert_eq!(bounded_cli_line(line), WITHHELD_LINE, "{line}");
        }
    }

    #[test]
    fn ordinary_cli_lines_pass_through_the_credential_filter() {
        for line in [
            "error: unexpected argument '--stdio' found",
            "error: unknown option '--safe-mode'",
            "error: unexpected argument '--dangerously-bypass-approvals-and-sandbox' found",
            "Usage: codex app-server [OPTIONS] [COMMAND]",
            "error: connection refused while loading the native runtime",
            "network failed: proxy refused connection",
            "Not logged in. Please run /login",
            "Your API key is invalid",
            "Error: Cannot find module '/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js'",
            "codex-cli 0.121.0",
            "risk-free task-id disk-usage",
        ] {
            assert!(!looks_like_credential(line), "{line}");
            assert_eq!(bounded_cli_line(line), line, "{line}");
        }
    }

    /// Issue #259, round 3: a run is a secret by its shape — 32 or more hex characters, 32 or
    /// more base64 characters mixing upper case, lower case and digits, or a JSON Web Token —
    /// and a run of letters alone, a word or a path, is not.
    #[test]
    fn runs_are_withheld_by_their_shape_and_letters_alone_pass() {
        let mixed_base64 = "Ab3dEf6hIj9lMn2pQr5tUv8xYz1BcD4fGh7jKl0nOp-";
        assert_eq!(mixed_base64.len(), 43);
        let digest = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        assert_eq!(digest.len(), 64);
        for line in [
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlc2lnbmF0dXJl",
            "session eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlc2lnbmF0dXJl. expired",
            "token=\"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlc2lnbmF0dXJl\"",
            &format!("fetch failed: {mixed_base64}"),
            &format!("sha256 {digest}"),
            &format!("blob/{digest}/x"),
            "upload id ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abc",
        ] {
            assert!(looks_like_credential(line), "{line}");
            assert_eq!(bounded_cli_line(line), WITHHELD_LINE, "{line}");
        }
        for line in [
            &"g".repeat(40),
            "mnpqrstuvwxyzmnpqrstuvwxyzmnpqrstuvwxyz",
            "/Users/alice/bs/apkhmv-main/af/afactory-wt-cli-compat/crates/af",
            "error: unexpected argument '--stdio' found",
            "ThisIsAVeryLongCamelCaseIdentifierWithoutDigits",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJ",
            "error: eyJ is not a token",
            // Issue #259, round 4: a JSON Web Token is three whole base64url segments, the header
            // and the signature 16 or more characters long; anything less is ordinary text.
            "error: eyJabc.def. ordinary failure",
            "error: eyJabc.def.ghi",
            "session eyJhbGciOiJub25lIn0.eyJzdWIiOiIxIn0. expired",
            "error: eyJhbGciOiJIUzI1NiJ9..c2lnbmF0dXJlc2lnbmF0dXJl",
            "error: xeyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
        ] {
            assert!(!looks_like_credential(line), "{line}");
            assert_eq!(bounded_cli_line(line), *line, "{line}");
        }
    }

    /// The privacy filter replaces both streams when either carries challenge material, so the
    /// excerpt of an innocent-looking stderr beside such a stdout is the marker alone.
    #[test]
    fn challenge_material_on_stdout_withholds_the_stderr_excerpt() {
        use std::os::unix::process::ExitStatusExt;
        let said = WorkerStderr::read(
            b"FIXTURE_STDERR\n",
            b"{\"type\":\"turn.failed\",\"error\":{\"message\":\"visit https://auth.openai.com/codex/device\"}}",
        );
        let exited_1 = Ok(std::process::ExitStatus::from_raw(256));
        assert_eq!(
            ended_without_result(&exited_1, &said),
            format!("exited 1 without a parsable result; stderr: {WITHHELD_LINE}")
        );
        assert_eq!(
            ended_without_result(&exited_1, &WorkerStderr::read(b"FIXTURE_STDERR\n", b"{}")),
            "exited 1 without a parsable result; stderr: FIXTURE_STDERR"
        );
    }

    #[test]
    fn a_withheld_line_keeps_the_rest_of_the_excerpt_and_the_fix() {
        for line in CREDENTIAL_LINES {
            let stderr = format!("{line}\nnext line\n");
            let excerpt = stderr_excerpt(stderr.as_bytes()).unwrap();
            // A header the stream-level challenge filter also knows withholds the whole stream.
            if crate::native_failure::carries_auth_material(stderr.as_bytes()) {
                assert_eq!(excerpt, WITHHELD_LINE, "{line}");
            } else {
                assert_eq!(excerpt, format!("{WITHHELD_LINE} | next line"));
            }
            assert!(
                !excerpt.contains("FIXTURE") && !excerpt.contains("sk-"),
                "{excerpt}"
            );
            let (error, _) = error_and_fix(stderr.as_bytes(), b"");
            assert!(
                error
                    .as_deref()
                    .is_some_and(|error| error == WITHHELD_LINE || error == "next line"),
                "{error:?}"
            );
        }
        let (error, fix) = error_and_fix(
            b"error: install broken; API_KEY=sk-secret\nRun `npm i -g @openai/codex` to repair.\n",
            b"",
        );
        assert_eq!(error.as_deref(), Some(WITHHELD_LINE));
        assert_eq!(
            fix.as_deref(),
            Some("Run `npm i -g @openai/codex` to repair")
        );
    }

    #[test]
    fn every_classification_carries_the_exit_and_the_excerpt() {
        use std::os::unix::process::ExitStatusExt;
        const INVOCATION: CliInvocation = CliInvocation {
            program: "codex",
            help_args: &["exec", "--help"],
            flags: &[CliFlag::long("--json")],
            upgrade: "npm install -g @openai/codex@latest",
        };
        let exited_1 = Ok(std::process::ExitStatus::from_raw(256));
        let said = WorkerStderr::read(b"network failed: proxy refused connection\n", b"");
        for kind in [
            NativeFailureKind::Network,
            NativeFailureKind::AuthMissing,
            NativeFailureKind::AuthRevoked,
            NativeFailureKind::AuthExpired,
            NativeFailureKind::AuthRefreshContended,
            NativeFailureKind::AuthRefreshFailed,
            NativeFailureKind::AuthRejected,
            NativeFailureKind::Quota,
            NativeFailureKind::Capacity,
            NativeFailureKind::ModelUnavailable,
            NativeFailureKind::Unknown,
            NativeFailureKind::UnsupportedInvocation,
        ] {
            let message = startup_failure("Codex Worker", &INVOCATION, &exited_1, &said, kind);
            assert!(message.contains("exited 1"), "{message}");
            assert!(
                message.contains("network failed: proxy refused connection"),
                "{message}"
            );
            if kind != NativeFailureKind::Unknown {
                let closed = kind.diagnostic();
                let category = &closed[closed.rfind('(').unwrap()..];
                assert!(message.contains(category), "{message}");
            }
        }
        assert_eq!(
            startup_failure(
                "Codex Worker",
                &INVOCATION,
                &exited_1,
                &said,
                NativeFailureKind::Network
            ),
            "Codex Worker failed: Provider network request failed (network); it exited 1 without a parsable result; stderr: network failed: proxy refused connection"
        );
        let secret = WorkerStderr::read(
            b"error: unknown option '--safe-mode'; API_KEY=sk-secret\n",
            b"",
        );
        let message = startup_failure(
            "Codex Worker",
            &INVOCATION,
            &exited_1,
            &secret,
            NativeFailureKind::UnsupportedInvocation,
        );
        assert!(message.contains("(unsupported_invocation)"), "{message}");
        assert!(
            message.contains(&format!("it exited 1: {WITHHELD_LINE}")),
            "{message}"
        );
        assert!(!message.contains("sk-secret"), "{message}");
    }
}
