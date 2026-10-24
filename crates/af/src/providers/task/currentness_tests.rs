use super::*;

/// The Attempt wall for a wrapper whose subject is not the deadline (ADR-0114). Its identity
/// recheck runs a fixture shell that took seconds to start on a loaded gate, so a 5 s wall raced
/// scheduling; two minutes only bounds a hung fixture, and a passing test never waits for it.
/// Tests whose subject is the recheck's deadline keep their own short walls.
const LOAD_SAFE_WALL: Duration = Duration::from_secs(120);

fn fixture(kind: ProviderKind, directory: &Path, body: &str) -> (PathBuf, ProviderSpec) {
    let program = directory.join("native-fixture");
    std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let spec = ProviderSpec {
        id: "fixture".into(),
        kind,
        auth_dir: Some(directory.to_path_buf()),
        explicit_selector: true,
        registry_declared: true,
        source: "fixture".into(),
    };
    (program, spec)
}

/// Publishes the fixture shell's PID as `$auth/ready`, complete or not at all. A redirection opens
/// and truncates its target before `printf` writes, so a deadline kill between the two left an
/// empty `ready` that the reap check read as a PID (issue #278). The PID is written beside the
/// marker and renamed over it: a kill leaves at most `ready.tmp`, never a partial `ready`.
const PUBLISH_READY: &str =
    "printf '%s' $$ >\"$auth/ready.tmp\" && mv \"$auth/ready.tmp\" \"$auth/ready\"";

/// Neither the marker nor its unpublished temporary: the fixture never started.
fn unspawned(directory: &Path) -> bool {
    !directory.join("ready").exists() && !directory.join("ready.tmp").exists()
}

#[test]
fn identity_rechecks_use_remaining_attempt_deadline_and_prespawn_cancellation() {
    for kind in [ProviderKind::Claude, ProviderKind::Codex] {
        let directory = tempfile::tempdir().unwrap();
        let (program, spec) = fixture(
            kind,
            directory.path(),
            &format!("auth=${{CLAUDE_CONFIG_DIR:-$CODEX_HOME}}\n{PUBLISH_READY}\nsleep 30 &\nwait"),
        );
        let flag = AtomicBool::new(true);
        let path = std::ffi::OsStr::new("/usr/bin:/bin");
        let deadline = Instant::now() + Duration::from_secs(2);
        assert!(probe_identity(&program, &spec, path, &flag, Some(deadline)).is_err());
        assert!(
            unspawned(directory.path()),
            "pre-cancelled identity check spawned"
        );
        flag.store(false, Ordering::Release);
        let started = Instant::now();
        let deadline = started + Duration::from_millis(150);
        let error = probe_identity(&program, &spec, path, &flag, Some(deadline)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "check received the full status timeout"
        );
        // Under load the deadline can expire before the shell reaches its first marker.
        // The separate cancellation case below requires an observed live process.
        if let Ok(text) = std::fs::read_to_string(directory.path().join("ready")) {
            let pid: i32 = text
                .parse()
                .unwrap_or_else(|error| panic!("published PID marker {text:?}: {error}"));
            assert!(
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err(),
                "status leader was not reaped"
            );
            std::fs::remove_file(directory.path().join("ready")).unwrap();
        }
        // A kill between the write and the rename leaves the unpublished temporary behind.
        let _ = std::fs::remove_file(directory.path().join("ready.tmp"));
        assert!(probe_identity(&program, &spec, path, &flag, Some(deadline)).is_err());
        assert!(unspawned(directory.path()), "expired check spawned");
    }
}

#[test]
fn identity_rechecks_cancel_an_inflight_status_process() {
    for kind in [ProviderKind::Claude, ProviderKind::Codex] {
        let directory = tempfile::tempdir().unwrap();
        let (program, spec) = fixture(
            kind,
            directory.path(),
            &format!("auth=${{CLAUDE_CONFIG_DIR:-$CODEX_HOME}}\n{PUBLISH_READY}\nsleep 30 &\nwait"),
        );
        let flag = AtomicBool::new(false);
        let (result, pid) = std::thread::scope(|scope| {
            let observer = scope.spawn(|| {
                // Generous for a loaded machine: the flag below ends the probe as soon as the
                // status process is seen, so a fast machine never waits this long.
                let deadline = Instant::now() + Duration::from_secs(20);
                let pid = loop {
                    if let Ok(text) = std::fs::read_to_string(directory.path().join("ready"))
                        && let Ok(pid) = text.parse::<i32>()
                    {
                        break Some(pid);
                    }
                    if Instant::now() >= deadline {
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                };
                let live = pid.filter(|pid| {
                    nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), None).is_ok()
                });
                flag.store(true, Ordering::Release);
                live
            });
            let result = probe_identity(
                &program,
                &spec,
                std::ffi::OsStr::new("/usr/bin:/bin"),
                &flag,
                // Past the observer's window, so cancellation, not the deadline, ends it.
                Some(Instant::now() + Duration::from_secs(30)),
            );
            (result, observer.join().unwrap())
        });
        let pid = pid.expect("status process must be live before cancellation");
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err(),
            "status leader was not reaped"
        );
    }
}

#[test]
fn interrupted_fixture_publishes_a_complete_pid_marker_or_none() {
    use std::os::unix::process::CommandExt;
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready");
    let temporary = directory.path().join("ready.tmp");
    // Deterministic: a `mv` that never returns holds the fixture after its write and before
    // publication, the window a redirection straight onto `ready` left empty.
    let stubs = tempfile::tempdir().unwrap();
    let stub = stubs.path().join("mv");
    std::fs::write(&stub, "#!/bin/sh\n: >\"$auth/held\"\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(PUBLISH_READY)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", stubs.path().display()))
        .env("auth", directory.path())
        .process_group(0)
        .spawn()
        .unwrap();
    let leader = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    // Kills the held group even when an assertion below fails first.
    struct Group(nix::unistd::Pid);
    impl Drop for Group {
        fn drop(&mut self) {
            let _ = nix::sys::signal::killpg(self.0, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let group = Group(leader);
    let held = Instant::now() + LOAD_SAFE_WALL;
    while !directory.path().join("held").exists() {
        assert!(Instant::now() < held, "fixture never reached publication");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!ready.exists(), "marker visible before publication");
    drop(group);
    child.wait().unwrap();
    assert!(!ready.exists(), "interrupted publication left a marker");
    assert_eq!(
        std::fs::read_to_string(&temporary).unwrap(),
        child.id().to_string()
    );
    std::fs::remove_file(&temporary).unwrap();
    std::fs::remove_file(directory.path().join("held")).unwrap();
    // Kill points from before the shell starts to after it has published, so both outcomes and
    // the window between the write and the rename are exercised; every outcome must hold.
    for step in 0..64u64 {
        // A directory per kill point: a rename already inside the kernel when SIGKILL lands can
        // only finish in its own run's directory, never publish into a later run's.
        let run = tempfile::tempdir_in(directory.path()).unwrap();
        let ready = run.path().join("ready");
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("{PUBLISH_READY}\nsleep 30"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("auth", run.path())
            .process_group(0)
            .spawn()
            .unwrap();
        let leader = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
        std::thread::sleep(Duration::from_micros(step * 250));
        // The whole group, so a `mv` mid-rename is killed too. Its orphan may linger as a
        // zombie that `killpg(.., None)` still finds and only init reaps, so the check does not
        // wait for the group to vanish: a SIGKILLed process never runs again and cannot write.
        let _ = nix::sys::signal::killpg(leader, nix::sys::signal::Signal::SIGKILL);
        child.wait().unwrap();
        match std::fs::read_to_string(&ready) {
            Ok(text) => assert_eq!(text, child.id().to_string(), "partial PID marker"),
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        }
    }
}

/// An inner adapter that records whether it was reached.
struct Recording {
    reached: std::sync::Arc<AtomicBool>,
}
impl WorkerModelAdapter for Recording {
    fn credential_mode(&self) -> review_core::CredentialModeV1 {
        review_core::CredentialModeV1::TrustedUnsafe
    }
    fn provider_kind(&self) -> &'static str {
        "claude"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        None
    }
    fn invoke(
        &self,
        _cas: &Cas,
        _workdir: &Path,
        _input: Vec<u8>,
        _timeout: Duration,
        _access: review_runner::task::WorkerAccess,
        _cancellation: Option<&AtomicBool>,
        _environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        self.reached.store(true, Ordering::SeqCst);
        ModelWorkerReturn {
            notes: Default::default(),
            message: Ok(b"{}".to_vec()),
            usage: None,
            usage_observation: None,
            raw_artifact_ids: vec![],
            native_failure: None,
        }
    }
}

/// The wrapper around a recording native adapter, finding its command through `resolve`.
fn wrapper(
    spec: ProviderSpec,
    program: PathBuf,
    reached: &std::sync::Arc<AtomicBool>,
    resolve: fn(&str) -> Option<PathBuf>,
) -> CurrentTaskProviderAdapter {
    // Fixed grants: a gate may run these tests without USER in its environment.
    let binary = BinaryStamp::of(&program);
    let identity = TaskProviderIdentity {
        home: spec.auth_dir.clone().map(Into::into),
        user: Some("fixture".into()),
        spec,
        program,
        principal_id: "sha256:".to_string() + &"0".repeat(64),
        auth_method: "claude.ai".into(),
        org: None,
        cli_version: None,
        binary,
        probe_wall: None,
        probe_path: sanitized_path(),
    };
    let native = Recording {
        reached: reached.clone(),
    };
    CurrentTaskProviderAdapter {
        model: "claude-fixture-1".into(),
        effort: "high".into(),
        resolve,
        current: Mutex::new((identity, Arc::new(native))),
    }
}

#[test]
fn sandbox_environment_passes_the_identity_recheck_before_it_can_reach_the_native_client() {
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(ProviderKind::Claude, directory.path(), "exit 0");
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let reached = std::sync::Arc::new(AtomicBool::new(false));
    // A fixture provider is not in the machine-local registry, so the recheck fails.
    let wrapper = wrapper(spec, program, &reached, resolve_program);
    let environment = [(
        "CARGO_TARGET_DIR".to_string(),
        "/sandbox/.af-cache".to_string(),
    )];
    let returned = wrapper.invoke(
        &cas,
        directory.path(),
        b"{}".to_vec(),
        LOAD_SAFE_WALL,
        review_runner::task::WorkerAccess::ReadOnly,
        None,
        &environment,
    );
    let error = returned.message.unwrap_err().to_string();
    assert!(
        error.contains("Captured Task Provider identity is no longer current"),
        "the wrapper's own recheck answered: {error}"
    );
    assert!(
        !reached.load(Ordering::SeqCst),
        "a failed recheck must not reach the native client"
    );
}

#[test]
fn a_removed_executable_with_no_installed_client_refuses_by_name_before_any_probe() {
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(ProviderKind::Claude, directory.path(), "exit 0");
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let reached = std::sync::Arc::new(AtomicBool::new(false));
    let wrapper = wrapper(spec, program.clone(), &reached, |_| None);
    // The client was uninstalled: its captured file is gone and `PATH` resolves nothing.
    std::fs::remove_file(&program).unwrap();
    let returned = wrapper.invoke(
        &cas,
        directory.path(),
        b"{}".to_vec(),
        Duration::from_secs(5),
        review_runner::task::WorkerAccess::ReadOnly,
        None,
        &[],
    );
    assert_eq!(
        returned.message.unwrap_err(),
        "Captured Task Provider executable was removed and no installed client replaces it"
    );
    assert_eq!(returned.usage.unwrap().chargeable_tokens.get(), 0);
    assert!(!reached.load(Ordering::SeqCst));
}

#[test]
fn the_installed_client_replaces_a_removed_executable_and_a_present_one_is_kept() {
    static INSTALLED: OnceLock<PathBuf> = OnceLock::new();
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(ProviderKind::Claude, directory.path(), "exit 0");
    let installed = directory.path().join("installed");
    std::fs::copy(&program, &installed).unwrap();
    INSTALLED.set(installed.clone()).unwrap();
    let reached = std::sync::Arc::new(AtomicBool::new(false));
    let wrapper = wrapper(spec, program.clone(), &reached, |_| {
        INSTALLED.get().cloned()
    });
    // While the captured executable is there, what `PATH` resolves is not consulted.
    assert_eq!(wrapper.current_or_installed().unwrap().0.program, program);
    std::fs::remove_file(&program).unwrap();
    let (identity, _) = wrapper.current_or_installed().unwrap();
    assert_eq!(identity.program, installed);
    // The account the replacement must prove is the captured one.
    assert_eq!(
        identity.principal_id,
        "sha256:".to_string() + &"0".repeat(64)
    );
    assert_eq!(identity.auth_method, "claude.ai");
}

const BROKEN_INSTALL: &str = "printf '%s\\n' 'Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall Codex: npm install -g @openai/codex@latest' >&2\nexit 1";

fn identity(spec: ProviderSpec, program: PathBuf) -> TaskProviderIdentity {
    let binary = BinaryStamp::of(&program);
    TaskProviderIdentity {
        spec,
        program,
        principal_id: "sha256:".to_string() + &"0".repeat(64),
        auth_method: "chatgpt".into(),
        org: None,
        cli_version: None,
        binary,
        probe_wall: None,
        probe_path: std::ffi::OsString::from("/usr/bin:/bin"),
        home: std::env::var_os("HOME"),
        user: std::env::var_os("USER"),
    }
}

#[test]
fn a_recheck_whose_cli_cannot_start_is_an_environment_failure_not_an_identity_one() {
    for kind in [ProviderKind::Claude, ProviderKind::Codex] {
        let directory = tempfile::tempdir().unwrap();
        let (program, spec) = fixture(kind, directory.path(), BROKEN_INSTALL);
        let checked = identity(spec, program.clone()).recheck_identity(
            Instant::now() + Duration::from_secs(30),
            &AtomicBool::new(false),
        );
        let Err(Recheck::CliCannotServe(failure @ CliRefusal::CannotStart(_))) = checked else {
            panic!("a CLI that cannot start was reported as an identity refusal");
        };
        let diagnostic = environment_failure(&failure);
        assert!(
            diagnostic.starts_with(
                "Provider environment failure, not a model or credential failure: provider fixture: Provider installation failure"
            ),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains(&program.display().to_string()),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains(": Error: Missing optional dependency @openai/codex-darwin-arm64"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.ends_with("; fix: Reinstall Codex: npm install -g @openai/codex@latest"),
            "{diagnostic}"
        );
        let refusal = admission_refusal(&failure);
        assert!(
            refusal.starts_with("Task Provider admission refused before any Worker was dispatched: provider fixture: Provider installation failure"),
            "{refusal}"
        );
    }
}

/// A CLI that answered — another account, or no login — has started, so it is never diagnosed
/// as an installation failure, even when it knows nothing about `--version`.
#[test]
fn a_recheck_that_the_cli_answered_stays_an_identity_refusal() {
    let codex_other_account = r#"if [ "$1" = app-server ]; then
  while read -r line; do
    case "$line" in
      *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
      *'"id":2'*) printf '%s\n' '{"id":2,"result":{"account":{"type":"chatgpt","email":"other@example.test"}}}' ;;
    esac
  done
  exit 0
fi
exit 64"#;
    let claude_logged_out = r#"if [ "$1" = auth ]; then
  printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'
  exit 1
fi
exit 64"#;
    for (kind, body) in [
        (ProviderKind::Codex, codex_other_account),
        (ProviderKind::Claude, claude_logged_out),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (program, spec) = fixture(kind, directory.path(), body);
        let checked = identity(spec, program).recheck_identity(
            Instant::now() + Duration::from_secs(30),
            &AtomicBool::new(false),
        );
        // Another account is an account change; no login is an identity no longer current.
        assert!(
            matches!(
                (kind, &checked),
                (ProviderKind::Codex, Err(Recheck::Changed("account")))
                    | (ProviderKind::Claude, Err(Recheck::NotCurrent))
            ),
            "an answering {} CLI was diagnosed as uninstalled or silent",
            kind.name()
        );
    }
}

/// codex-cli 0.121.0 has no `--stdio`: its `app-server` serves stdio by default and clap refuses
/// the flag with exit 2 (issue #259). The probe starts the bare app-server, so such a CLI is
/// probed successfully.
const CODEX_WITHOUT_STDIO_FLAG: &str = r#"if [ "$1" = app-server ] && [ -n "$2" ]; then
  printf '%s\n' "error: unexpected argument '$2' found" '' 'Usage: codex app-server [OPTIONS] [COMMAND]' >&2
  exit 2
fi
if [ "$1" = app-server ]; then
  while read -r line; do
    case "$line" in
      *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
      *'"id":2'*) printf '%s\n' '{"id":2,"result":{"account":{"type":"chatgpt","email":"developer@example.test"}}}' ;;
    esac
  done
  exit 0
fi
exit 64"#;

#[test]
fn the_identity_probe_starts_the_bare_app_server_a_cli_without_stdio_flag_serves() {
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(
        ProviderKind::Codex,
        directory.path(),
        CODEX_WITHOUT_STDIO_FLAG,
    );
    let (principal, method) = probe_identity(
        &program,
        &spec,
        std::ffi::OsStr::new("/usr/bin:/bin"),
        &AtomicBool::new(false),
        Some(Instant::now() + LOAD_SAFE_WALL),
    )
    .unwrap();
    assert_eq!(
        principal,
        super::principal("codex", Some("developer@example.test")).unwrap()
    );
    assert_eq!(method, "chatgpt");
}

/// An app-server that exits before answering `initialize` is reported with its exit status and
/// its own first error line, never as a missing response; when its version check says the CLI
/// starts, the refusal names the missing protocol capability, and it is never a login problem.
#[test]
fn an_app_server_that_exits_before_initialize_is_reported_with_its_exit_and_line() {
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(
        ProviderKind::Codex,
        directory.path(),
        r#"if [ "$1" = --version ]; then echo 'codex-cli 0.99.0'; exit 0; fi
if [ "$1" = app-server ]; then
  printf '%s
' "error: unrecognized subcommand 'app-server'" '' 'Usage: codex [OPTIONS] [PROMPT]' >&2
  exit 2
fi
exit 64"#,
    );
    let path = std::ffi::OsStr::new("/usr/bin:/bin");
    let error = probe_identity(&program, &spec, path, &AtomicBool::new(false), None).unwrap_err();
    assert_eq!(
        error,
        "codex app-server exited 2 before answering initialize: error: unrecognized subcommand 'app-server'"
    );
    let checked = identity(spec, program.clone())
        .recheck_identity(Instant::now() + LOAD_SAFE_WALL, &AtomicBool::new(false));
    let Err(Recheck::CliCannotServe(refusal @ CliRefusal::Incompatible(_))) = checked else {
        panic!("an app-server without the protocol was not refused as incompatible");
    };
    let refusal = admission_refusal(&refusal);
    assert_eq!(
        refusal,
        format!(
            "Task Provider admission refused before any Worker was dispatched: provider fixture: incompatible Provider CLI: the codex CLI `{}` (version codex-cli 0.99.0) does not accept af's invocation; missing: {}; codex app-server exited 2 before answering initialize: error: unrecognized subcommand 'app-server'; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`",
            program.display(),
            super::super::compatibility::APP_SERVER_PROTOCOL
        )
    );
    assert!(!refusal.contains("login"), "{refusal}");
}

/// Issue #259, round 3: an app-server that answers `initialize` with `{"id":1}` and exits proved
/// nothing; the identity probe reads it as the missing protocol capability, never as an account
/// answer or a login problem.
#[test]
fn an_app_server_answering_initialize_without_a_result_lacks_the_protocol() {
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(
        ProviderKind::Codex,
        directory.path(),
        r#"if [ "$1" = --version ]; then echo 'codex-cli 0.99.0'; exit 0; fi
if [ "$1" = app-server ]; then
  IFS= read -r _
  printf '%s\n' '{"id":1}'
  exit 0
fi
exit 64"#,
    );
    let path = std::ffi::OsStr::new("/usr/bin:/bin");
    let error = probe_identity(&program, &spec, path, &AtomicBool::new(false), None).unwrap_err();
    assert!(
        error.ends_with(r#"initialize without a result object: {"id":1}"#),
        "{error}"
    );
    let checked = identity(spec, program)
        .recheck_identity(Instant::now() + LOAD_SAFE_WALL, &AtomicBool::new(false));
    let Err(Recheck::CliCannotServe(refusal @ CliRefusal::Incompatible(_))) = checked else {
        panic!("an app-server without an initialize result was not refused as incompatible");
    };
    let refusal = admission_refusal(&refusal);
    assert!(
        refusal.contains(&format!(
            "missing: {};",
            super::super::compatibility::APP_SERVER_PROTOCOL
        )),
        "{refusal}"
    );
    assert!(!refusal.contains("login"), "{refusal}");
}

/// A Claude identity captured for `developer@example.test` at CLI version 2.1.299, whose probes
/// are bounded to `probe_wall` each.
fn claude_identity(
    spec: ProviderSpec,
    program: PathBuf,
    probe_wall: Duration,
) -> TaskProviderIdentity {
    let mut identity = identity(spec, program);
    identity.principal_id = claude_principal(&serde_json::json!({
        "loggedIn": true, "apiProvider": "firstParty", "authMethod": "claude.ai",
        "email": "developer@example.test"
    }))
    .unwrap();
    identity.auth_method = "claude.ai".into();
    identity.cli_version = Some("2.1.299 (Claude Code)".into());
    identity.probe_wall = Some(probe_wall);
    identity
}

/// The `auth status --json` answer of `email` logged in by `method`.
fn status_answer(email: &str, method: &str) -> String {
    format!(
        "if [ \"$1\" = auth ]; then printf '%s\\n' '{{\"loggedIn\":true,\"apiProvider\":\"firstParty\",\"authMethod\":\"{method}\",\"email\":\"{email}\"}}'; exit 0; fi"
    )
}

/// ADR-0202, issue #289: an identity probe that times out proves nothing, so the recheck sends it
/// again, three probes in all within the Attempt's wall, and then fails as the environment's
/// failure — never as an account change.
#[test]
fn an_identity_probe_that_times_out_is_sent_again_and_then_fails_as_the_environment() {
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(
        ProviderKind::Claude,
        directory.path(),
        "if [ \"$1\" = auth ]; then sleep 30 & wait; fi\nexit 64",
    );
    let identity = claude_identity(spec, program, Duration::from_millis(300));
    let started = Instant::now();
    let refused = identity
        .recheck_account(Instant::now() + LOAD_SAFE_WALL, &AtomicBool::new(false))
        .err()
        .expect("a silent probe is refused");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "probes were not bounded"
    );
    let Recheck::Unverified { silences, events } = &refused else {
        panic!("silence was taken for another refusal");
    };
    assert_eq!(silences, &[ProbeSilenceV1::TimedOut; 3]);
    assert_eq!(
        events,
        &[2, 3].map(|probe| ProviderEventV1::IdentityReprobed {
            cause: ProbeSilenceV1::TimedOut,
            probe,
        })
    );
    let diagnostic = refused.diagnostic(ProviderKind::Claude, identity.probe_wall);
    assert!(
        diagnostic.starts_with("identity recheck: probe timed out 3× (up to 1 s each)"),
        "{diagnostic}"
    );
    for part in [
        "the account was not verified as changed",
        "(environment)",
        "run the Task again with `af task run`",
    ] {
        assert!(diagnostic.contains(part), "{part}: {diagnostic}");
    }
    assert!(!diagnostic.contains("differs"), "{diagnostic}");
}

/// A probe that answers a line af cannot read proves nothing either: re-probed, then the
/// environment's failure.
#[test]
fn an_identity_probe_that_answers_malformed_is_sent_again_and_then_fails_as_the_environment() {
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(
        ProviderKind::Claude,
        directory.path(),
        "if [ \"$1\" = auth ]; then echo 'Loading…'; exit 0; fi\nexit 64",
    );
    let identity = claude_identity(spec, program, Duration::from_secs(20));
    let refused = identity
        .recheck_account(Instant::now() + LOAD_SAFE_WALL, &AtomicBool::new(false))
        .err()
        .expect("a malformed answer is refused");
    let Recheck::Unverified { silences, .. } = &refused else {
        panic!("a malformed answer was taken for another refusal");
    };
    assert_eq!(silences, &[ProbeSilenceV1::Malformed; 3]);
    let diagnostic = refused.diagnostic(ProviderKind::Claude, identity.probe_wall);
    // Where the host's load can be read (Linux always), the count is followed by it: "3× on a
    // host with load 2; the account …".
    assert!(
        diagnostic.starts_with("identity recheck: probe answered malformed 3×"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("; the account was not verified as changed"),
        "{diagnostic}"
    );
}

/// A real account change still fails the recheck, named as one, with the command that re-plans.
#[test]
fn another_account_still_fails_the_recheck_as_an_account_change() {
    let directory = tempfile::tempdir().unwrap();
    let body = format!(
        "{}\nexit 64",
        status_answer("other@example.test", "claude.ai")
    );
    let (program, spec) = fixture(ProviderKind::Claude, directory.path(), &body);
    let identity = claude_identity(spec, program, Duration::from_secs(20));
    let refused = identity
        .recheck_account(Instant::now() + LOAD_SAFE_WALL, &AtomicBool::new(false))
        .err()
        .expect("another account is refused");
    assert!(matches!(refused, Recheck::Changed("account")));
    assert_eq!(
        refused.diagnostic(ProviderKind::Claude, None),
        "identity recheck: the Claude account differs from the one captured at plan: run \
         `af task plan` again"
    );
}

/// ADR-0202, the auto-update rule: a CLI that updated itself — a new version, the same account,
/// its method spelled `oauth` where the capture spelled `claude.ai` — passes the recheck. The
/// update is one Provider event, and the capture follows the new version, so it is recorded once.
#[test]
fn a_cli_update_with_the_same_account_passes_and_the_capture_follows_it() {
    let directory = tempfile::tempdir().unwrap();
    let body = format!(
        "[ \"$1\" = --version ] && {{ echo '2.1.300 (Claude Code)'; exit 0; }}\n{}\nexit 64",
        status_answer("developer@example.test", "oauth")
    );
    let (program, spec) = fixture(ProviderKind::Claude, directory.path(), &body);
    let mut identity = claude_identity(spec, program, Duration::from_secs(20));
    // The capture read another executable file: the launcher was repointed since.
    identity.binary = None;
    let rechecked = identity
        .recheck_account(Instant::now() + LOAD_SAFE_WALL, &AtomicBool::new(false))
        .unwrap_or_else(|_| panic!("an update alone was refused"));
    assert_eq!(
        rechecked.events,
        [ProviderEventV1::IdentityRefreshed {
            provider: "fixture".into(),
            previous_version: "2.1.299 (Claude Code)".into(),
            current_version: "2.1.300 (Claude Code)".into(),
        }]
    );
    let reached = std::sync::Arc::new(AtomicBool::new(false));
    let adapter = CurrentTaskProviderAdapter {
        model: "claude-fixture-1".into(),
        effort: "high".into(),
        resolve: resolve_program,
        current: Mutex::new((
            identity.clone(),
            Arc::new(Recording {
                reached: reached.clone(),
            }),
        )),
    };
    adapter.follow(&identity, &rechecked);
    let followed = adapter.current.lock().unwrap().0.clone();
    assert_eq!(
        followed.cli_version.as_deref(),
        Some("2.1.300 (Claude Code)")
    );
    // Rechecked again with the capture it follows, the update is not reported twice.
    let again = followed
        .recheck_account(Instant::now() + LOAD_SAFE_WALL, &AtomicBool::new(false))
        .unwrap_or_else(|_| panic!("the followed capture was refused"));
    assert!(again.events.is_empty());
    // Capture starts no `--version` process, so the first update names no earlier version.
    let mut fresh = identity.clone();
    fresh.cli_version = None;
    let rechecked = fresh
        .recheck_account(Instant::now() + LOAD_SAFE_WALL, &AtomicBool::new(false))
        .unwrap_or_else(|_| panic!("an update alone was refused"));
    assert_eq!(
        rechecked.events,
        [ProviderEventV1::IdentityRefreshed {
            provider: "fixture".into(),
            previous_version: "unknown".into(),
            current_version: "2.1.300 (Claude Code)".into(),
        }]
    );
}
