//! Deterministic process crashes inside real capture/clone operations. The barriers exist
//! only in this crate's unit-test build, never behind a product environment variable.
use super::*;
use crate::{CheckRuntime, sweep_stale_sandboxes_in};
use af_contracts::Producer;
use af_contracts::task::{TASK_REVISION_V1, TaskRevisionV1};
use af_store::EventStore;
use serde_json::json;
use std::io::{BufRead, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

static STAGE: OnceLock<String> = OnceLock::new();
const CHILD_ROOT: &str = "AF_CACHE_CRASH_TEST_ROOT";
const UNADMITTED: &[u8] = b"interrupted producer output, never an admitted cache";

pub(crate) fn pause(stage: &str) {
    if STAGE.get().is_some_and(|selected| selected == stage) {
        println!("CACHE_CRASH_READY:{stage}");
        std::io::stdout().flush().unwrap();
        // The parent kills this exact pid while the operation owns its private directory.
        // EOF instead means the parent failed, and must fail the child rather than continue.
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
        panic!("the crash fixture must never resume");
    }
}

#[test]
fn crash_fixture_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let stage = std::env::var("AF_CACHE_CRASH_TEST_STAGE").unwrap();
    crate::set_sandbox_root(root.clone()).unwrap();
    let cas = Cas::open(root.join("cas")).unwrap();
    let runtime = CheckRuntime::new().unwrap();
    let preserved = CheckRuntime::new().unwrap();
    std::fs::write(preserved.path().join("container-owned"), b"keep").unwrap();
    std::fs::write(
        root.join("paths.json"),
        serde_json::to_vec(&json!({"runtime":runtime.path(),"preserved":preserved.path()}))
            .unwrap(),
    )
    .unwrap();
    preserved.preserve();
    STAGE.set(stage.clone()).unwrap();
    if stage == "capture" {
        let build = runtime.path().join("build");
        std::fs::create_dir(&build).unwrap();
        std::fs::write(build.join("object"), UNADMITTED).unwrap();
        capture_cache_directory(
            BuildCacheKindV1::Worker,
            &build,
            &BuildCacheLimitsV1::default(),
            &cas,
        )
        .unwrap();
    } else {
        assert_eq!(stage, "clone");
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        clone_cache_directory(
            &manifest,
            &runtime.path().join("layer"),
            &BuildCacheLimitsV1::default(),
            &cas,
        )
        .unwrap();
    }
    panic!("operation returned instead of stopping at its crash barrier");
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn mid_capture_and_clone_crashes_recover_private_roots_without_admitting_partial_caches() {
    for stage in ["capture", "clone"] {
        crash_and_recover(stage);
    }
}

fn crash_and_recover(stage: &str) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let cas = Cas::open(root.join("cas")).unwrap();
    let source = root.join("approved-source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("object"), b"reachable immutable cache").unwrap();
    let limits = BuildCacheLimitsV1::default();
    let captured =
        capture_cache_directory(BuildCacheKindV1::Worker, &source, &limits, &cas).unwrap();
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&captured.manifest).unwrap(),
    )
    .unwrap();
    let store_path = root.join("events.sqlite");
    let mut store = EventStore::open(&store_path).unwrap();
    let producer = Producer::KernelOperation {
        run_id: "cache-crash".into(),
        node_id: None,
        operation_id: "fixture@1".into(),
    };
    let policy = cas
        .put_json(&json!({"fixture":"cache crash authority"}))
        .unwrap();
    let requirement = cas
        .put_artifact(
            "af/Requirements@1",
            producer.clone(),
            vec![],
            None,
            json!({"text":"cache recovery"}),
        )
        .unwrap()
        .0;
    let mut task: TaskRevisionV1 = serde_json::from_str(include_str!(
        "../../../../../fixtures/task-contracts/v1/task-revision.json"
    ))
    .unwrap();
    task.inputs.get_mut("requirements").unwrap().artifact_ids = vec![requirement.clone()];
    task.provenance.adapter_id = policy.clone();
    task.provenance.input_artifact_ids = vec![requirement, captured.manifest_id.clone()];
    task.authority.policy_id = policy.clone();
    task.acceptance.get_mut("checked").unwrap().verifier_policy = policy;
    task.limits.deadline_unix_ms = af_store::store::task::now().unwrap() + 600000;
    let revision = cas
        .put_artifact(
            TASK_REVISION_V1,
            producer,
            vec![],
            None,
            serde_json::to_value(&task).unwrap(),
        )
        .unwrap()
        .0;
    let lease = store
        .open_task(&cas, &revision, "active-cache-writer", 600000)
        .unwrap();
    let before = store.task_projection(&cas, &task.task_id).unwrap().unwrap();
    let mut child = KillOnDrop(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "build_cache::tests::crash::crash_fixture_child",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &root)
            .env("AF_CACHE_CRASH_TEST_STAGE", stage)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(root.join("child.stderr")).unwrap())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut transcript = String::new();
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.unwrap();
            transcript.push_str(&line);
            transcript.push('\n');
            if line.starts_with("CACHE_CRASH_READY:") {
                send.send(line).unwrap();
                return;
            }
        }
        panic!("child exited before the barrier: {transcript}");
    });
    let ready = receive
        .recv_timeout(Duration::from_secs(120))
        .unwrap_or_else(|error| {
            panic!(
                "{stage} barrier: {error}; {}",
                std::fs::read_to_string(root.join("child.stderr")).unwrap()
            );
        });
    assert_eq!(ready, format!("CACHE_CRASH_READY:{stage}"));
    reader.join().unwrap();
    let paths: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("paths.json")).unwrap()).unwrap();
    let runtime = PathBuf::from(paths["runtime"].as_str().unwrap());
    let preserved = PathBuf::from(paths["preserved"].as_str().unwrap());
    let partial = runtime.join("layer");
    let unpublished = Manifest::new(vec![Entry {
        path: "object".into(),
        kind: EntryKind::File,
        content: af_source_git::digest_bytes(UNADMITTED),
        size: UNADMITTED.len() as u64,
    }])
    .unwrap();
    let unpublished_id =
        af_store::content_id(&serde_json::to_value(&unpublished).unwrap()).unwrap();
    if stage == "capture" {
        assert!(
            cas.contains(&unpublished.entries[0].content),
            "real object publication precedes the crash"
        );
        assert!(
            !cas.contains(&unpublished_id),
            "no complete Manifest published"
        );
    } else {
        assert_eq!(
            std::fs::read(partial.join("object")).unwrap(),
            b"reachable immutable cache"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(partial.join("object"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644,
            "clone has not completed private metadata normalization"
        );
    }
    // A live owner is never swept, including while it is paused inside the operation.
    let live = sweep_stale_sandboxes_in(&root);
    assert_eq!(live.live, 2);
    assert!(runtime.exists());
    child.0.kill().unwrap();
    assert_eq!(child.0.wait().unwrap().signal(), Some(9));
    drop(child);
    drop(store);
    let mut store = EventStore::open(&store_path).unwrap();
    let replay = store.task_projection(&cas, &task.task_id).unwrap().unwrap();
    assert_eq!(replay.revision_id, before.revision_id);
    assert!(
        replay.execution.is_none(),
        "unsettled cache bytes cannot invent an admitted Attempt/output"
    );
    assert!(
        store
            .apply_task_collection(&cas, 0, 0, false)
            .unwrap_err()
            .to_string()
            .contains("live writer lease")
    );
    for id in [&captured.manifest_id, &captured.manifest.entries[0].content] {
        cas.verify(id).unwrap();
    }
    if stage == "clone" {
        assert!(
            clone_cache_directory(&captured.manifest, &partial, &limits, &cas)
                .unwrap_err()
                .operator_detail()
                .contains("already exists"),
            "an interrupted destination cannot be reused"
        );
    }
    let sweep = sweep_stale_sandboxes_in(&root);
    assert_eq!(sweep.removed, 1);
    assert_eq!(sweep.preserved, 1);
    assert_eq!(sweep.failed, 0);
    assert!(!runtime.exists());
    assert_eq!(
        std::fs::read(preserved.join("container-owned")).unwrap(),
        b"keep"
    );
    assert!(preserved.join("preserved").is_file());
    store.release_task_lease(&cas, &lease).unwrap();
    let collection = store.apply_task_collection(&cas, 0, 0, false).unwrap();
    assert!(collection.swept);
    for id in [&captured.manifest_id, &captured.manifest.entries[0].content] {
        cas.verify(id).unwrap();
    }
    assert!(!cas.contains(&unpublished_id));
    assert!(
        !cas.contains(&unpublished.entries[0].content),
        "unreachable capture bytes are collectible after the lease ends"
    );
    let fresh = root.join("fresh-clone");
    clone_cache_directory(&captured.manifest, &fresh, &limits, &cas).unwrap();
    assert_eq!(
        std::fs::read(fresh.join("object")).unwrap(),
        b"reachable immutable cache"
    );
    assert_eq!(sweep_stale_sandboxes_in(&root).preserved, 1);
}
