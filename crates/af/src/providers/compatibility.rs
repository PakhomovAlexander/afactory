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
//! refusal is the only outcome. For Codex, the check also proves the app-server protocol the
//! identity and usage probes need with a token-free `initialize` round trip of its own
//! ([`check_app_server`]): only a JSON-RPC response with id 1 whose `result` is a JSON object
//! proves it, and an app-server that exits before one, rejects `initialize` or answers it
//! malformed (`{"id":1}`, a `null` or non-object `result`, a line that is not JSON-RPC) is
//! reported as the missing protocol capability ([`protocol_incompatibility`]) on every surface
//! alike, except an observed host permission refusal before `initialize`, which proves neither
//! compatibility nor login.
//!
//! The help page, the version and the CLI's error line are Provider-authored text: they reach
//! human output, stderr and Attempt diagnostics bounded and stripped of control characters, never
//! a versioned document, which carries only closed states and af-authored diagnostics.

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

/// The host refused a probe operation before af could establish compatibility or login.
pub(crate) struct DeniedProbe {
    provider: String,
    operation: String,
}

impl DeniedProbe {
    fn new(spec: &ProviderSpec, operation: String) -> Self {
        Self {
            provider: spec.id.clone(),
            operation,
        }
    }

    pub(crate) fn summary(&self) -> String {
        format!(
            "provider {}: host denied the Provider capability probe; compatibility and login could not be established",
            self.provider
        )
    }

    pub(crate) fn message(&self) -> String {
        let context = AgentEnvironment::detect();
        let location = if context.sandboxed {
            " inside a detected sandbox"
        } else {
            ""
        };
        let remedy = if context.sandboxed {
            "retry outside that sandbox or with host-approved permissions"
        } else {
            "retry with host-approved permissions or outside any sandbox"
        };
        format!(
            "provider {}: the probe failed with a host permission denial{location}: {}; compatibility and login could not be established; {remedy}",
            self.provider, self.operation
        )
    }
}

/// Environment markers describe the caller only; they never decide a capability probe.
pub(crate) struct AgentEnvironment {
    pub(crate) harness: Option<&'static str>,
    pub(crate) sandboxed: bool,
    pub(crate) network_allowed: Option<bool>,
}

impl AgentEnvironment {
    pub(crate) fn detect() -> Self {
        let codex_network = std::env::var("CODEX_SANDBOX_NETWORK_DISABLED").ok();
        let codex = std::env::var_os("CODEX_THREAD_ID").is_some()
            || std::env::var_os("CODEX_SANDBOX").is_some()
            || codex_network.is_some();
        let claude = std::env::var_os("CLAUDECODE").is_some()
            || std::env::var_os("CLAUDE_CODE_SANDBOXED").is_some();
        let sandboxed = std::env::var_os("CODEX_SANDBOX").is_some()
            || codex_network.is_some()
            || std::env::var("CLAUDE_CODE_SANDBOXED").is_ok_and(|value| value == "1");
        let network_allowed = codex_network.map(|value| value != "1");
        Self {
            harness: if codex {
                Some("Codex")
            } else if claude {
                Some("Claude Code")
            } else {
                None
            },
            sandboxed,
            network_allowed,
        }
    }
}

fn permission_denied(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "operation not permitted",
        "permission denied",
        "os error 1",
        "os error 13",
        "eperm",
        "eacces",
    ]
    .iter()
    .any(|word| lower.contains(word))
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
    /// A host permission failure says nothing about the CLI's capabilities or login.
    Denied(DeniedProbe),
    /// af gave up waiting, through a timeout, cancellation or the Attempt deadline, or could not
    /// run the check: nothing about the CLI is known, so nothing is dispatched to it either.
    Unchecked(String),
}

impl CliRefusal {
    pub(super) fn message(&self) -> String {
        match self {
            Self::CannotStart(failure) => failure.message(),
            Self::Incompatible(incompatibility) => incompatibility.message(),
            Self::Denied(denied) => denied.message(),
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
    availability::check(spec).map_err(CliRefusal::Unchecked)?;
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
            if permission_denied(&error) {
                return Err(CliRefusal::Denied(DeniedProbe::new(
                    spec,
                    format!(
                        "`{}`: {}",
                        invocation.help_command(),
                        bounded_cli_line(&error)
                    ),
                )));
            }
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
                return match spec.kind {
                    ProviderKind::Claude => Ok(()),
                    ProviderKind::Codex => {
                        check_app_server(program, spec, probe_path, cancelled, attempt_deadline)
                    }
                };
            }
            (missing, None)
        }
        BoundedRun::Exited {
            status,
            stdout,
            stderr,
        } => {
            if permission_denied(&String::from_utf8_lossy(&stderr)) {
                let (line, _) = error_and_fix(&stderr, &stdout);
                return Err(CliRefusal::Denied(DeniedProbe::new(
                    spec,
                    format!(
                        "`{}` {}{}",
                        invocation.help_command(),
                        exit_description(&status),
                        line.map(|line| format!(": {line}")).unwrap_or_default()
                    ),
                )));
            }
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

/// The token-free half of the Codex app-server protocol every af probe needs: start the bare
/// `codex app-server` in the probe environment, send `initialize`, accept its answer and stop the
/// process. No account or rate-limit request is sent, the answer is never read as a login answer,
/// and the round trip is bounded by the probe timeout and the Attempt deadline. Only an answer
/// whose `result` is a JSON object proves the protocol: an app-server that exits before one,
/// rejects af's `initialize` or answers it malformed lacks the protocol capability; one af gave up
/// waiting on, or could not run, leaves the CLI unchecked.
fn check_app_server(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
) -> Result<(), CliRefusal> {
    let mut observed = AppServerProbe::default();
    match probe_codex_request_observed(
        program,
        spec,
        probe_path,
        cancelled,
        None,
        attempt_deadline,
        &mut observed,
    ) {
        Ok(_) => Ok(()),
        Err(error)
            if !observed.answered
                && permission_denied(&error)
                && (error.starts_with("cannot start Codex app-server probe:")
                    || (error.starts_with("codex app-server exited")
                        && error.contains("before answering initialize"))) =>
        {
            Err(CliRefusal::Denied(DeniedProbe::new(spec, error)))
        }
        Err(error) if observed.lacks_protocol => Err(protocol_incompatibility(
            program,
            spec,
            probe_path,
            cancelled,
            attempt_deadline,
            &error,
        )),
        Err(error) => Err(CliRefusal::Unchecked(format!(
            "provider {}: af could not check that the {} CLI `{}` serves {APP_SERVER_PROTOCOL}: {error}",
            spec.id,
            spec.kind.name(),
            program.display()
        ))),
    }
}

/// A Codex CLI whose app-server gave no `initialize` result object — it exited first, rejected
/// it or answered it malformed — and whose own version check says it starts: it lacks the
/// protocol capability af's probes need. `observed` is the probe's own error, with the exit
/// status when it exited and the CLI's first error line or the line it answered with.
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

    /// ADR-0246: a review-Campaign reviewer's dispatch passes its native reply schema, so a CLI
    /// whose help page lacks the option that carries it is refused by the token-free check
    /// before any paid dispatch, on both Providers, naming exactly that option.
    #[test]
    fn a_cli_without_the_native_reply_schema_option_is_refused() {
        for (kind, option, help_args, version) in [
            (
                ProviderKind::Claude,
                "--json-schema",
                "$1",
                "2.1.299 (Claude Code)",
            ),
            (
                ProviderKind::Codex,
                "--output-schema",
                "$1 $2",
                "codex-cli 0.121.0",
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let page = invocation(kind).help_args.join(" ");
            let help: String = help_of(kind)
                .lines()
                .filter(|line| !line.contains(option))
                .map(|line| format!("{line}\n"))
                .collect();
            let body = format!(
                "[ \"$1\" = --version ] && {{ echo '{version}'; exit 0; }}\n[ \"{help_args}\" = '{page}' ] && {{ printf '%s' '{help}'; exit 0; }}\n{INITIALIZES}\nexit 64"
            );
            let Err(CliRefusal::Incompatible(refused)) = check(directory.path(), kind, &body)
            else {
                panic!("{option}: a CLI without the reply schema option was not refused");
            };
            let message = refused.message();
            assert!(
                message.contains(&format!("missing: {option};")),
                "{message}"
            );
        }
    }

    #[test]
    fn codex_is_judged_on_its_exec_help_and_a_failed_help_is_reported_with_its_line() {
        let directory = tempfile::tempdir().unwrap();
        let help = help_of(ProviderKind::Codex);
        let body = format!(
            "[ \"$1 $2\" = 'exec --help' ] && {{ printf '%s' '{help}'; exit 0; }}\n{INITIALIZES}\nexit 64"
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
            message.contains("missing: --ephemeral, --skip-git-repo-check, --json, -C/--cd, -s/--sandbox, -o/--output-last-message, --output-schema, --model, -c/--config; `codex exec --help` exited 2: error: unrecognized subcommand 'exec'; fix: upgrade it with `npm install -g @openai/codex@latest`"),
            "{message}"
        );
    }

    /// A bare `codex app-server` that answers `initialize` and then waits for more.
    const INITIALIZES: &str = "[ \"$1\" = app-server ] && [ -z \"$2\" ] && { IFS= read -r _; printf '%s\\n' '{\"id\":1,\"result\":{}}'; while IFS= read -r _; do :; done; exit 0; }";

    /// A codex whose `exec --help` declares every option and whose `--version` answers, with
    /// `app_server` as its bare `app-server`.
    fn codex_with_app_server(app_server: &str) -> String {
        let help = help_of(ProviderKind::Codex);
        format!(
            "[ \"$1\" = --version ] && {{ echo 'codex-cli 0.121.0'; exit 0; }}\n[ \"$1 $2\" = 'exec --help' ] && {{ printf '%s' '{help}'; exit 0; }}\nif [ \"$1\" = app-server ]; then\n{app_server}\nfi\nexit 64"
        )
    }

    /// Issue #259, round 2: the check itself starts the bare app-server, so a codex whose help is
    /// complete but whose app-server exits before `initialize` is refused for the protocol
    /// capability with the exit and the CLI's own line, as it is by every surface that runs it.
    #[test]
    fn a_codex_whose_app_server_exits_before_initialize_lacks_the_protocol_capability() {
        let directory = tempfile::tempdir().unwrap();
        let body = codex_with_app_server(
            "printf '%s\\n' \"error: unrecognized subcommand 'app-server'\" '' 'Usage: codex [OPTIONS] [PROMPT]' >&2\nexit 2",
        );
        let Err(CliRefusal::Incompatible(refused)) =
            check(directory.path(), ProviderKind::Codex, &body)
        else {
            panic!("an app-server that exits before initialize was accepted");
        };
        let path = directory.path().join("codex").display().to_string();
        assert_eq!(
            refused.message(),
            format!(
                "provider codex-main: incompatible Provider CLI: the codex CLI `{path}` (version codex-cli 0.121.0) does not accept af's invocation; missing: {APP_SERVER_PROTOCOL}; codex app-server exited 2 before answering initialize: error: unrecognized subcommand 'app-server'; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`"
            )
        );
        assert!(
            refused
                .summary()
                .ends_with(&format!("missing: {APP_SERVER_PROTOCOL}")),
            "{}",
            refused.summary()
        );

        // Issue #259, round 4: a rejection carries the process outcome and the line it answered
        // with — its exit status when it exited, af's phrase when af stopped it still running.
        let rejection = r#"{"id":1,"error":{"code":-32600,"message":"bad init"}}"#;
        for (after, outcome) in [
            ("exit 0", "exited 0"),
            ("while IFS= read -r _; do :; done", "was stopped by af"),
        ] {
            let rejecting = codex_with_app_server(&format!(
                "IFS= read -r _\nprintf '%s\\n' '{rejection}'\n{after}"
            ));
            let Err(CliRefusal::Incompatible(refused)) =
                check(directory.path(), ProviderKind::Codex, &rejecting)
            else {
                panic!("an app-server that rejects initialize was accepted");
            };
            assert!(
                refused.message().contains(&format!(
                    "; missing: {APP_SERVER_PROTOCOL}; codex app-server {outcome} after answering initialize with an error: {rejection}; fix:"
                )),
                "{}",
                refused.message()
            );
        }

        // An answer is enough, however soon the app-server exits after giving it.
        let answers_and_exits = codex_with_app_server(
            "IFS= read -r _\nprintf '%s\\n' '{\"id\":1,\"result\":{}}'\nexit 0",
        );
        assert!(check(directory.path(), ProviderKind::Codex, &answers_and_exits).is_ok());
    }

    /// Issue #259, round 3: only a response with id 1 whose `result` is a JSON object proves the
    /// protocol. `{"id":1}`, a `null` or non-object `result` and a line that is not JSON-RPC are
    /// the missing protocol capability, reported with the exit and the line it answered with.
    #[test]
    fn only_an_initialize_result_object_proves_the_protocol() {
        let directory = tempfile::tempdir().unwrap();
        for (answer, how, line) in [
            (r#"{"id":1}"#, "without a result object", r#"{"id":1}"#),
            (
                r#"{"id":1,"result":null}"#,
                "without a result object",
                r#"{"id":1,"result":null}"#,
            ),
            (
                r#"{"id":1,"result":"ok"}"#,
                "without a result object",
                r#"{"id":1,"result":"ok"}"#,
            ),
            (
                "codex app-server ready",
                "with a line that is not a JSON-RPC message",
                "codex app-server ready",
            ),
            ("[1]", "with a line that is not a JSON-RPC message", "[1]"),
        ] {
            std::fs::write(directory.path().join("answer"), format!("{answer}\n")).unwrap();
            let body = codex_with_app_server(&format!(
                "IFS= read -r _\ncat '{}'\nexit 0",
                directory.path().join("answer").display()
            ));
            let Err(CliRefusal::Incompatible(refused)) =
                check(directory.path(), ProviderKind::Codex, &body)
            else {
                panic!("{answer}: a malformed initialize answer was accepted");
            };
            let message = refused.message();
            assert!(
                message.contains(&format!(
                    "missing: {APP_SERVER_PROTOCOL}; codex app-server exited 0 after answering initialize {how}: {line}; fix:"
                )),
                "{message}"
            );
        }

        // A notification before the answer is skipped; the result object is the proof.
        let answers = codex_with_app_server(
            "IFS= read -r _\nprintf '%s\\n' '' '{\"method\":\"note\"}' '{\"id\":1,\"result\":{}}'\nexit 0",
        );
        assert!(check(directory.path(), ProviderKind::Codex, &answers).is_ok());
    }

    /// The protocol check is bounded by the probe timeout and the Attempt deadline, and a silent
    /// app-server leaves the CLI unchecked, never incompatible and never logged in.
    #[test]
    fn a_silent_app_server_is_unchecked_within_the_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let program = program(
            directory.path(),
            "codex",
            &codex_with_app_server("sleep 30"),
        );
        let started = Instant::now();
        let refusal = check_invocation(
            &program,
            &spec(directory.path(), ProviderKind::Codex),
            std::ffi::OsStr::new("/usr/bin:/bin"),
            &AtomicBool::new(false),
            Some(Instant::now() + Duration::from_secs(2)),
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        let Err(CliRefusal::Unchecked(reason)) = refusal else {
            panic!("a silent app-server was judged");
        };
        assert!(reason.contains(APP_SERVER_PROTOCOL), "{reason}");
    }

    /// An app-server error line that looks like credential material is withheld whole, with the
    /// exit kept; an ordinary clap error line and the trusted executable path pass through.
    #[test]
    fn an_app_server_error_line_carrying_credential_material_is_withheld() {
        use review_runner::cli::WITHHELD_LINE;
        let root = tempfile::tempdir().unwrap();
        // The trusted path may contain the credential prefix (as in task-sk-parent). It is
        // not leaked stderr: check complete synthetic secrets, independently of that path.
        for directory in [
            root.path().to_path_buf(),
            root.path().join("task-sk-parent"),
        ] {
            std::fs::create_dir_all(&directory).unwrap();
            for line in [
                "error: unknown option '--safe-mode'; API_KEY=sk-secret",
                "Authorization: Bearer abcFIXTUREdefFIXTUREghi",
                "error: 3f9a1c0b7e6d5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b",
                "error: OPENAI_API_KEY=\"sk-proj-FIXTURE\"",
                "error: unexpected argument '--stdio' found",
            ] {
                std::fs::write(directory.join("said"), format!("{line}\n")).unwrap();
                let body = codex_with_app_server(&format!(
                    "cat '{}' >&2\nexit 2",
                    directory.join("said").display()
                ));
                let Err(CliRefusal::Incompatible(refused)) =
                    check(&directory, ProviderKind::Codex, &body)
                else {
                    panic!("{line}: an app-server that exits before initialize was accepted");
                };
                let message = refused.message();
                let quoted = if line.contains("--stdio") {
                    line
                } else {
                    WITHHELD_LINE
                };
                assert!(
                    message.contains(&format!(
                        "codex app-server exited 2 before answering initialize: {quoted}; fix:"
                    )),
                    "{message}"
                );
                assert!(
                    message.contains(directory.join("codex").to_str().unwrap()),
                    "{message}"
                );
                for secret in [
                    "sk-secret",
                    "abcFIXTUREdefFIXTUREghi",
                    "3f9a1c0b7e6d5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b",
                    "sk-proj-FIXTURE",
                ] {
                    assert!(!message.contains(secret), "{message}");
                }
            }
        }
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

    #[test]
    fn a_claude_help_probe_denied_by_the_host_is_not_incompatible() {
        let directory = tempfile::tempdir().unwrap();
        let body = "[ \"$1\" = --version ] && { echo '2.1.299 (Claude Code)'; exit 0; }\n\
                    echo 'Error: Permission denied (os error 13)' >&2\nexit 1";
        let Err(CliRefusal::Denied(denied)) = check(directory.path(), ProviderKind::Claude, body)
        else {
            panic!("a denied Claude help probe was not classified");
        };
        let message = denied.message();
        assert!(
            message.contains("`claude --help` exited 1: Error: Permission denied"),
            "{message}"
        );
        assert!(!message.contains("upgrade"), "{message}");
    }

    #[test]
    fn an_app_server_spawn_denied_by_the_host_is_not_missing_protocol() {
        let directory = tempfile::tempdir().unwrap();
        let help = help_of(ProviderKind::Codex);
        let body = format!(
            "[ \"$1 $2\" = 'exec --help' ] && {{ chmod 600 \"$0\"; printf '%s' '{help}'; exit 0; }}\nexit 64"
        );
        let Err(CliRefusal::Denied(denied)) = check(directory.path(), ProviderKind::Codex, &body)
        else {
            panic!("an app-server the host refused to spawn was not classified");
        };
        let message = denied.message();
        assert!(
            message.contains("cannot start Codex app-server probe: Permission denied"),
            "{message}"
        );
        assert!(!message.contains("upgrade"), "{message}");
    }
}
