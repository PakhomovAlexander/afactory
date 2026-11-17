//! A client exit or a missing name cannot fence an in-flight daemon create request.
//! Keep creation alive through settlement, then start and remove only its acknowledged ID.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct Creation {
    pub capture: SupervisedCapture,
    pub id: Option<String>,
    pub cleanup_deadline: Option<Instant>,
}

pub(super) fn create(
    command: &mut std::process::Command,
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
) -> Creation {
    if Instant::now() >= deadline || cancelled(cancellation) {
        let mut capture = empty_capture(SupervisedError::Cancelled);
        refusal(&mut capture, deadline, cancellation);
        return Creation {
            capture,
            id: None,
            cleanup_deadline: Some(Instant::now() + CLEANUP_TIMEOUT),
        };
    }
    let stop = AtomicBool::new(false);
    let mut cleanup_deadline = None;
    let capture = std::thread::scope(|scope| {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let stop = &stop;
        scope.spawn(move || {
            let capture = af_process::run_supervised_captured_bounded_until(
                command,
                None,
                deadline + CLEANUP_TIMEOUT,
                ExitPolicy::KillDescendants,
                Some(stop),
                16 * 1024,
            );
            let _ = send.send(capture);
        });
        loop {
            let now = Instant::now();
            if cleanup_deadline.is_none() && (now >= deadline || cancelled(cancellation)) {
                cleanup_deadline =
                    Some(if now >= deadline { deadline } else { now } + CLEANUP_TIMEOUT);
            }
            if cleanup_deadline.is_some_and(|end| now >= end) {
                stop.store(true, Ordering::Release);
            }
            match receive.recv_timeout(Duration::from_millis(10)) {
                Ok(capture) => break capture,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(error) => panic!("creation supervisor disconnected: {error}"),
            }
        }
    });
    if cleanup_deadline.is_none() && (Instant::now() >= deadline || cancelled(cancellation)) {
        cleanup_deadline = Some(
            if Instant::now() >= deadline {
                deadline
            } else {
                Instant::now()
            } + CLEANUP_TIMEOUT,
        );
    }
    let id = if capture.status.as_ref().is_ok_and(|status| status.success())
        && !capture.stderr_held
        && !capture.truncation.is_truncated()
    {
        std::str::from_utf8(&capture.stdout)
            .ok()
            .map(str::trim)
            .filter(|id| valid_id(id))
            .map(str::to_owned)
    } else {
        None
    };
    Creation {
        capture,
        id,
        cleanup_deadline,
    }
}

pub(super) fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub(super) fn cancelled(flag: Option<&AtomicBool>) -> bool {
    flag.is_some_and(|flag| flag.load(Ordering::Acquire))
}

pub(super) fn create_args(mut argv: Vec<String>) -> Vec<String> {
    argv[0] = "create".into();
    if argv.get(1).is_some_and(|arg| arg == "--rm") {
        argv.remove(1);
    }
    argv
}

pub(super) fn remove(
    runtime: &Path,
    id: &str,
    home: Option<&Path>,
    deadline: Instant,
) -> Result<(), String> {
    if !valid_id(id) {
        return Err("unacknowledged container identity".into());
    }
    let mut command = std::process::Command::new(runtime);
    command.args(["rm", "-f", id]);
    if let Some(home) = home {
        private_client(&mut command, home);
    }
    let execution = af_process::run_supervised_captured_bounded_until(
        &mut command,
        None,
        deadline.min(Instant::now() + PROBE_TIMEOUT),
        ExitPolicy::KillDescendants,
        None,
        4096,
    );
    if execution
        .status
        .as_ref()
        .is_ok_and(|status| status.success())
        && !execution.stderr_held
        && !execution.truncation.is_truncated()
    {
        return Ok(());
    }
    // Creation has settled and the durable journal binds the full ID. A complete successful
    // same-daemon listing can therefore close the rm-to-journal-deletion crash window.
    // Failed inspect (including a daemon failure) would not establish absence.
    let mut probe = std::process::Command::new(runtime);
    probe.args([
        "ps",
        "--all",
        "--no-trunc",
        "--quiet",
        "--filter",
        &format!("id={id}"),
    ]);
    if let Some(home) = home {
        private_client(&mut probe, home);
    }
    let capture = af_process::run_supervised_captured_bounded_until(
        &mut probe,
        None,
        deadline.min(Instant::now() + PROBE_TIMEOUT),
        ExitPolicy::KillDescendants,
        None,
        4096,
    );
    if capture.status.as_ref().is_ok_and(|status| status.success())
        && !capture.stderr_held
        && !capture.truncation.is_truncated()
        && capture.stdout.iter().all(u8::is_ascii_whitespace)
    {
        Ok(())
    } else {
        Err("container identity removal or exact-ID absence was not confirmed".into())
    }
}

/// Creation output is setup evidence, never payload output. If setup fails, retain its actual
/// bytes and truncation while refusing execution. Acknowledgement after expiry never starts work.
pub(super) fn refusal(
    capture: &mut SupervisedCapture,
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
) {
    if cancelled(cancellation) {
        capture.status = Err(SupervisedError::Cancelled);
    } else if Instant::now() >= deadline {
        capture.status = Err(SupervisedError::TimedOut {
            stdout: capture.stdout.clone(),
            stderr: capture.stderr.clone(),
            pid: capture.pid,
            rusage: capture.rusage,
        });
    } else if capture.status.is_ok() {
        capture.status = Err(SupervisedError::Wait(std::io::Error::other(
            "container creation did not acknowledge an exact identity",
        )));
    }
}

fn empty_capture(error: SupervisedError) -> SupervisedCapture {
    SupervisedCapture {
        status: Err(error),
        stdout: vec![],
        stderr: vec![],
        stderr_held: false,
        pid: None,
        rusage: None,
        truncation: af_process::Truncation::default(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn launch(
    runtime: &Path,
    argv: Vec<String>,
    home: Option<&Path>,
    input: Option<Vec<u8>>,
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
    cap: usize,
    journal: Option<&Path>,
    detached: bool,
    exit_policy: ExitPolicy,
) -> (SupervisedCapture, bool, Instant) {
    launch_after_ack(
        runtime,
        argv,
        home,
        input,
        deadline,
        cancellation,
        cap,
        journal,
        detached,
        exit_policy,
        || {},
    )
}

#[allow(clippy::too_many_arguments)]
fn launch_after_ack(
    runtime: &Path,
    argv: Vec<String>,
    home: Option<&Path>,
    input: Option<Vec<u8>>,
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
    cap: usize,
    journal: Option<&Path>,
    detached: bool,
    exit_policy: ExitPolicy,
    after_ack: impl FnOnce(),
) -> (SupervisedCapture, bool, Instant) {
    if Instant::now() >= deadline || cancelled(cancellation) {
        let mut capture = empty_capture(SupervisedError::Cancelled);
        refusal(&mut capture, deadline, cancellation);
        return (capture, true, Instant::now() + CLEANUP_TIMEOUT);
    }
    let mut command = std::process::Command::new(runtime);
    command.args(create_args(argv));
    if let Some(home) = home {
        private_client(&mut command, home);
    }
    // A durable empty file means create may still commit. Only the actual acknowledged full
    // daemon ID replaces it. A crash in either window therefore refuses recovery.
    if let Some(path) = journal {
        if let Err(error) = write_intent(path, b"") {
            let capture = empty_capture(SupervisedError::Wait(error));
            return (capture, false, Instant::now());
        }
    }
    let mut creation = create(&mut command, deadline, cancellation);
    if let (Some(path), Some(id)) = (journal, creation.id.as_ref()) {
        if write_ack(path, id.as_bytes()).is_err() {
            creation.id = None;
        }
    }
    after_ack();
    let mut capture = if creation.id.is_some() {
        let mut start = std::process::Command::new(runtime);
        start.arg("start");
        if !detached {
            start.args(["--attach", "--interactive"]);
        }
        start.arg(creation.id.as_ref().unwrap());
        if let Some(home) = home {
            private_client(&mut start, home);
        }
        af_process::run_supervised_captured_bounded_until(
            &mut start,
            input,
            deadline,
            exit_policy,
            cancellation,
            cap,
        )
    } else {
        refusal(&mut creation.capture, deadline, cancellation);
        creation.capture
    };
    let cleanup_deadline = creation
        .cleanup_deadline
        .unwrap_or_else(|| (Instant::now() + CLEANUP_TIMEOUT).min(deadline + CLEANUP_TIMEOUT));
    if detached
        && capture.status.as_ref().is_ok_and(|status| status.success())
        && !capture.stderr_held
    {
        return (capture, true, cleanup_deadline);
    }
    let clean = match creation.id {
        Some(id) => remove(runtime, &id, home, cleanup_deadline).is_ok(),
        None => matches!(capture.status, Err(SupervisedError::Spawn(_))),
    };
    if clean {
        if let Some(path) = journal {
            if let Err(error) = std::fs::remove_file(path) {
                capture.status = Err(SupervisedError::Wait(error));
                return (capture, false, Instant::now());
            }
        }
    }
    (capture, clean, cleanup_deadline)
}

pub(super) fn write_intent(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::File::open(path.parent().unwrap())?.sync_all()
}

pub(super) fn write_ack(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn expiry_or_cancellation_after_durable_ack_refuses_start_at_launch_boundary() {
        for cancel in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let runtime = root.path().join("runtime");
            let journal = root.path().join("worker.cid");
            super::super::tests::write_runtime(
                &runtime,
                &format!(
                    r#"#!/bin/sh
case "$1" in
create) printf '{ID}';;
start) touch "$0.started";;
rm) exit 0;;
*) exit 91;;
esac
"#
                ),
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            let flag = AtomicBool::new(false);
            let (capture, clean, _) = launch_after_ack(
                &runtime,
                vec!["run".into()],
                None,
                None,
                deadline,
                Some(&flag),
                4096,
                Some(&journal),
                false,
                ExitPolicy::KillDescendants,
                || {
                    assert_eq!(std::fs::read_to_string(&journal).unwrap(), ID);
                    if cancel {
                        flag.store(true, Ordering::Release);
                    } else {
                        let (_send, receive) = std::sync::mpsc::channel::<()>();
                        assert!(matches!(
                            receive
                                .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        ));
                    }
                },
            );
            assert!(capture.status.is_err());
            assert!(capture.pid.is_none(), "no start process admitted");
            assert!(clean);
            assert!(!runtime.with_extension("started").exists());
            assert!(!journal.exists());
        }
    }

    // FIFO handshakes put the daemon commit strictly after the apparent name removal and
    // cancellation. No elapsed-time guess determines which side of the race we exercise.
    #[test]
    fn delayed_create_is_settled_before_exact_removal_and_never_starts_after_cancellation() {
        for worker in [false, true] {
            for acknowledge in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let runtime = root.path().join("runtime");
                for name in ["ready", "release"] {
                    nix::unistd::mkfifo(
                        &root.path().join(name),
                        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
                    )
                    .unwrap();
                }
                super::super::tests::write_runtime(
                    &runtime,
                    &format!(
                        r#"#!/bin/sh
cd "$(dirname "$0")" || exit 90
case "$1" in
info) exit 0;;
create)
  printf ready > ready
  read -r signal < release
  printf committed > daemon
  if [ "{acknowledge}" = true ]; then printf '{ID}'; exit 0; fi
  printf 'acknowledgement lost' >&2; exit 7;;
start) printf started > started; exit 0;;
rm)
  printf '%s\n' "$3" >> removals
  if [ "$3" = '{ID}' ]; then rm -f daemon; fi
  exit 0;;
ps) exit 0;;
esac
"#
                    ),
                );
                let flag = AtomicBool::new(false);
                std::thread::scope(|scope| {
                    scope.spawn(|| {
                        let mut ready = String::new();
                        std::fs::File::open(root.path().join("ready"))
                            .unwrap()
                            .read_to_string(&mut ready)
                            .unwrap();
                        assert_eq!(ready, "ready");
                        // Model the old helper's misleading rm/absence result while daemon
                        // creation is blocked. This must grant the new lifecycle no authority.
                        let output = std::process::Command::new(&runtime)
                            .args(["rm", "-f", "af-gate-race"])
                            .output()
                            .unwrap();
                        assert!(output.status.success());
                        assert!(!root.path().join("daemon").exists());
                        flag.store(true, Ordering::Release);
                        std::fs::File::create(root.path().join("release"))
                            .unwrap()
                            .write_all(b"commit\n")
                            .unwrap();
                    });
                    let provider = ContainerProvider::with_runtime(&runtime);
                    if worker {
                        std::fs::create_dir(root.path().join("package")).unwrap();
                        std::fs::create_dir(root.path().join("empty")).unwrap();
                        let capture = provider
                            .with_worker_layout(WorkerContainerLayout {
                                package: root.path().join("package"),
                                roots: vec![],
                                source_writable: true,
                                base: None,
                                processes: 32,
                                disk_bytes: 4096,
                                probe_shell: "/bin/sh".into(),
                            })
                            .with_identity_probe_root(&root.path().join("empty"))
                            .exec_worker_captured(
                                root.path(),
                                "/bin/true",
                                &[],
                                &[],
                                vec![],
                                Instant::now() + Duration::from_secs(120),
                                Some(&flag),
                            )
                            .unwrap();
                        assert_eq!(capture.cleanup_confirmed, acknowledge);
                        assert_eq!(capture.filesystem_exported, acknowledge);
                        assert!(matches!(
                            capture.capture.status,
                            Err(SupervisedError::Cancelled)
                        ));
                        if !acknowledge {
                            assert_eq!(capture.capture.stderr, b"acknowledgement lost");
                        }
                    } else {
                        let error = provider
                            .exec_evidenced_controlled(
                                root.path(),
                                "/bin/true",
                                &[],
                                &[],
                                Duration::from_secs(120),
                                Some(&flag),
                            )
                            .unwrap_err();
                        assert_eq!(error.cleanup_confirmed(), acknowledge, "{error}");
                    }
                });
                assert!(!root.path().join("started").exists());
                assert_eq!(root.path().join("daemon").exists(), !acknowledge);
                let removals = std::fs::read_to_string(root.path().join("removals")).unwrap();
                assert_eq!(
                    removals,
                    if acknowledge {
                        format!("af-gate-race\n{ID}\n")
                    } else {
                        "af-gate-race\n".into()
                    }
                );
            }
        }
    }

    #[test]
    fn cancelled_worker_keeps_cancellation_when_final_export_is_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        for name in ["ready", "release"] {
            nix::unistd::mkfifo(
                &root.path().join(name),
                nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
            )
            .unwrap();
        }
        for name in ["source", "package", "home"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        std::fs::write(root.path().join("source/original"), b"source").unwrap();
        super::super::tests::write_runtime(
            &runtime,
            &format!(
                r#"#!/bin/sh
cd "$(dirname "$0")" || exit 90
case "$1" in
info|rm|volume) exit 0;;
create)
  for arg do
    case "$arg" in
      *-keeper) printf '{keeper}'; exit 0;;
      *-seed) printf '{seed}'; exit 0;;
      *-worker) printf '{ID}'; exit 0;;
    esac
  done
  printf unexpected > unexpected-export; exit 9;;
start)
  for arg do
    if [ "$arg" = '{ID}' ]; then
      printf ready > ready
      read -r signal < release
    fi
  done
  exit 0;;
esac
"#,
                keeper = "a".repeat(64),
                seed = "c".repeat(64)
            ),
        );
        let provider =
            ContainerProvider::with_runtime(&runtime).with_worker_layout(WorkerContainerLayout {
                package: root.path().join("package"),
                roots: vec![],
                source_writable: true,
                base: None,
                processes: 32,
                disk_bytes: 4096,
                probe_shell: "/bin/sh".into(),
            });
        let flag = AtomicBool::new(false);
        let capture = std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut ready = String::new();
                std::fs::File::open(root.path().join("ready"))
                    .unwrap()
                    .read_to_string(&mut ready)
                    .unwrap();
                assert_eq!(ready, "ready");
                flag.store(true, Ordering::Release);
            });
            provider
                .exec_worker_captured(
                    &root.path().join("source"),
                    "/bin/true",
                    &[],
                    &[],
                    vec![],
                    Instant::now() + Duration::from_secs(120),
                    Some(&flag),
                )
                .unwrap()
        });
        assert!(matches!(
            capture.capture.status,
            Err(SupervisedError::Cancelled)
        ));
        assert!(capture.cleanup_confirmed);
        assert!(!capture.filesystem_exported);
        assert!(!root.path().join("unexpected-export").exists());
        assert!(!root.path().join("worker-storage-v2.json").exists());
        assert_eq!(
            std::fs::read(root.path().join("source/original")).unwrap(),
            b"source"
        );
    }

    #[test]
    fn expired_deadline_refuses_creation_and_payload_without_uncertainty() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        super::super::tests::write_runtime(
            &runtime,
            "#!/bin/sh\nif [ \"$1\" = info ]; then exit 0; fi\nprintf unexpected > \"$0.called\"\nexit 0\n",
        );
        let provider = ContainerProvider::with_runtime(&runtime);
        let error = provider
            .exec_evidenced_controlled(root.path(), "/bin/true", &[], &[], Duration::ZERO, None)
            .unwrap_err();
        assert!(error.cleanup_confirmed());
        assert!(error.to_string().contains("did not finish"));
        assert!(!runtime.with_extension("called").exists());
    }

    #[test]
    fn create_options_preserve_payload_arguments_that_look_like_runtime_flags() {
        let argv = [
            "run", "--rm", "--name", "owned", "image", "program", "--rm", "--detach",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            create_args(argv),
            [
                "create", "--name", "owned", "image", "program", "--rm", "--detach"
            ]
        );
    }
}
