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
                let deadline = Instant::now() + Duration::from_secs(3);
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
                Some(Instant::now() + Duration::from_secs(4)),
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

#[test]
fn sandbox_environment_passes_the_identity_recheck_before_it_can_reach_the_native_client() {
    let directory = tempfile::tempdir().unwrap();
    let (program, spec) = fixture(ProviderKind::Claude, directory.path(), "exit 0");
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let reached = std::sync::Arc::new(AtomicBool::new(false));
    // A fixture provider is not in the machine-local registry, so the recheck fails.
    let wrapper = CurrentTaskProviderAdapter {
        identity: TaskProviderIdentity {
            spec,
            program,
            principal_id: "sha256:".to_string() + &"0".repeat(64),
            auth_method: "claude.ai".into(),
            probe_path: sanitized_path(),
            home: std::env::var_os("HOME"),
            user: std::env::var_os("USER"),
        },
        inner: Box::new(Recording {
            reached: reached.clone(),
        }),
    };
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
