//! Whether a Provider's official CLI can start at all (ADR-0130).
//!
//! An npm auto-update can leave `codex` without its platform package, and every invocation then
//! fails at once with the CLI's own installation error. The status, setup and Task identity
//! probes got no answer, and that silence surfaced as an unavailable context, a failed login, or
//! a Provider identity that "could not be verified". This module tells the case apart. A program
//! that is missing or not executable needs no process at all; a CLI whose own probe answered
//! nothing recognizable is asked for `--version`, and a version check that cannot start or exits
//! non-zero is a Provider installation failure, reported with the CLI's own first error line and
//! the fix it suggests.
//!
//! The version check never runs after the CLI answered — a logged-out or changed account is not
//! an installation failure — and never after af gave up waiting, which says nothing about
//! whether the CLI starts. The CLI's words are Provider-authored text: they reach human output,
//! stderr and Attempt diagnostics bounded and stripped of control characters, never a versioned
//! document, which carries only the af-authored [`CliInstallationFailure::summary`].

use super::*;
use review_runner::cli::{bounded_cli_line, error_and_fix, exit_description};

const VERSION_ARGUMENT: &str = "--version";

/// A Provider whose official CLI cannot start, named by Provider ID and program path.
pub(crate) struct CliInstallationFailure {
    provider: String,
    kind: ProviderKind,
    program: String,
    cause: Box<StartFailure>,
}

pub(super) enum StartFailure {
    /// No `claude` or `codex` on PATH.
    Missing,
    /// A file of that name is on PATH, without an execute bit.
    NotExecutable,
    /// The operating system refused to start it (a broken interpreter line, a bad format).
    Unstartable(String),
    /// It started and exited non-zero on its own version check.
    VersionCheck {
        exit: String,
        error_line: Option<String>,
        fix: Option<String>,
    },
}

impl CliInstallationFailure {
    pub(super) fn new(spec: &ProviderSpec, program: String, cause: StartFailure) -> Self {
        Self {
            provider: spec.id.clone(),
            kind: spec.kind,
            program,
            cause: Box::new(cause),
        }
    }

    /// af-authored: the Provider, the program path and how it failed, never the CLI's words.
    pub(crate) fn summary(&self) -> String {
        let what = match self.cause.as_ref() {
            StartFailure::Missing => "is not on PATH".to_string(),
            StartFailure::NotExecutable => "is not executable".to_string(),
            StartFailure::Unstartable(_) => "cannot be started".to_string(),
            StartFailure::VersionCheck { exit, .. } => {
                format!(
                    "{exit} on its own `{VERSION_ARGUMENT}` check before doing any provider work"
                )
            }
        };
        format!(
            "provider {}: Provider installation failure: the {} CLI `{}` {what}",
            self.provider,
            self.kind.name(),
            self.program
        )
    }

    /// The whole report: the summary, the CLI's own first error line, and the fix.
    pub(crate) fn message(&self) -> String {
        let mut message = self.summary();
        let said = match self.cause.as_ref() {
            StartFailure::Unstartable(error) => Some(error.as_str()),
            StartFailure::VersionCheck { error_line, .. } => error_line.as_deref(),
            StartFailure::Missing | StartFailure::NotExecutable => None,
        };
        if let Some(said) = said {
            message.push_str(": ");
            message.push_str(said);
        }
        message.push_str("; fix: ");
        message.push_str(&self.fix());
        message
    }

    /// The fix the CLI suggested, or af's own when it suggested none.
    fn fix(&self) -> String {
        let kind = self.kind.name();
        match self.cause.as_ref() {
            StartFailure::VersionCheck { fix: Some(fix), .. } => fix.clone(),
            StartFailure::Missing => {
                format!("install the official {kind} CLI on PATH, then rerun `af provider status`")
            }
            _ => format!(
                "reinstall the official {kind} CLI so `{} {VERSION_ARGUMENT}` succeeds, then rerun `af provider status`",
                self.program
            ),
        }
    }
}

/// The CLI's executable on PATH, or the installation failure that explains its absence.
pub(super) fn locate_cli(spec: &ProviderSpec) -> Result<PathBuf, CliInstallationFailure> {
    let command = spec.kind.command();
    if let Some(program) = resolve_program(command) {
        return Ok(program);
    }
    let present = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(command))
        .find(|candidate| fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file()))
        .map(|candidate| fs::canonicalize(&candidate).unwrap_or(candidate));
    Err(match present {
        Some(path) => CliInstallationFailure::new(
            spec,
            path.display().to_string(),
            StartFailure::NotExecutable,
        ),
        None => CliInstallationFailure::new(spec, command.to_string(), StartFailure::Missing),
    })
}

/// Whether af stopped waiting on a probe rather than the CLI failing to answer it. Cancellation
/// and timeouts say nothing about installation, and a second bounded wait would only double one.
pub(super) fn probe_gave_up(error: &str) -> bool {
    error.contains("timed out") || error.contains("cancelled") || error.contains("deadline elapsed")
}

/// Whether a status probe's captured output is an answer af recognizes, whatever its exit code.
pub(super) fn status_answered(kind: ProviderKind, captured: &str) -> bool {
    match kind {
        ProviderKind::Claude => serde_json::from_str::<serde_json::Value>(captured).is_ok(),
        ProviderKind::Codex => {
            captured
                .lines()
                .any(|line| line.trim().starts_with("Logged in using "))
                || super::codex_reports_logged_out(captured)
        }
    }
}

/// Ask a CLI that answered nothing recognizable for its version. `None` means it starts, or that
/// af could not tell before the deadline or cancellation; only a CLI that cannot start or exits
/// non-zero is an installation failure.
pub(super) fn diagnose_cli(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
) -> Option<CliInstallationFailure> {
    match version_check(program, spec, probe_path, cancelled, attempt_deadline) {
        VersionCheck::Failed(cause) => Some(CliInstallationFailure::new(
            spec,
            program.display().to_string(),
            cause,
        )),
        VersionCheck::Reported(_) | VersionCheck::Undetermined => None,
    }
}

/// What a CLI's own `--version` check concluded.
pub(super) enum VersionCheck {
    /// It exited zero, printing this first line, when it printed one af may quote.
    Reported(Option<String>),
    /// It cannot start, or exited non-zero: an installation failure.
    Failed(StartFailure),
    /// af gave up waiting, or could not run the check at all; this proves nothing.
    Undetermined,
}

/// Every harness version a `--version` check reported in this process, by program name, for
/// the telemetry configuration record (design §7.4). Nothing the kernel decides reads it.
static PROBED: std::sync::Mutex<std::collections::BTreeMap<String, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The harness versions this process's preflight probed: program name and the first line it
/// reported.
pub(crate) fn probed_versions() -> Vec<(String, String)> {
    PROBED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|(program, version)| (program.clone(), version.clone()))
        .collect()
}

pub(super) fn version_check(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
) -> VersionCheck {
    let checked = version_run(program, spec, probe_path, cancelled, attempt_deadline);
    if let VersionCheck::Reported(Some(line)) = &checked
        && let Some(name) = program.file_name()
    {
        PROBED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_string_lossy().into_owned(), line.clone());
    }
    checked
}

fn version_run(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
) -> VersionCheck {
    match run_bounded(
        program,
        &[VERSION_ARGUMENT],
        spec,
        probe_path,
        cancelled,
        attempt_deadline,
        probe_timeout(PROBE_TIMEOUT),
    ) {
        BoundedRun::GaveUp(_) => VersionCheck::Undetermined,
        BoundedRun::Unstartable(error) => {
            VersionCheck::Failed(StartFailure::Unstartable(bounded_cli_line(&error)))
        }
        BoundedRun::Exited {
            status,
            stdout,
            stderr,
        } if status.success() => VersionCheck::Reported(first_line(&stdout, &stderr)),
        BoundedRun::Exited {
            status,
            stdout,
            stderr,
        } => {
            let (error_line, fix) = error_and_fix(&stderr, &stdout);
            VersionCheck::Failed(StartFailure::VersionCheck {
                exit: exit_description(&status),
                error_line,
                fix,
            })
        }
    }
}

/// The first line a CLI printed, stdout before stderr, made safe to print; withheld when its
/// output carries credential or challenge material.
fn first_line(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let (line, _) = error_and_fix(b"", stdout);
    line.or_else(|| error_and_fix(stderr, b"").0)
}

/// How one bounded run of the CLI with fixed arguments ended.
pub(super) enum BoundedRun {
    /// It exited. Output past the probe bound was read and dropped: only its first lines are ever
    /// reported or parsed.
    Exited {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// The operating system refused to start it.
    Unstartable(String),
    /// af stopped waiting — the timeout, the Attempt deadline or cancellation — or could not
    /// read the process's streams. Nothing about the CLI follows.
    GaveUp(String),
}

/// Run `program args…` once, token-free, in the probe environment and a probe directory af made
/// for it (ADR-0144), with stdin closed, until it exits, `timeout` or the Attempt deadline
/// elapses, or the probe is cancelled. The process group is ended either way.
pub(super) fn run_bounded(
    program: &Path,
    args: &[&str],
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
    timeout: Duration,
) -> BoundedRun {
    if let Err(error) = availability::check(spec) {
        return BoundedRun::GaveUp(error);
    }
    if cancelled.load(Ordering::Acquire) {
        return BoundedRun::GaveUp("was cancelled".into());
    }
    if let Err(error) = check_task_probe_control(attempt_deadline, cancelled) {
        return BoundedRun::GaveUp(error);
    }
    let directory = match ProbeDirectory::new(spec) {
        Ok(directory) => directory,
        Err(error) => return BoundedRun::GaveUp(error),
    };
    let mut command = Command::new(program);
    command.args(args);
    configure_probe_environment(&mut command, spec, probe_path, directory.path());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = match review_process::spawn(&mut command) {
        Ok(child) => child,
        Err(error) => return BoundedRun::Unstartable(error.to_string()),
    };
    let mut watch = review_process::ExitWatch::new(child.id());
    let mut stdout = child.stdout.take().expect("bounded run stdout was piped");
    let mut stderr = child.stderr.take().expect("bounded run stderr was piped");
    if let Err(error) = set_nonblocking(&stdout).and_then(|()| set_nonblocking(&stderr)) {
        stop_probe(&mut child);
        return BoundedRun::GaveUp(error);
    }
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_exceeded, mut err_exceeded) = (false, false);
    let deadline = Instant::now() + timeout;
    let deadline = attempt_deadline.map_or(deadline, |limit| deadline.min(limit));
    let status = loop {
        let drained = drain_available(&mut stdout, &mut out, &mut out_exceeded)
            .and_then(|_| drain_available(&mut stderr, &mut err, &mut err_exceeded));
        if let Err(error) = drained {
            stop_probe(&mut child);
            return BoundedRun::GaveUp(error);
        }
        match review_process::try_reap_killing_group(&mut child, &mut watch) {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                stop_probe(&mut child);
                return BoundedRun::GaveUp(error.to_string());
            }
        }
        if cancelled.load(Ordering::Acquire) {
            stop_probe(&mut child);
            return BoundedRun::GaveUp("was cancelled".into());
        }
        if Instant::now() >= deadline {
            stop_probe(&mut child);
            return BoundedRun::GaveUp(
                if attempt_deadline.is_some_and(|limit| limit <= deadline) {
                    "stopped: Task Provider identity check deadline elapsed".into()
                } else {
                    format!("timed out after {} seconds", timeout.as_secs())
                },
            );
        }
        thread::sleep(Duration::from_millis(25));
    };
    // The group was ended before the reap, inside `try_reap_killing_group`.
    while matches!(
        (
            drain_available(&mut stdout, &mut out, &mut out_exceeded),
            drain_available(&mut stderr, &mut err, &mut err_exceeded),
        ),
        (Ok(true), _) | (_, Ok(true))
    ) {}
    BoundedRun::Exited {
        status,
        stdout: out,
        stderr: err,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NPM_ERROR: &str = "Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall Codex: npm install -g @openai/codex@latest";

    fn spec(directory: &Path) -> ProviderSpec {
        ProviderSpec {
            id: "codex-main".into(),
            kind: ProviderKind::Codex,
            auth_dir: Some(directory.to_path_buf()),
            explicit_selector: true,
            registry_declared: true,
            source: "fixture".into(),
        }
    }

    fn program(directory: &Path, body: &str) -> PathBuf {
        let program = directory.join("codex");
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        program
    }

    fn diagnose(directory: &Path, program: &Path) -> Option<CliInstallationFailure> {
        diagnose_cli(
            program,
            &spec(directory),
            std::ffi::OsStr::new("/usr/bin:/bin"),
            &AtomicBool::new(false),
            None,
        )
    }

    #[test]
    fn a_cli_that_exits_on_its_version_check_reports_its_own_error_and_fix() {
        let directory = tempfile::tempdir().unwrap();
        let program = program(
            directory.path(),
            &format!("printf '%s\\n' 'file:///codex.js:1' '' '{NPM_ERROR}' >&2\nexit 1"),
        );
        let failure = diagnose(directory.path(), &program).expect("installation failure");
        let message = failure.message();
        assert!(
            message
                .starts_with("provider codex-main: Provider installation failure: the codex CLI"),
            "{message}"
        );
        assert!(
            message.contains(&program.display().to_string()),
            "{message}"
        );
        assert!(
            message.contains("exited 1 on its own `--version` check"),
            "{message}"
        );
        assert!(message.contains(&format!(": {NPM_ERROR};")), "{message}");
        assert!(
            message.ends_with("; fix: Reinstall Codex: npm install -g @openai/codex@latest"),
            "{message}"
        );
        // The summary is the af-authored half a versioned document may carry.
        let summary = failure.summary();
        assert!(
            !summary.contains("Missing optional dependency"),
            "{summary}"
        );
        assert!(
            summary.contains(&program.display().to_string()),
            "{summary}"
        );
    }

    #[test]
    fn a_working_cli_and_a_slow_or_cancelled_check_are_not_installation_failures() {
        let directory = tempfile::tempdir().unwrap();
        let working = program(directory.path(), "echo 'codex-cli 0.149.0'\nexit 0");
        assert!(diagnose(directory.path(), &working).is_none());

        let slow = program(directory.path(), "sleep 30\nexit 1");
        let started = Instant::now();
        let none = diagnose_cli(
            &slow,
            &spec(directory.path()),
            std::ffi::OsStr::new("/usr/bin:/bin"),
            &AtomicBool::new(false),
            Some(Instant::now() + Duration::from_millis(200)),
        );
        assert!(none.is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
        let cancelled = AtomicBool::new(true);
        assert!(
            diagnose_cli(
                &slow,
                &spec(directory.path()),
                std::ffi::OsStr::new("/usr/bin:/bin"),
                &cancelled,
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn a_cli_the_operating_system_cannot_start_is_an_installation_failure() {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("codex");
        std::fs::write(&program, "#!/nonexistent/node\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let message = diagnose(directory.path(), &program)
            .expect("installation failure")
            .message();
        assert!(message.contains("cannot be started: "), "{message}");
        assert!(
            message.contains("; fix: reinstall the official codex CLI"),
            "{message}"
        );
    }

    #[test]
    fn only_a_silent_status_probe_or_one_af_did_not_give_up_on_is_diagnosed() {
        assert!(status_answered(
            ProviderKind::Codex,
            "Logged in using ChatGPT\n"
        ));
        assert!(status_answered(ProviderKind::Codex, "Not logged in\n"));
        assert!(!status_answered(
            ProviderKind::Codex,
            "Error: cannot determine whether user is not logged in\n"
        ));
        assert!(!status_answered(
            ProviderKind::Codex,
            "Not logged in\nError: status backend failed\n"
        ));
        assert!(!status_answered(ProviderKind::Codex, NPM_ERROR));
        assert!(status_answered(
            ProviderKind::Claude,
            "{\"loggedIn\":false}"
        ));
        assert!(!status_answered(ProviderKind::Claude, ""));
        assert!(probe_gave_up(
            "provider status probe timed out after 15 seconds"
        ));
        assert!(probe_gave_up("provider status refresh cancelled"));
        assert!(probe_gave_up(
            "Task Provider identity check deadline elapsed"
        ));
        assert!(!probe_gave_up(
            "cannot start provider status probe: No such file"
        ));
    }
}
