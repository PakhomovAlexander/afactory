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

const VERSION_ARGUMENT: &str = "--version";
const MAX_CLI_LINE_CHARS: usize = 240;
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

/// A Provider whose official CLI cannot start, named by Provider ID and program path.
pub(crate) struct CliInstallationFailure {
    provider: String,
    kind: ProviderKind,
    program: String,
    cause: Box<StartFailure>,
}

enum StartFailure {
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
    fn new(spec: &ProviderSpec, program: String, cause: StartFailure) -> Self {
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
    version_check(program, spec, probe_path, cancelled, attempt_deadline)
        .map(|cause| CliInstallationFailure::new(spec, program.display().to_string(), cause))
}

fn version_check(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
) -> Option<StartFailure> {
    if cancelled.load(Ordering::Acquire)
        || check_task_probe_control(attempt_deadline, cancelled).is_err()
    {
        return None;
    }
    // A version check is a probe too: it runs in a directory af made for it (ADR-0144).
    let Ok(directory) = ProbeDirectory::new(spec) else {
        return None;
    };
    let mut command = Command::new(program);
    command.arg(VERSION_ARGUMENT);
    configure_probe_environment(&mut command, spec, probe_path, directory.path());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = match review_process::spawn(&mut command) {
        Ok(child) => child,
        Err(error) => {
            return Some(StartFailure::Unstartable(bounded_cli_line(
                &error.to_string(),
            )));
        }
    };
    let mut watch = review_process::ExitWatch::new(child.id());
    let mut stdout = child.stdout.take().expect("version check stdout was piped");
    let mut stderr = child.stderr.take().expect("version check stderr was piped");
    if set_nonblocking(&stdout)
        .and_then(|()| set_nonblocking(&stderr))
        .is_err()
    {
        stop_probe(&mut child);
        return None;
    }
    // Output past the bound is read and dropped: only the first lines are ever reported.
    let (mut out, mut err, mut exceeded) = (Vec::new(), Vec::new(), false);
    let deadline = Instant::now() + probe_timeout(PROBE_TIMEOUT);
    let deadline = attempt_deadline.map_or(deadline, |limit| deadline.min(limit));
    let status = loop {
        let drained = drain_available(&mut stdout, &mut out, &mut exceeded)
            .and_then(|_| drain_available(&mut stderr, &mut err, &mut exceeded));
        if drained.is_err() {
            stop_probe(&mut child);
            return None;
        }
        match review_process::try_reap_killing_group(&mut child, &mut watch) {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                stop_probe(&mut child);
                return None;
            }
        }
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            stop_probe(&mut child);
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    };
    // The group was ended before the reap, inside `try_reap_killing_group`.
    while matches!(
        (
            drain_available(&mut stdout, &mut out, &mut exceeded),
            drain_available(&mut stderr, &mut err, &mut exceeded),
        ),
        (Ok(true), _) | (_, Ok(true))
    ) {}
    if status.success() {
        return None;
    }
    let exit = match (
        status.code(),
        std::os::unix::process::ExitStatusExt::signal(&status),
    ) {
        (Some(code), _) => format!("exited {code}"),
        (None, Some(signal)) => format!("was ended by signal {signal}"),
        (None, None) => "exited unsuccessfully".to_string(),
    };
    let (error_line, fix) = cli_error_and_fix(&err, &out);
    Some(StartFailure::VersionCheck {
        exit,
        error_line,
        fix,
    })
}

/// The CLI's own first error line — the first line starting with "error", else its first line,
/// stderr before stdout — and the first sentence suggesting an install command.
fn cli_error_and_fix(stderr: &[u8], stdout: &[u8]) -> (Option<String>, Option<String>) {
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

/// One line of Provider text made safe to print: terminal escapes and control characters
/// removed, whitespace collapsed, and the length bounded.
fn bounded_cli_line(line: &str) -> String {
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
    fn a_suggested_fix_and_error_line_are_found_and_made_safe_to_print() {
        let (line, fix) = cli_error_and_fix(
            b"\x1b[31mwarning\x1b[0m: something\nerror: broken install\x07\nRun `npm i -g @openai/codex` to repair.\n",
            b"",
        );
        assert_eq!(line.as_deref(), Some("error: broken install"));
        assert_eq!(
            fix.as_deref(),
            Some("Run `npm i -g @openai/codex` to repair")
        );
        let (line, fix) = cli_error_and_fix(b"", b"panic: nothing useful here\n");
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
