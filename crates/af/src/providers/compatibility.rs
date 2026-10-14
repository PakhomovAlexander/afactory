//! Whether an installed Provider CLI accepts af's invocation (ADR-0146).
//!
//! Authentication success never implies invocation compatibility: a Claude Code 2.1.101 that `af
//! provider status` called authenticated failed its first Worker on `error: unknown option
//! '--safe-mode'`, which af reported only as a bare exit status (issue #259). This module asks
//! the resolved executable, token-free, for the help page of the (sub)command af invokes and
//! compares the options it declares with the adapter crate's required list
//! ([`review_runner_claude::CLAUDE_INVOCATION`], [`review_runner_codex::CODEX_INVOCATION`]), the
//! single source of truth that the adapters' command builders are tested against. The decision is
//! made on the help page, never on a version string, so a wrapper or a fork is judged by what it
//! accepts; `--version` runs only to report the detected version once a CLI is refused.
//!
//! `af provider status`, `af provider setup` and Task Provider admission all call
//! [`check_invocation`] before anything else asks the CLI about a login, so an incompatible CLI is
//! reported as one — with its Provider, kind, path, version, every missing option and the
//! Provider's upgrade command — never as a login or account problem, and before any Worker or
//! admission Attempt starts. No isolation flag is ever dropped to make an older CLI run: the
//! refusal is the only outcome. For Codex, the app-server protocol the identity and usage probes
//! need is proven by their own `initialize` round trip, and an app-server that exits before
//! answering it is reported here as the missing protocol capability ([`protocol_incompatibility`]).
//!
//! The help page, the version and the CLI's error line are Provider-authored text: they reach
//! human output, stderr and Attempt diagnostics bounded and stripped of control characters, never
//! a versioned document, which carries only the closed `incompatible` state and the af-authored
//! [`CliIncompatibility::summary`].

use super::installation::{BoundedRun, StartFailure, VersionCheck, run_bounded, version_check};
use super::*;
use review_runner::cli::{CliInvocation, bounded_cli_line, error_and_fix, exit_description};

/// The protocol capability the Codex identity and usage probes need of the installed CLI.
pub(super) const APP_SERVER_PROTOCOL: &str =
    "`codex app-server` answering JSON-RPC `initialize` over its default stdio transport";

/// What af's invocation of this Provider kind requires of its installed CLI.
pub(super) fn invocation(kind: ProviderKind) -> &'static CliInvocation {
    match kind {
        ProviderKind::Claude => &review_runner_claude::CLAUDE_INVOCATION,
        ProviderKind::Codex => &review_runner_codex::CODEX_INVOCATION,
    }
}

/// A Provider whose installed CLI does not accept af's invocation.
pub(crate) struct CliIncompatibility {
    provider: String,
    kind: ProviderKind,
    program: String,
    lacks: Box<Lacks>,
}

/// What the CLI lacks, what it said, and how to fix it.
struct Lacks {
    /// The first line of the CLI's own `--version`, bounded; `None` when it printed none af may
    /// quote or could not be asked.
    version: Option<String>,
    /// af-authored names of every option or protocol capability the CLI lacks.
    missing: Vec<String>,
    /// What the CLI itself said when it refused, bounded: Provider-authored.
    observed: Option<String>,
    /// The CLI's own upgrade hint when it printed one, else the Provider's upgrade command.
    fix: String,
}

impl CliIncompatibility {
    /// af-authored: the Provider, kind, program path and what it lacks, never the CLI's words.
    pub(crate) fn summary(&self) -> String {
        format!(
            "provider {}: incompatible Provider CLI: the {} CLI `{}` does not accept af's invocation; missing: {}",
            self.provider,
            self.kind.name(),
            self.program,
            self.lacks.missing.join(", ")
        )
    }

    /// The whole report: the summary with the detected version, what the CLI said, and the fix.
    pub(crate) fn message(&self) -> String {
        let mut message = format!(
            "provider {}: incompatible Provider CLI: the {} CLI `{}` (version {}) does not accept af's invocation; missing: {}",
            self.provider,
            self.kind.name(),
            self.program,
            self.lacks.version.as_deref().unwrap_or("unknown"),
            self.lacks.missing.join(", ")
        );
        if let Some(observed) = &self.lacks.observed {
            message.push_str("; ");
            message.push_str(observed);
        }
        message.push_str("; fix: ");
        message.push_str(&self.lacks.fix);
        message.push_str(", then rerun `af provider status`");
        message
    }
}

/// Why the capability check refused a CLI.
pub(super) enum CliRefusal {
    /// It cannot start at all: an installation failure (ADR-0130).
    CannotStart(CliInstallationFailure),
    /// It starts and does not accept af's invocation.
    Incompatible(CliIncompatibility),
    /// af gave up waiting, through a timeout, cancellation or the Attempt deadline, or could not
    /// run the check: nothing about the CLI is known, so nothing is dispatched to it either.
    Unchecked(String),
}

impl CliRefusal {
    pub(super) fn message(&self) -> String {
        match self {
            Self::CannotStart(failure) => failure.message(),
            Self::Incompatible(incompatibility) => incompatibility.message(),
            Self::Unchecked(reason) => reason.clone(),
        }
    }
}

/// Ask the resolved executable for the help page of the (sub)command af invokes, token-free and
/// in the probe environment, and refuse it unless it declares every option af passes.
pub(super) fn check_invocation(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
) -> Result<(), CliRefusal> {
    let invocation = invocation(spec.kind);
    let timeout = probe_timeout(match spec.kind {
        ProviderKind::Claude => CLAUDE_STRUCTURAL_TIMEOUT,
        ProviderKind::Codex => PROBE_TIMEOUT,
    });
    let path = program.display().to_string();
    let run = run_bounded(
        program,
        invocation.help_args,
        spec,
        probe_path,
        cancelled,
        attempt_deadline,
        timeout,
    );
    let (missing, observed) = match run {
        BoundedRun::GaveUp(reason) => {
            return Err(CliRefusal::Unchecked(format!(
                "provider {}: af could not check that the {} CLI `{path}` accepts its invocation: `{}` {reason}",
                spec.id,
                spec.kind.name(),
                invocation.help_command()
            )));
        }
        BoundedRun::Unstartable(error) => {
            return Err(CliRefusal::CannotStart(CliInstallationFailure::new(
                spec,
                path,
                StartFailure::Unstartable(bounded_cli_line(&error)),
            )));
        }
        BoundedRun::Exited {
            status,
            stdout,
            stderr,
        } if status.success() => {
            // Some CLIs print help on stderr; the declarations are read wherever they are.
            let mut page = String::from_utf8_lossy(&stdout).into_owned();
            page.push('\n');
            page.push_str(&String::from_utf8_lossy(&stderr));
            let missing = invocation.missing_flags(&page);
            if missing.is_empty() {
                return Ok(());
            }
            (missing, None)
        }
        BoundedRun::Exited {
            status,
            stdout,
            stderr,
        } => {
            // Without a help page af cannot confirm a single option it passes.
            let (line, _) = error_and_fix(&stderr, &stdout);
            let said = line.map(|line| format!(": {line}")).unwrap_or_default();
            (
                invocation.flags.to_vec(),
                Some(format!(
                    "`{}` {}{said}",
                    invocation.help_command(),
                    exit_description(&status)
                )),
            )
        }
    };
    let version = match version_check(program, spec, probe_path, cancelled, attempt_deadline) {
        // A CLI that printed no help and fails its version check cannot start (ADR-0130).
        VersionCheck::Failed(cause) if observed.is_some() => {
            return Err(CliRefusal::CannotStart(CliInstallationFailure::new(
                spec, path, cause,
            )));
        }
        VersionCheck::Reported(version) => version,
        VersionCheck::Failed(_) | VersionCheck::Undetermined => None,
    };
    Err(CliRefusal::Incompatible(CliIncompatibility {
        provider: spec.id.clone(),
        kind: spec.kind,
        program: path,
        lacks: Box::new(Lacks {
            version,
            missing: missing.iter().map(|flag| flag.display()).collect(),
            observed,
            fix: format!("upgrade it with `{}`", invocation.upgrade),
        }),
    }))
}

/// A Codex CLI whose app-server exited before answering `initialize`, and whose own version
/// check says it starts: it lacks the protocol capability af's probes need. `observed` is the
/// probe's own error, with the exit status and the CLI's first error line.
pub(super) fn protocol_incompatibility(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
    observed: &str,
) -> CliRefusal {
    let version = match version_check(program, spec, probe_path, cancelled, attempt_deadline) {
        VersionCheck::Failed(cause) => {
            return CliRefusal::CannotStart(CliInstallationFailure::new(
                spec,
                program.display().to_string(),
                cause,
            ));
        }
        VersionCheck::Reported(version) => version,
        VersionCheck::Undetermined => None,
    };
    CliRefusal::Incompatible(CliIncompatibility {
        provider: spec.id.clone(),
        kind: spec.kind,
        program: program.display().to_string(),
        lacks: Box::new(Lacks {
            version,
            missing: vec![APP_SERVER_PROTOCOL.to_string()],
            observed: Some(observed.to_string()),
            fix: format!("upgrade it with `{}`", invocation(spec.kind).upgrade),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(directory: &Path, kind: ProviderKind) -> ProviderSpec {
        ProviderSpec {
            id: format!("{}-main", kind.name()),
            kind,
            auth_dir: Some(directory.to_path_buf()),
            explicit_selector: true,
            registry_declared: true,
            source: "fixture".into(),
        }
    }

    fn program(directory: &Path, name: &str, body: &str) -> PathBuf {
        let program = directory.join(name);
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        program
    }

    fn check(directory: &Path, kind: ProviderKind, body: &str) -> Result<(), CliRefusal> {
        let program = program(directory, kind.name(), body);
        check_invocation(
            &program,
            &spec(directory, kind),
            std::ffi::OsStr::new("/usr/bin:/bin"),
            &AtomicBool::new(false),
            None,
        )
    }

    /// A help page declaring every option af passes, in the CLI's own layout.
    fn help_of(kind: ProviderKind) -> String {
        invocation(kind)
            .flags
            .iter()
            .map(|flag| {
                if flag.spelling == flag.long {
                    format!("  {} <value>  Fixture\n", flag.long)
                } else {
                    format!("  {}, {} <value>  Fixture\n", flag.spelling, flag.long)
                }
            })
            .collect()
    }

    #[test]
    fn a_cli_declaring_every_option_passes_and_one_missing_isolation_flags_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let help = help_of(ProviderKind::Claude);
        let body = format!(
            "[ \"$1\" = --version ] && {{ echo '2.1.299 (Claude Code)'; exit 0; }}\n[ \"$1\" = --help ] && {{ printf '%s' '{help}'; exit 0; }}\nexit 64"
        );
        assert!(check(directory.path(), ProviderKind::Claude, &body).is_ok());

        let old: String = help
            .lines()
            .filter(|line| !line.contains("--safe-mode") && !line.contains("--restricted"))
            .map(|line| format!("{line}\n"))
            .collect();
        let body = format!(
            "[ \"$1\" = --version ] && {{ echo '2.1.101 (Claude Code)'; exit 0; }}\n[ \"$1\" = --help ] && {{ printf '%s' '{old}'; exit 0; }}\nexit 64"
        );
        let Err(CliRefusal::Incompatible(refused)) =
            check(directory.path(), ProviderKind::Claude, &body)
        else {
            panic!("an old Claude Code was not refused");
        };
        let message = refused.message();
        let path = directory.path().join("claude").display().to_string();
        assert_eq!(
            message,
            format!(
                "provider claude-main: incompatible Provider CLI: the claude CLI `{path}` (version 2.1.101 (Claude Code)) does not accept af's invocation; missing: --safe-mode, --restricted; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`"
            )
        );
        // The versioned half never quotes the CLI.
        assert!(
            !refused.summary().contains("2.1.101"),
            "{}",
            refused.summary()
        );
    }

    #[test]
    fn codex_is_judged_on_its_exec_help_and_a_failed_help_is_reported_with_its_line() {
        let directory = tempfile::tempdir().unwrap();
        let help = help_of(ProviderKind::Codex);
        let body = format!(
            "[ \"$1 $2\" = 'exec --help' ] && {{ printf '%s' '{help}'; exit 0; }}\nexit 64"
        );
        assert!(check(directory.path(), ProviderKind::Codex, &body).is_ok());

        let body = "[ \"$1\" = --version ] && { echo 'codex-cli 0.1.0'; exit 0; }\necho \"error: unrecognized subcommand 'exec'\" >&2\nexit 2";
        let Err(CliRefusal::Incompatible(refused)) =
            check(directory.path(), ProviderKind::Codex, body)
        else {
            panic!("a codex without `exec` was not refused");
        };
        let message = refused.message();
        assert!(message.contains("(version codex-cli 0.1.0)"), "{message}");
        assert!(
            message.contains("missing: --ephemeral, --skip-git-repo-check, --json, -C/--cd, -s/--sandbox, -o/--output-last-message, --model, -c/--config; `codex exec --help` exited 2: error: unrecognized subcommand 'exec'; fix: upgrade it with `npm install -g @openai/codex@latest`"),
            "{message}"
        );
    }

    #[test]
    fn a_cli_that_cannot_start_stays_an_installation_failure_and_a_slow_one_is_unchecked() {
        let directory = tempfile::tempdir().unwrap();
        let broken = "echo 'Error: Cannot find module ../cli.js' >&2\nexit 1";
        assert!(matches!(
            check(directory.path(), ProviderKind::Claude, broken),
            Err(CliRefusal::CannotStart(_))
        ));
        let slow = program(directory.path(), "codex", "sleep 30");
        let started = Instant::now();
        let refusal = check_invocation(
            &slow,
            &spec(directory.path(), ProviderKind::Codex),
            std::ffi::OsStr::new("/usr/bin:/bin"),
            &AtomicBool::new(false),
            Some(Instant::now() + Duration::from_millis(200)),
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        let Err(CliRefusal::Unchecked(reason)) = refusal else {
            panic!("a check af gave up on was judged");
        };
        assert!(reason.contains("deadline elapsed"), "{reason}");
    }
}
