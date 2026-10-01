use super::*;

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

#[test]
fn identity_rechecks_use_remaining_attempt_deadline_and_prespawn_cancellation() {
    for kind in [ProviderKind::Claude, ProviderKind::Codex] {
        let directory = tempfile::tempdir().unwrap();
        let (program, spec) = fixture(
            kind,
            directory.path(),
            "auth=${CLAUDE_CONFIG_DIR:-$CODEX_HOME}\nprintf '%s' $$ >\"$auth/ready\"\nsleep 30 &\nwait",
        );
        let flag = AtomicBool::new(true);
        let path = std::ffi::OsStr::new("/usr/bin:/bin");
        let deadline = Instant::now() + Duration::from_secs(2);
        assert!(probe_identity(&program, &spec, path, &flag, Some(deadline)).is_err());
        assert!(
            !directory.path().join("ready").exists(),
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
            let pid: i32 = text.parse().unwrap();
            assert!(
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err(),
                "status leader was not reaped"
            );
            std::fs::remove_file(directory.path().join("ready")).unwrap();
        }
        assert!(probe_identity(&program, &spec, path, &flag, Some(deadline)).is_err());
        assert!(
            !directory.path().join("ready").exists(),
            "expired check spawned"
        );
    }
}

#[test]
fn identity_rechecks_cancel_an_inflight_status_process() {
    for kind in [ProviderKind::Claude, ProviderKind::Codex] {
        let directory = tempfile::tempdir().unwrap();
        let (program, spec) = fixture(
            kind,
            directory.path(),
            "auth=${CLAUDE_CONFIG_DIR:-$CODEX_HOME}\nprintf '%s' $$ >\"$auth/ready\"\nsleep 30 &\nwait",
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
            message: Ok(b"{}".to_vec()),
            usage: None,
            usage_observation: None,
            raw_artifact_ids: vec![],
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
    let identity = TaskProviderIdentity {
        home: spec.auth_dir.clone().map(Into::into),
        user: Some("fixture".into()),
        spec,
        program,
        principal_id: "sha256:".to_string() + &"0".repeat(64),
        auth_method: "claude.ai".into(),
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
        Duration::from_secs(5),
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
