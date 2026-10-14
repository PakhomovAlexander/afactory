//! What an installed Provider CLI must accept, and how its own words are quoted (ADR-0146).
//!
//! Each adapter crate declares the options its command builder emits as one [`CliInvocation`],
//! next to that builder, and a unit test there holds the two equal, so the list cannot drift from
//! the invocation. The af host judges an installed CLI by the options its `--help` declares, never
//! by a version string: a wrapper or a fork is judged by what it accepts, and a CLI that lacks one
//! is refused before any paid dispatch instead of being run with a weaker invocation.
//!
//! Provider-authored text (a help page, an error line, a stderr tail) reaches human output and
//! Attempt diagnostics only through [`bounded_cli_line`] and [`stderr_excerpt`]: terminal escapes
//! and control characters removed, whitespace collapsed, every line and the whole excerpt bounded,
//! and nothing at all when the text carries credential or challenge material.

use std::collections::BTreeSet;

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

/// One line of Provider text made safe to print: terminal escapes and control characters
/// removed, whitespace collapsed, and the length bounded.
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
    if collapsed.chars().count() <= MAX_CLI_LINE_CHARS {
        return collapsed;
    }
    let mut bounded: String = collapsed.chars().take(MAX_CLI_LINE_CHARS).collect();
    bounded.push('…');
    bounded
}

/// The CLI's own first error line — the first line starting with "error", else its first line,
/// stderr before stdout — and the first sentence suggesting an install or upgrade command. Both
/// are withheld when the output carries credential or challenge material.
pub fn error_and_fix(stderr: &[u8], stdout: &[u8]) -> (Option<String>, Option<String>) {
    if crate::native_failure::carries_auth_material(stderr)
        || crate::native_failure::carries_auth_material(stdout)
    {
        return (None, None);
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
/// lines, joined by ` | ` and cut at [`MAX_STDERR_TAIL_CHARS`]. `None` when there is nothing to
/// show, or when the stream carries credential or challenge material.
pub fn stderr_excerpt(stderr: &[u8]) -> Option<String> {
    if crate::native_failure::carries_auth_material(stderr) {
        return None;
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

/// The diagnostic of a Worker process that ended without an answer af can parse. `worker` names
/// it (`Claude Worker`). When its CLI refused af's own arguments the diagnostic says so, with the
/// exit, the bounded stderr excerpt and the fix: the CLI's own upgrade hint when it printed one,
/// else the Provider's upgrade command. Otherwise it carries the exit and the excerpt alone.
pub fn startup_failure(
    worker: &str,
    invocation: &CliInvocation,
    status: &Result<std::process::ExitStatus, crate::RunnerError>,
    stderr: &[u8],
    unsupported: bool,
) -> String {
    let outcome = match status {
        Ok(status) => exit_description(status),
        Err(error) => format!("ended ({error})"),
    };
    let excerpt = stderr_excerpt(stderr);
    if !unsupported {
        return match excerpt {
            Some(excerpt) => {
                format!("{worker} {outcome} without a parsable result; stderr: {excerpt}")
            }
            None => format!("{worker} {outcome} without a parsable result and wrote no stderr"),
        };
    }
    let fix = error_and_fix(stderr, b"")
        .1
        .unwrap_or_else(|| format!("upgrade it with `{}`", invocation.upgrade));
    format!(
        "{worker} failed: the installed {} CLI does not accept af's invocation (unsupported_invocation); it {outcome}: {}; fix: {fix}, then rerun `af provider status`",
        invocation.program,
        excerpt.as_deref().unwrap_or("(no stderr)"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let long = "e".repeat(1000);
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
            (None, None)
        );
        assert_eq!(stderr_excerpt(b"access_token=FIXTURE_SECRET\n"), None);
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
                b"error: unknown option '--safe-mode'\n",
                true,
            ),
            "Claude Worker failed: the installed claude CLI does not accept af's invocation (unsupported_invocation); it exited 1: error: unknown option '--safe-mode'; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`"
        );
        assert_eq!(
            startup_failure("Claude Worker", invocation, &exited_1, b"", false),
            "Claude Worker exited 1 without a parsable result and wrote no stderr"
        );
        let flood = format!("{}\n", "x".repeat(10_000)).repeat(10);
        let excerpt = stderr_excerpt(flood.as_bytes()).unwrap();
        assert!(excerpt.chars().count() <= MAX_STDERR_TAIL_CHARS + 1);
        assert_eq!(stderr_excerpt(b"\n \n"), None);
    }
}
