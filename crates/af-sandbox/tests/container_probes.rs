//! The three containment probes that need real isolation, run against a live daemon.
//!
//! The record of which probes are discharged is `docs/security/containment-probes.md`.
//! `trusted_local` could honestly answer three of the five probes; these are the other
//! two plus the absolute-write half of the checkout probe — the ones where "nothing stops a
//! process" was the whole finding. Here something does: the container has one bind (the
//! sandbox), no network, and no inherited environment, so each probe fails at a boundary
//! rather than by convention.
//!
//! Every live probe is `#[ignore]` because the ordinary test run must not depend on a container
//! runtime. They are *not* optional where they do run: `make review-kernel-container-probes`
//! (locally and in CI) invokes them explicitly, and a missing daemon is then a hard failure,
//! never a skip — an unrun probe that reports nothing is how a guarantee quietly stops being
//! tested.
//!
//! Each escape probe is paired with the control at the bottom, which proves the same provider
//! does run work and does land writes in the sandbox — so the probes fail for isolation
//! reasons, not because the container is broken.

use af_check::{Arg, CheckDefinition, CheckRunner, CheckStatus, Command};
use af_sandbox::{Availability, ContainerProvider, Mode, Sandbox};
use af_source_git::Capture;
use std::path::Path;
use std::time::{Duration, Instant};

mod common;
use common::fixture_repo;

const EXEC_TIMEOUT: Duration = Duration::from_secs(60);

/// Invoked explicitly means required: refusal here is a failure, not a skip.
fn provider() -> ContainerProvider {
    let provider = ContainerProvider::detect();
    assert!(
        matches!(provider.availability(), Availability::Usable { .. }),
        "these probes were invoked explicitly and need a live runtime: {}",
        provider.availability().reason()
    );
    provider
}

/// One contained command with no extra environment, under the probe deadline.
fn run(
    provider: &ContainerProvider,
    sandbox_root: &Path,
    program: &str,
    args: &[String],
) -> std::process::Output {
    provider
        .exec_evidenced_controlled(sandbox_root, program, args, &[], EXEC_TIMEOUT, None)
        .map(|execution| execution.output)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// Probe: host marker. A file planted outside the sandbox is unreadable and unmodifiable —
/// the absolute path simply names nothing inside the container.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn a_host_marker_is_out_of_reach() {
    let provider = provider();
    let host = tempfile::tempdir().unwrap();
    let marker = host.path().join("review-host-marker");
    std::fs::write(&marker, "untouched").unwrap();
    let sandbox = tempfile::tempdir().unwrap();

    let read = run(
        &provider,
        sandbox.path(),
        "/bin/cat",
        &[marker.display().to_string()],
    );
    assert!(!read.status.success(), "the marker must be unreadable");
    assert!(
        !String::from_utf8_lossy(&read.stdout).contains("untouched"),
        "no marker bytes may cross the boundary"
    );

    let write = run(
        &provider,
        sandbox.path(),
        "/bin/sh",
        &[
            "-c".to_string(),
            format!("echo pwned > {}", marker.display()),
        ],
    );
    assert!(!write.status.success(), "the marker must be unwritable");
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "untouched",
        "and unmodified on the host"
    );

    // The Worker extension uses this same containment boundary, with distinct private state
    // mounts. Run two slots concurrently: neither receives its sibling's host directory.
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for root in [first.path(), second.path()] {
        for name in ["source", "state", "package"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("state/private"), "initial").unwrap();
    }
    std::thread::scope(|scope| {
        let slots: Vec<_> = [(first.path(), second.path(), "first"), (second.path(), first.path(), "second")]
            .into_iter().map(|(own, sibling, value)| {
                let provider = provider.clone().with_limits(Some("134217728".into()), Some("1".into()))
                    .with_output_cap(4096)
                    .with_worker_layout(af_sandbox::WorkerContainerLayout {
                        package: own.join("package"), roots: vec![(own.join("state"), "state".into(), true)],
                        source_writable: true, base: None, processes: 32,
                disk_bytes: 16 * 1024 * 1024,
                probe_shell: "/bin/sh".into(),
                    });
                let marker = marker.clone();
                scope.spawn(move || {
                    let capture = provider.exec_worker_captured(
                        &own.join("source"), "/bin/sh",
                        &["-c".into(), "set -eu; read -r value; test ! -r \"$HOST_MARKER\"; test ! -r \"$SIBLING\"; printf '%s\\n' \"$value\" > /af/state/private; read -r retained < /af/state/private; test \"$value\" = \"$retained\"; printf '%s' \"$retained\"".into()],
                        &[("HOST_MARKER".into(), marker.to_string_lossy().into()), ("SIBLING".into(), sibling.join("state/private").to_string_lossy().into())],
                        format!("{value}\n").into_bytes(), Instant::now()+EXEC_TIMEOUT, None,
                    ).unwrap();
                    assert!(capture.cleanup_confirmed, "slot {value}: daemon cleanup unconfirmed");
                    assert!(
                        capture.capture.status.as_ref().is_ok_and(|status| status.success()),
                        "slot {value}: status={:?}, stderr_held={}, truncation={:?}, stdout={:?}, stderr={:?}",
                        capture.capture.status, capture.capture.stderr_held, capture.capture.truncation,
                        String::from_utf8_lossy(&capture.capture.stdout),
                        String::from_utf8_lossy(&capture.capture.stderr),
                    );
                    assert_eq!(capture.capture.stdout, value.as_bytes());
                })
            }).collect();
        for slot in slots {
            slot.join().unwrap();
        }
    });
    assert_eq!(
        std::fs::read_to_string(first.path().join("state/private")).unwrap(),
        "first\n"
    );
    assert_eq!(
        std::fs::read_to_string(second.path().join("state/private")).unwrap(),
        "second\n"
    );
}

/// All declared roots and auxiliary writable directories compete for the same eight MiB.
/// Each individual write fits; only their aggregate exceeds the bound. Another Worker runs
/// concurrently, and the host receives only a bounded export after the workload is gone.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn worker_aggregate_disk_bound_stops_writes_across_all_roots_and_protects_a_sibling() {
    let provider = provider();
    let host = tempfile::tempdir().unwrap();
    let marker = host.path().join("host-marker");
    std::fs::write(&marker, b"untouched").unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let layout = |root: &Path| {
        for name in [
            "source",
            "package",
            "state",
            "scratch",
            "build",
            "cache",
            "knowledge",
        ] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("source/original"), b"exact input").unwrap();
        std::fs::create_dir(root.join("base")).unwrap();
        std::fs::write(root.join("base/private"), b"immutable base").unwrap();
        af_sandbox::WorkerContainerLayout {
            package: root.join("package"),
            roots: ["state", "scratch", "build", "cache", "knowledge"]
                .into_iter()
                .map(|name| (root.join(name), name.into(), true))
                .collect(),
            source_writable: true,
            base: Some(root.join("base")),
            processes: 32,
            disk_bytes: 8 * 1024 * 1024,
            probe_shell: "/bin/sh".into(),
        }
    };
    let first_provider = provider
        .clone()
        .with_limits(Some("134217728".into()), Some("1".into()))
        .with_worker_layout(layout(first.path()));
    let second_provider = provider
        .with_limits(Some("134217728".into()), Some("1".into()))
        .with_worker_layout(layout(second.path()));
    first_provider
        .admit_worker_image(
            first.path(),
            "/bin/sh",
            &["/bin/sh".into()],
            Instant::now() + EXEC_TIMEOUT,
            None,
        )
        .unwrap();
    let script = r#"
set -eu
test "$(id -u)" != 0
test ! -w /dev
test ! -w /dev/mqueue
test ! -w /etc/hosts
test ! -e "$HOST_MARKER"
test ! -w /base/private
if ln /base/private /af/state/alias 2>/dev/null; then exit 93; fi
long=$(printf '%0200d' 1)
mkdir /af/build/"$long"
printf 'long path preserved' > /af/build/"$long"/proof
device=$(stat -c %d /work)
for p in /work /af/state /af/scratch /af/build /af/cache /af/knowledge /tmp /run /dev/shm; do
    test "$(stat -c %d "$p")" = "$device"
done
for p in /work /af/state /af/scratch /af/build /af/cache /af/knowledge; do
    dd if=/dev/zero of="$p/allocated" bs=1048576 count=1 2>/dev/null
done
for p in /tmp /run /dev/shm; do
    dd if=/dev/zero of="$p/allocated" bs=524288 count=1 2>/dev/null
done
if refusal=$(dd if=/dev/zero of=/af/build/excess bs=1048576 count=2 2>&1); then
    echo 'unbounded write escaped aggregate quota'; exit 91
fi
case "$refusal" in *'No space left on device'*) ;; *) exit 92;; esac
test "$(cat /work/original)" = 'exact input'
printf 'aggregate quota enforced'
"#;
    std::thread::scope(|scope| {
        let attacker = scope.spawn(|| {
            first_provider
                .exec_worker_captured(
                    &first.path().join("source"),
                    "/bin/sh",
                    &["-c".into(), script.into()],
                    &[("HOST_MARKER".into(), marker.to_string_lossy().into())],
                    vec![],
                    Instant::now() + EXEC_TIMEOUT,
                    None,
                )
                .unwrap()
        });
        let sibling = scope.spawn(|| second_provider.exec_worker_captured(
            &second.path().join("source"), "/bin/sh", &["-c".into(), "set -eu; test ! -e /af/build/allocated; printf 'independent' > /af/state/private; printf 'sibling succeeded'".into()],
            &[], vec![], Instant::now()+EXEC_TIMEOUT, None).unwrap());
        for (capture, expected) in [
            (attacker.join().unwrap(), "aggregate quota enforced"),
            (sibling.join().unwrap(), "sibling succeeded"),
        ] {
            assert!(capture.cleanup_confirmed);
            assert!(
                capture
                    .capture
                    .status
                    .as_ref()
                    .is_ok_and(|status| status.success()),
                "{:?}: {}",
                capture.capture.status,
                String::from_utf8_lossy(&capture.capture.stderr)
            );
            assert_eq!(capture.capture.stdout, expected.as_bytes());
        }
    });
    assert_eq!(std::fs::read(&marker).unwrap(), b"untouched");
    assert_eq!(
        std::fs::read(second.path().join("state/private")).unwrap(),
        b"independent"
    );
    assert!(!second.path().join("build/allocated").exists());
    assert_eq!(
        std::fs::read(first.path().join("base/private")).unwrap(),
        b"immutable base"
    );
    assert_eq!(
        std::fs::read(
            first
                .path()
                .join("build")
                .join(format!("{:0200}", 1))
                .join("proof")
        )
        .unwrap(),
        b"long path preserved"
    );
    let mut exported = 0;
    for name in ["source", "state", "scratch", "build", "cache", "knowledge"] {
        for file in std::fs::read_dir(first.path().join(name)).unwrap() {
            exported += file.unwrap().metadata().unwrap().len();
        }
    }
    assert!(exported <= 8 * 1024 * 1024);
    for root in [first.path(), second.path()] {
        assert!(
            !root.join("worker-storage-v1.json").exists(),
            "confirmed cleanup removes the recovery intent"
        );
    }
}

#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn sparse_worker_output_is_refused_before_any_unbounded_host_export() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["source", "package"] {
        std::fs::create_dir(directory.path().join(name)).unwrap();
    }
    std::fs::write(directory.path().join("source/original"), b"unchanged").unwrap();
    let provider = provider()
        .with_limits(Some("134217728".into()), Some("1".into()))
        .with_worker_layout(af_sandbox::WorkerContainerLayout {
            package: directory.path().join("package"),
            roots: vec![],
            source_writable: true,
            base: None,
            processes: 32,
            disk_bytes: 1024 * 1024,
            probe_shell: "/bin/sh".into(),
        });
    let result = provider.exec_worker_captured(&directory.path().join("source"), "/bin/sh", &["-c".into(), "dd if=/dev/zero of=huge bs=1 count=0 seek=1073741824 2>/dev/null; printf 'worker finished'".into()], &[], vec![], Instant::now()+EXEC_TIMEOUT, None).unwrap();
    assert!(
        result.capture.status.is_err(),
        "sparse logical bytes must not be exported beyond disk_bytes"
    );
    assert!(result.cleanup_confirmed);
    assert!(
        !result.filesystem_exported,
        "unavailable final filesystem evidence cannot be admitted"
    );
    assert_eq!(
        std::fs::read(directory.path().join("source/original")).unwrap(),
        b"unchanged"
    );
    assert!(!directory.path().join("source/huge").exists());
    assert!(!directory.path().join("worker-storage-v1.json").exists());
}

const STORAGE_CRASH_ROOT: &str = "AF_STORAGE_CRASH_FIXTURE_ROOT";
const STORAGE_CRASH_IMAGE: &str = "AF_STORAGE_CRASH_FIXTURE_IMAGE";

/// Worker storage deliberately uses --pull=never. Prepare the multi-architecture pin with
/// the parent's actual Engine before starting the child or its storage-barrier deadline.
fn prepare_storage_crash_image(engine: &Path, image: &str) {
    let mut inspect = std::process::Command::new(engine);
    inspect.args(["image", "inspect", image]);
    let present = af_process::run_supervised(&mut inspect, None, EXEC_TIMEOUT).unwrap();
    if !present.status.success() {
        let mut pull = std::process::Command::new(engine);
        pull.args(["pull", image]);
        let prepared = af_process::run_supervised(&mut pull, None, EXEC_TIMEOUT).unwrap();
        assert!(
            prepared.status.success(),
            "prepare crash fixture image {image}: {}",
            String::from_utf8_lossy(&prepared.stderr)
        );
    }
}

fn storage_crash_provider(root: &Path, runtime: &Path, image: &str) -> ContainerProvider {
    ContainerProvider::with_runtime(root.join("runtime-wrapper"))
        .with_image(image)
        .with_limits(Some("134217728".into()), Some("1".into()))
        .with_worker_layout(af_sandbox::WorkerContainerLayout {
            package: runtime.join("package"),
            roots: vec![],
            source_writable: true,
            base: None,
            processes: 32,
            disk_bytes: 1024 * 1024,
            probe_shell: "/bin/sh".into(),
        })
}

#[test]
fn storage_crash_fixture_prepares_and_forwards_the_exact_image_without_a_cache() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let engine = root.join("engine");
    // No real daemon or host image cache: inspect reports a miss and only pull prepares it.
    std::fs::write(
        &engine,
        r#"#!/bin/sh
cd -- "$(dirname -- "$0")" || exit 1
printf '%s\n' "$*" >> calls
case "$1" in
image) test -f prepared;;
pull) touch prepared;;
info) exit 0;;
*) exit 99;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::hard_link(&engine, root.join("runtime-wrapper")).unwrap();
    // Deliberately differs from DEFAULT_IMAGE so omitting with_image cannot pass.
    let image = format!("fixture.invalid/crash@sha256:{}", "a".repeat(64));
    prepare_storage_crash_image(&engine, &image);
    let provider = storage_crash_provider(root, root, &image);
    assert!(provider.availability().usable());
    let invocation = provider.invocation(root, "/bin/false", &[], &[], "fixture-worker");
    assert!(invocation.iter().any(|arg| arg == "--pull=never"));
    let image_index = invocation.iter().position(|arg| arg == &image).unwrap();
    assert_eq!(image_index, invocation.len() - 1);
    assert!(
        invocation
            .windows(2)
            .any(|pair| pair == ["--entrypoint", "/bin/false"])
    );
    assert!(
        !invocation
            .iter()
            .any(|arg| arg == af_sandbox::container::DEFAULT_IMAGE)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("calls")).unwrap(),
        format!("image inspect {image}\npull {image}\ninfo\n")
    );
}

#[test]
fn storage_crash_fixture_child() {
    let Some(root) = std::env::var_os(STORAGE_CRASH_ROOT).map(std::path::PathBuf::from) else {
        return;
    };
    af_sandbox::set_sandbox_root(root.clone()).unwrap();
    let runtime = af_sandbox::CheckRuntime::new().unwrap();
    for name in ["source", "package"] {
        std::fs::create_dir(runtime.path().join(name)).unwrap();
    }
    std::fs::write(root.join("runtime-path"), runtime.path().to_str().unwrap()).unwrap();
    std::fs::write(runtime.path().join("source/original"), b"exact input").unwrap();
    // This test-only transport forwards real Engine operations. It pauses immediately before
    // workload dispatch, while the real seeded tmpfs and keeper already exist.
    let image =
        std::env::var(STORAGE_CRASH_IMAGE).expect("parent prepared the crash fixture image");
    let provider = storage_crash_provider(&root, runtime.path(), &image);
    provider
        .exec_worker_captured(
            &runtime.path().join("source"),
            "/bin/false",
            &[],
            &[],
            vec![],
            Instant::now() + EXEC_TIMEOUT,
            None,
        )
        .unwrap();
    panic!("the crash fixture must never resume");
}

#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn a_crashed_storage_owner_is_swept_without_touching_live_or_immutable_state() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    let provider = provider();
    let Availability::Usable { runtime: engine } = provider.availability() else {
        unreachable!()
    };
    let image = af_sandbox::container::DEFAULT_IMAGE;
    prepare_storage_crash_image(engine, image);
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let socket = root.join("ready.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let wrapper = root.join("runtime-wrapper");
    std::fs::write(&wrapper, format!("#!/usr/bin/env python3\nimport os,socket,sys\na=sys.argv[1:]\nif a[:1]==['run'] and '--name' in a and a[a.index('--name')+1].endswith('-worker'):\n s=socket.socket(socket.AF_UNIX); s.connect({}); s.sendall(b'ready'); s.recv(1); sys.exit(91)\nos.execv({},[{}]+a)\n", serde_json::to_string(socket.to_str().unwrap()).unwrap(), serde_json::to_string(engine.to_str().unwrap()).unwrap(), serde_json::to_string(engine.to_str().unwrap()).unwrap())).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cas = af_store::Cas::open(root.join("cas")).unwrap();
    let id = cas.put(b"reachable immutable bytes").unwrap();
    let live = root.join(format!("af-check-{}-live", std::process::id()));
    std::fs::create_dir(&live).unwrap();
    std::fs::write(live.join("private"), b"active").unwrap();
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage_crash_fixture_child", "--nocapture"])
            .env(STORAGE_CRASH_ROOT, root)
            .env(STORAGE_CRASH_IMAGE, image)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(root.join("child.stderr")).unwrap())
            .spawn()
            .unwrap(),
    );
    let (sender, receiver) = std::sync::mpsc::channel();
    let barrier = std::thread::spawn(move || {
        sender.send(listener.accept().unwrap().0).unwrap();
    });
    let connection = receiver.recv_timeout(EXEC_TIMEOUT).unwrap_or_else(|error| {
        panic!(
            "storage barrier: {error}: {}",
            std::fs::read_to_string(root.join("child.stderr")).unwrap()
        )
    });
    barrier.join().unwrap();
    let path =
        std::path::PathBuf::from(std::fs::read_to_string(root.join("runtime-path")).unwrap());
    let lease: af_contracts::task::development::WorkerStorageLeaseV1 =
        serde_json::from_slice(&std::fs::read(path.join("worker-storage-v1.json")).unwrap())
            .unwrap();
    lease.validate().unwrap();
    let before = af_sandbox::sweep_stale_sandboxes_in(root);
    assert_eq!(before.live, 2, "both owners are live before the crash");
    assert!(path.is_dir());
    child.0.kill().unwrap();
    assert_eq!(child.0.wait().unwrap().signal(), Some(9));
    drop(connection); // The test transport exits on EOF; no orphan host process is left.
    let swept = af_sandbox::sweep_stale_sandboxes_in(root);
    assert_eq!(swept.removed, 1, "{swept:?}");
    assert_eq!(swept.live, 1);
    assert!(!path.exists());
    assert_eq!(std::fs::read(live.join("private")).unwrap(), b"active");
    drop(cas);
    assert_eq!(
        af_store::Cas::open(root.join("cas"))
            .unwrap()
            .get(&id)
            .unwrap(),
        b"reachable immutable bytes"
    );
    let mut query = Command::new(engine);
    query.args([
        "volume",
        "ls",
        "--filter",
        &format!("name=^{}$", lease.name),
        "--format={{.Name}}",
    ]);
    let result = af_process::run_supervised(&mut query, None, EXEC_TIMEOUT).unwrap();
    assert!(result.status.success());
    assert!(
        result.stdout.is_empty(),
        "the crashed owner's exact private volume was removed"
    );
}

/// Probe: the canonical checkout on disk. `malicious_check.rs` could only prove the *snapshot*
/// immutable and printed a note when the working tree changed; here the assertion is the real
/// one — the same hostile command, and the checkout does not change.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn an_absolute_write_cannot_reach_the_checkout() {
    let provider = provider();
    let (dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let before = af_source_git::worktree_state(&repo).unwrap();

    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let output = run(
        &provider,
        sandbox.root(),
        "/bin/sh",
        &[
            "-c".to_string(),
            format!(
                "echo building; echo pwned > {}/src/main.rs; exit 0",
                repo.workdir().display()
            ),
        ],
    );
    assert!(output.status.success(), "the check did its job");

    let after = af_source_git::worktree_state(&repo).unwrap();
    assert_eq!(
        before, after,
        "an absolute-path write must not reach the checkout"
    );
    drop(dir);
}

/// Probe: undeclared network. `--network=none` leaves nothing to connect with — no route out
/// and no resolver, so the refusal is immediate, not a timeout. `timeout` guards the assertion
/// against hanging instead of failing.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn an_undeclared_connection_is_refused() {
    let provider = provider();
    let sandbox = tempfile::tempdir().unwrap();

    let connect = run(
        &provider,
        sandbox.path(),
        "/usr/bin/timeout",
        &[
            "10".to_string(),
            "/bin/bash".to_string(),
            "-c".to_string(),
            "echo probe > /dev/tcp/1.1.1.1/80".to_string(),
        ],
    );
    assert!(
        !connect.status.success(),
        "a direct connection must fail: {}",
        String::from_utf8_lossy(&connect.stderr)
    );

    let resolve = run(
        &provider,
        sandbox.path(),
        "/usr/bin/timeout",
        &[
            "10".to_string(),
            "/usr/bin/getent".to_string(),
            "hosts".to_string(),
            "debian.org".to_string(),
        ],
    );
    assert!(
        !resolve.status.success(),
        "name resolution must fail: {}",
        String::from_utf8_lossy(&resolve.stdout)
    );
}

/// The control, and the integration point: a project check — typed slots, `resolve()`, exactly
/// what the check runner validates — executes *inside* the container, does its work, and the
/// work lands in the sandbox bind. Without this, the probes above could pass because the
/// container runs nothing at all.
#[test]
#[ignore = "needs a live container runtime; run via make review-kernel-container-probes"]
fn a_check_command_runs_contained_and_its_work_lands_in_the_sandbox() {
    let provider = provider();
    let (dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();

    let check = CheckDefinition::new(
        "contained-control",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("cat src/main.rs > copied.rs && echo checked"),
            ],
        ),
    );
    let runner = CheckRunner::new(&cas, sandbox.root()).with_timeout(EXEC_TIMEOUT);
    let result = runner.run_with(&check, |program, args, env, timeout| {
        provider
            .exec_evidenced_controlled(sandbox.root(), program, args, env, timeout, None)
            .map(|execution| (execution.output, execution.stderr_held))
            .map_err(|error| error.to_string())
    });

    assert_eq!(result.status, CheckStatus::Passed, "{result:?}");
    assert!(
        String::from_utf8_lossy(&cas.get(result.stdout.as_ref().unwrap()).unwrap())
            .contains("checked")
    );
    assert_eq!(
        std::fs::read_to_string(sandbox.root().join("copied.rs")).unwrap(),
        "fn main() {}\n",
        "work done in /work lands in the sandbox on the host"
    );
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(sandbox.root().join("copied.rs"))
                .unwrap()
                .uid(),
            nix::unistd::getuid().as_raw(),
            "container writes must remain owned by the host user"
        );
    }
    drop(dir);
}
