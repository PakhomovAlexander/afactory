//! Guarded kernel performance experiments (ADR-0185) through the real kernel. A consumer
//! repository holds this workspace's crate sources and complete protected harness with the
//! fixture authority of `fixtures/task-runtime/experiment` in its `.af/` (never this
//! repository's own), and the staged
//! `fixtures/performance/kernel-measure/code-policy-measures.toml` installed into it verbatim,
//! as its README describes. Every Task below runs `af task start --execute`: the kernel measures
//! the source and the implementer's sealed candidate with the staged literal bootstrap, which
//! verifies the protected harness before it imports the driver or builds anything. Only a
//! passing guard builds the real `perf-probe` offline into the kernel's warm Cargo target and
//! supplies one judged sample of `canonical_validate_small` per repetition.
//!
//! The build-free baseline refusals need no successful measurement. The positive comparison is
//! controlled, never timing noise: the Task's source is a disposable product revision whose
//! `review_store::validate_envelope` sleeps, and the candidate removes the sleep with the
//! protected harness byte-identical. In the ignored manual test, tampered candidates also remove
//! the sleep, so only the guard stands between them and an improved receipt; each leaves a marker if any
//! of its code ran. Every Task's public evidence, its result, each output's typed envelope and
//! every repetition's bounded public stdout and stderr (`af.perf-kernel-receipts/3`, ADR-0186,
//! ADR-0187), is checked against its schema and reader, which recomputes every envelope's kernel
//! identities and binds each output to its Task's result, and exported to
//! `$AF_PERF_KERNEL_RECEIPTS` when it names a directory. No number here is a product speed claim.

use std::path::{Path, PathBuf};
use std::process::Command;

use review_core::task::measurement::{
    ComparisonObjective, MeasureObjectiveV1, MeasurementComparisonV1, MeasurementV1,
    compare_measurements,
};
use review_store::Cas;
use serde_json::{Value, json};

use crate::schemas;
use crate::task_cli;

const MEASURE: &str = "perf_canonical_validate_small";
const OBJECTIVE: &str = "perf_canonical_validate_small_elapsed";
const INSTANCE: &str = "canonical_validate_small__package_small_envelope";
const STAGED: &str = "fixtures/performance/kernel-measure/code-policy-measures.toml";
const CANONICAL: &str = "crates/review-store/src/canonical.rs";
const VALIDATE: &str =
    "pub fn validate_envelope(envelope: &review_core::ArtifactEnvelope) -> Result<(), String> {\n";
/// The controlled slowdown of the successful comparison's disposable baseline revision.
const SLOW_MS: u64 = 7500;
/// Refusal scenarios need only a small slowdown; they do not prove improvement.
const REFUSAL_SLOW_MS: u64 = 1500;
const EVALUATE: &str = "root.nodes.trial.nodes.evaluate";
const REFUSED: &str = "refused before any candidate import or build";

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// The files of the workspace a Snapshot of it holds: its tracked and unignored files in a Git
/// checkout, else every file of the sealed tree but a build target.
fn workspace_files(root: &Path) -> Vec<String> {
    if root.join(".git").exists() {
        let listed = git(
            root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        );
        return listed
            .split('\0')
            .filter(|path| !path.is_empty() && root.join(path).symlink_metadata().is_ok())
            .map(String::from)
            .collect();
    }
    fn walk(root: &Path, relative: &str, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(root.join(relative)).unwrap().flatten() {
            let name = entry.file_name().into_string().unwrap();
            let path = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            if relative.is_empty() && matches!(name.as_str(), "target" | ".git") {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out
}

/// Only the inputs of the real probe build and the complete protected instrument. Copying the
/// whole repository also made every five-second Worker materialize unrelated documentation,
/// release assets and other CLI fixtures; a judging Worker materializes them twice, for its
/// source and its base. The release runner left only 742 ms to start the evaluator. Keep all
/// crate files and every inventory entry, including build inputs outside the usual source
/// directories. The unchanged literal bootstrap checks the resulting closure.
fn consumer_file(path: &str, protected: &std::collections::BTreeSet<&str>) -> bool {
    path != ".af"
        && !path.starts_with(".af/")
        && (path.starts_with("crates/")
            || path.starts_with("scripts/performance/")
            || path.starts_with("fixtures/performance/")
            || protected.contains(path))
}

#[test]
fn guarded_consumer_keeps_the_complete_instrument_and_crates_without_repository_growth() {
    let consumer = Consumer::new();
    let workspace = workspace();
    let inventory: Value = serde_json::from_slice(
        &std::fs::read(workspace.join("fixtures/performance/kernel-measure/inventory.json"))
            .unwrap(),
    )
    .unwrap();
    for entry in inventory["files"].as_array().unwrap() {
        let path = entry["path"].as_str().unwrap();
        assert_eq!(
            std::fs::read(consumer.repo.join(path)).unwrap(),
            std::fs::read(workspace.join(path)).unwrap(),
            "the consumer keeps the exact protected file {path}"
        );
    }
    for path in workspace_files(&workspace)
        .into_iter()
        .filter(|path| path.starts_with("crates/"))
    {
        assert_eq!(
            std::fs::read(consumer.repo.join(&path)).unwrap(),
            std::fs::read(workspace.join(&path)).unwrap(),
            "the real probe builds with the workspace's crate file {path}"
        );
    }
    assert_eq!(
        std::fs::read(consumer.repo.join(STAGED)).unwrap(),
        std::fs::read(workspace.join(STAGED)).unwrap()
    );
    for path in [
        "docs/adr/README.md",
        "brand/README.md",
        ".github/workflows/release.yml",
        "fixtures/task-runtime/experiment/.af/task-catalog.toml",
    ] {
        assert!(workspace.join(path).is_file(), "fixture input {path}");
        assert!(!consumer.repo.join(path).exists(), "unrelated input {path}");
    }
    // Inventory membership, rather than an allowlist of today's directory names, retains a
    // protected build input anywhere. Unrelated repository growth adds no Worker setup work.
    let protected =
        std::collections::BTreeSet::from(["other/build.rs", "other/.cargo/config.toml"]);
    for path in &protected {
        assert!(consumer_file(path, &protected));
    }
    for index in 0..2000 {
        assert!(!consumer_file(&format!("docs/adr/{index}.md"), &protected));
        assert!(!consumer_file(
            &format!("fixtures/task-runtime/other/{index}.json"),
            &protected
        ));
    }
    let evaluator: toml::Value = toml::from_str(
        &std::fs::read_to_string(
            consumer
                .repo
                .join(".af/task-packages/fixture/experiment-evaluator/worker.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        evaluator["signature"]["attempt"]["wall_ms"].as_integer(),
        Some(5000)
    );
    assert_eq!(
        evaluator["signature"]["effects"][0].as_str(),
        Some("read-source")
    );
}

struct Consumer {
    _root: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    markers: PathBuf,
    receipts: Vec<Value>,
}

impl Consumer {
    /// The consumer repository: the probe's real crate sources and protected harness, plus the
    /// experiment fixture's authority with the staged measures installed, committed.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let workspace = workspace();
        let repo = root.join("repo");
        let inventory: Value = serde_json::from_slice(
            &std::fs::read(workspace.join("fixtures/performance/kernel-measure/inventory.json"))
                .unwrap(),
        )
        .unwrap();
        let protected = inventory["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        for path in workspace_files(&workspace) {
            if !consumer_file(&path, &protected) {
                continue;
            }
            let target = repo.join(&path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(workspace.join(&path), &target).unwrap();
            let mut permissions = std::fs::metadata(&target).unwrap().permissions();
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() | 0o200);
            }
            std::fs::set_permissions(&target, permissions).unwrap();
        }
        install_authority(&workspace, &repo);
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &["commit", "-qm", "consumer with the staged policy installed"],
        );
        let markers = root.join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        std::fs::create_dir_all(root.join("tasks")).unwrap();
        Consumer {
            state: root.join("state"),
            repo,
            markers,
            receipts: Vec::new(),
            root,
            _root: directory,
        }
    }

    fn commit(&self, edits: &[(&str, String)], message: &str) {
        for (path, text) in edits {
            std::fs::write(self.repo.join(path), text).unwrap();
        }
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-qm", message]);
    }

    /// Commit source edits in the same forms accepted by the fixture implementer. Null removes
    /// a file so each refusal can restore the original source before the next scenario.
    fn commit_source(&self, edits: &Value, message: &str) {
        for (path, body) in edits.as_object().unwrap() {
            let target = self.repo.join(path);
            let original = if body.get("replace").is_some() {
                Some(std::fs::read_to_string(&target).unwrap())
            } else {
                None
            };
            if target.symlink_metadata().is_ok() {
                std::fs::remove_file(&target).unwrap();
            }
            if body.is_null() {
                continue;
            }
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            if let Some(link) = body.get("symlink") {
                std::os::unix::fs::symlink(link.as_str().unwrap(), &target).unwrap();
            } else if let Some(encoded) = body.get("base64") {
                std::fs::write(&target, decode_base64(encoded.as_str().unwrap())).unwrap();
            } else if let Some(replace) = body.get("replace") {
                let original = original.unwrap();
                let old = replace[0].as_str().unwrap();
                assert!(original.contains(old), "{path}: {old}");
                std::fs::write(
                    &target,
                    original.replacen(old, replace[1].as_str().unwrap(), 1),
                )
                .unwrap();
            } else {
                std::fs::write(&target, body.as_str().unwrap()).unwrap();
            }
        }
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-qm", message]);
    }

    fn af(&self, args: &[&str]) -> (i32, String, String) {
        let home = self.root.join("home");
        let mut command = crate::common::af();
        // The kernel's toolchain, exactly as the operator's: the warm layer passes its rustup
        // home on, and rustup gave this test process both.
        for name in ["RUSTUP_HOME", "PATH"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let output = command
            .current_dir(&self.repo)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("AF_CACHE_POLICY_FILE")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("AF_GATE_TARGET_DIR")
            .env_remove("RUSTUP_TOOLCHAIN")
            .args(args)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // An `af` that ended without an exit code fails with its actual status and signal,
        // never as a made-up code.
        let code = output.status.code().unwrap_or_else(|| {
            use std::os::unix::process::ExitStatusExt;
            panic!(
                "af {args:?} ended without an exit code: {:?} (signal {:?}, core dumped {})\n\
                 stdout: {stdout}\nstderr: {stderr}",
                output.status,
                output.status.signal(),
                output.status.core_dumped()
            )
        });
        (code, stdout, stderr)
    }

    /// Start and execute one Task whose implementer writes `files`; its inspection document.
    fn start(&mut self, task_id: &str, files: Value, expected: i32) -> Value {
        let file = self.root.join("tasks").join(format!("{task_id}.json"));
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({
                "schema": "af.task-file/1",
                "task_id": task_id,
                "kind": "implement",
                "goal": format!("Make the guarded measure faster. files={files}"),
                "pipeline": {"name": "fixture/experiment", "fallback": "refuse"},
                "strategy": "small",
                "verification": "evaluation",
                "facts": {},
                "limits": {
                    "tokens": 1000,
                    "max_attempts": 8,
                    // The staged measures declare three 20-minute repetitions, and the checks
                    // node the captured 60-minute check wall: declared allowances, not runtime.
                    "wall_ms": 14_400_000,
                    "verification": {"tokens": 200, "attempts": 3, "wall_ms": 3_700_000}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let (code, stdout, stderr) = self.af(&[
            "task",
            "start",
            "--execute",
            "--file",
            file.to_str().unwrap(),
            "--state",
            self.state.to_str().unwrap(),
            "--json",
        ]);
        let outcome: Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(
            code,
            expected,
            "{task_id}: {stderr}\n{}",
            self.explain(&outcome)
        );
        schemas::valid(&schemas::validator("task-inspection-v13.json"), &outcome);
        self.retain(task_id, code, &outcome);
        outcome
    }

    /// What each measured side and the comparison of a Task came to, for a failure message.
    fn explain(&self, outcome: &Value) -> String {
        let cas = self.cas();
        let mut out = format!("{}\n", failure_diagnostics(&cas, outcome));
        // The Task's own account, which names each failed node and its first error line.
        if let Some(task) = outcome["task_id"].as_str() {
            let (_, shown, _) = self.af(&[
                "task",
                "show",
                task,
                "--state",
                self.state.to_str().unwrap(),
            ]);
            out.push_str(&format!("show: {shown}\n"));
        }
        for port in ["baseline", "candidate", "comparison"] {
            let Some(id) = outcome["result"]["outputs"][port]["artifact_ids"][0].as_str() else {
                out.push_str(&format!("{port}: absent\n"));
                continue;
            };
            let payload = cas.get_json(id).unwrap()["payload"].clone();
            out.push_str(&format!(
                "{port}: outcome {} failure {} summary {} metrics {}\n",
                payload["outcome"], payload["failure"], payload["summary"], payload["metrics"]
            ));
            for run in payload["runs"].as_array().into_iter().flatten() {
                let stderr = run["stderr_id"]
                    .as_str()
                    .map(|id| String::from_utf8_lossy(&cas.get(id).unwrap()).into_owned())
                    .unwrap_or_default();
                let tail: String = stderr.chars().rev().take(1500).collect();
                out.push_str(&format!(
                    "  run elapsed {} exit {} cache {} stderr tail: {}\n",
                    run["elapsed_ms"],
                    run["exit_code"],
                    run["cache"],
                    tail.chars().rev().collect::<String>()
                ));
            }
        }
        out
    }

    fn cas(&self) -> Cas {
        Cas::open_existing(self.state.join("cas")).unwrap()
    }

    /// The public evidence of one Task (`af.perf-kernel-receipts/3`, ADR-0187): its exit code
    /// and kernel result, every output's complete typed envelope, and each measured
    /// repetition's public stdout and stderr, bounded. Never a private Worker log.
    fn retain(&mut self, task_id: &str, code: i32, outcome: &Value) {
        let cas = self.cas();
        let mut ports = serde_json::Map::new();
        for port in ["baseline", "candidate", "comparison"] {
            if let Some(id) = outcome["result"]["outputs"][port]["artifact_ids"][0].as_str() {
                let envelope = cas.get_json(id).unwrap();
                assert_eq!(envelope["artifact_id"], id, "{task_id} {port}");
                let streams: Vec<Value> = envelope["payload"]["runs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, run)| {
                        json!({
                            "repetition": index + 1,
                            "stdout": retained_stream(&cas, run["stdout_id"].as_str()),
                            "stderr": retained_stream(&cas, run["stderr_id"].as_str()),
                        })
                    })
                    .collect();
                ports.insert(
                    port.into(),
                    json!({
                        "artifact_id": id,
                        "type": envelope["type"],
                        "snapshot_id": envelope["subject_snapshot_id"],
                        "envelope": envelope,
                        "streams": streams,
                    }),
                );
            }
        }
        self.receipts.push(json!({
            "task_id": task_id,
            "exit_code": code,
            "result": outcome["result"],
            "outputs": ports,
        }));
    }

    /// Each homogeneous source phase is checked against the schema and laboratory reader.
    fn export(&self, digest: &str) {
        let directory = std::env::var_os("AF_PERF_KERNEL_RECEIPTS").map(PathBuf::from);
        for (phase, document) in receipt_documents(digest, &self.receipts) {
            self.export_document(phase, &document, directory.as_deref());
        }
    }

    fn export_document(&self, phase: &str, document: &Value, directory: Option<&Path>) {
        schemas::valid(
            &schemas::validator("performance-kernel-receipts-v3.json"),
            document,
        );
        let filename = format!("receipts-{}-{phase}.json", std::process::id());
        let checked = self.root.join(&filename);
        std::fs::write(&checked, serde_json::to_vec_pretty(document).unwrap()).unwrap();
        let read = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .args([
                "-B",
                "-c",
                "import sys; sys.path.insert(0, sys.argv[1]); from perflab import contracts; \
                 contracts.read_file(sys.argv[2], 'kernel-receipts')",
            ])
            .arg(workspace().join("scripts/performance"))
            .arg(&checked)
            .output()
            .unwrap();
        assert!(
            read.status.success(),
            "the laboratory refuses the {phase} receipts: {}",
            String::from_utf8_lossy(&read.stderr)
        );
        let Some(directory) = directory else {
            return;
        };
        std::fs::create_dir_all(directory).unwrap();
        std::fs::copy(&checked, directory.join(filename)).unwrap();
    }
}

/// Exact Task membership is part of the fixture: additions, omissions or reordering must
/// update this partition explicitly rather than inherit another source revision's slowdown.
fn receipt_documents(digest: &str, receipts: &[Value]) -> Vec<(&'static str, Value)> {
    const TAMPERED: [&str; 6] = [
        "timing-boundary",
        "golden-fixture",
        "build-inputs",
        "case-variant-build-inputs",
        "symlink-to-file",
        "bytecode",
    ];
    let actual: Vec<_> = receipts
        .iter()
        .map(|task| task["task_id"].as_str().unwrap())
        .collect();
    let baseline_refusal: Vec<_> = TAMPERED.iter().copied().chain(["refused-case"]).collect();
    let candidate_refusal: Vec<_> = TAMPERED
        .iter()
        .copied()
        .chain(["wrong-unit", "undeclared-key"])
        .collect();
    let refusal: Vec<_> = ["missing-inputs"]
        .into_iter()
        .chain(baseline_refusal.iter().copied())
        .collect();
    let manual: Vec<_> = ["improve"]
        .into_iter()
        .chain(candidate_refusal.iter().copied())
        .collect();
    let phases: Vec<(&str, u64, Vec<&str>)> = if actual == refusal {
        vec![
            ("missing-inputs", 0, vec!["missing-inputs"]),
            ("refusal", 0, baseline_refusal),
        ]
    } else if actual == manual {
        vec![
            ("improve", SLOW_MS, vec!["improve"]),
            ("candidate-refusal", REFUSAL_SLOW_MS, candidate_refusal),
        ]
    } else if actual == ["improve"] {
        vec![("improve", SLOW_MS, vec!["improve"])]
    } else {
        panic!("receipt source-phase partition changed: {actual:?}");
    };
    let mut offset = 0;
    phases
        .into_iter()
        .map(|(phase, slowdown_ms, tasks)| {
            let end = offset + tasks.len();
            assert_eq!(actual[offset..end], tasks);
            let document = json!({
                "schema": "af.perf-kernel-receipts/3",
                "harness_digest": digest,
                "instance": INSTANCE,
                "measure": MEASURE,
                "objective": OBJECTIVE,
                "slowdown_ms": slowdown_ms,
                "tasks": receipts[offset..end],
            });
            offset = end;
            (phase, document)
        })
        .collect()
}

#[test]
fn receipts_partition_preserves_every_task_and_labels_each_source_phase() {
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(
            workspace().join("fixtures/performance/schema/kernel-receipts/valid-fixture.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let ids = [
        "missing-inputs",
        "timing-boundary",
        "golden-fixture",
        "build-inputs",
        "case-variant-build-inputs",
        "symlink-to-file",
        "bytecode",
        "refused-case",
    ];
    // Reuse a valid public Task receipt; only its scenario label differs in this fast test.
    let receipts: Vec<_> = ids
        .iter()
        .map(|id| {
            let mut task = fixture["tasks"][0].clone();
            task["task_id"] = json!(id);
            task
        })
        .collect();
    let documents = receipt_documents(fixture["harness_digest"].as_str().unwrap(), &receipts);
    assert_eq!(
        documents
            .iter()
            .map(|(phase, doc)| (
                *phase,
                doc["slowdown_ms"].as_u64().unwrap(),
                doc["tasks"].as_array().unwrap().len()
            ))
            .collect::<Vec<_>>(),
        vec![("missing-inputs", 0, 1), ("refusal", 0, 7)]
    );
    let retained: Vec<_> = documents
        .iter()
        .flat_map(|(_, doc)| doc["tasks"].as_array().unwrap().clone())
        .collect();
    assert_eq!(retained, receipts);
    let root = tempfile::tempdir().unwrap();
    let consumer = Consumer {
        root: root.path().to_owned(),
        repo: root.path().join("unused-repo"),
        state: root.path().join("unused-state"),
        markers: root.path().join("unused-markers"),
        receipts: receipts.clone(),
        _root: root,
    };
    let exported = consumer.root.join("exported");
    for (phase, document) in &documents {
        consumer.export_document(phase, document, Some(&exported));
        let filename = format!("receipts-{}-{phase}.json", std::process::id());
        let saved: Value =
            serde_json::from_slice(&std::fs::read(exported.join(filename)).unwrap()).unwrap();
        assert_eq!(&saved, document);
    }
    assert_eq!(std::fs::read_dir(&exported).unwrap().count(), 2);
    let manual_ids = [
        "improve",
        "timing-boundary",
        "golden-fixture",
        "build-inputs",
        "case-variant-build-inputs",
        "symlink-to-file",
        "bytecode",
        "wrong-unit",
        "undeclared-key",
    ];
    let manual_receipts: Vec<_> = manual_ids
        .iter()
        .map(|id| {
            let mut task = fixture["tasks"][0].clone();
            task["task_id"] = json!(id);
            task
        })
        .collect();
    let manual_documents = receipt_documents(
        fixture["harness_digest"].as_str().unwrap(),
        &manual_receipts,
    );
    assert_eq!(
        manual_documents
            .iter()
            .map(|(phase, doc)| (
                *phase,
                doc["slowdown_ms"].as_u64().unwrap(),
                doc["tasks"].as_array().unwrap().len()
            ))
            .collect::<Vec<_>>(),
        vec![("improve", 7500, 1), ("candidate-refusal", 1500, 8)]
    );
    for (phase, document) in &manual_documents {
        consumer.export_document(phase, document, None);
    }
    let improvement_documents = receipt_documents(
        fixture["harness_digest"].as_str().unwrap(),
        &manual_receipts[..1],
    );
    assert_eq!(improvement_documents.len(), 1);
    assert_eq!(improvement_documents[0].0, "improve");
    for altered in [
        receipts[..7].to_vec(),
        {
            let mut tasks = receipts.clone();
            tasks.swap(0, 1);
            tasks
        },
        {
            let mut tasks = receipts.clone();
            tasks.push(receipts[0].clone());
            tasks
        },
    ] {
        assert!(std::panic::catch_unwind(|| receipt_documents("unused", &altered)).is_err());
    }
}

/// The bytes of one retained stream kept per head and tail (`KERNEL_STREAM_PART_BYTES`).
const STREAM_PART: usize = 32 * 1024;

/// One repetition's public stream as the receipts keep it: the kernel's content ID, the
/// SHA-256 and size of its exact bytes, and those bytes, whole up to twice `STREAM_PART`,
/// else their first and last `STREAM_PART`; UTF-8 text when both parts are, else base64.
fn retained_stream(cas: &Cas, id: Option<&str>) -> Value {
    let Some(id) = id else {
        return Value::Null;
    };
    let bytes = cas.get(id).unwrap();
    assert_eq!(review_store::canonical::blob_content_id(&bytes), id);
    retained_bytes(id, &bytes)
}

/// The retained form of a stream's exact `bytes` under its kernel content ID `id`.
fn retained_bytes(id: &str, bytes: &[u8]) -> Value {
    use sha2::Digest;
    let (head, tail, omitted) = if bytes.len() <= 2 * STREAM_PART {
        let split = bytes.len().min(STREAM_PART);
        (&bytes[..split], &bytes[split..], 0)
    } else {
        (
            &bytes[..STREAM_PART],
            &bytes[bytes.len() - STREAM_PART..],
            bytes.len() - 2 * STREAM_PART,
        )
    };
    let (encoding, head, tail) = match (std::str::from_utf8(head), std::str::from_utf8(tail)) {
        (Ok(head), Ok(tail)) => ("utf-8", head.to_string(), tail.to_string()),
        _ => ("base64", base64(head), base64(tail)),
    };
    let sha256: String = sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    json!({
        "id": id,
        "sha256": format!("sha256:{sha256}"),
        "bytes": bytes.len(),
        "retained": if omitted == 0 { "whole" } else { "head_and_tail" },
        "encoding": encoding,
        "head": head,
        "tail": tail,
        "omitted_bytes": omitted,
    })
}

/// The fixture authority of `fixtures/task-runtime/experiment` in `repo/.af`, its experiment
/// Pipelines naming the staged measure and objective, its implementer able to write bytes,
/// replacements and links, every package re-pinned; and the code policy: a sanity check, the
/// Warm Check Cache's Cargo target and home, and the staged tables appended unchanged.
fn install_authority(workspace: &Path, repo: &Path) {
    let source = workspace.join("fixtures/task-runtime/experiment/.af");
    task_cli::copy_tree(&source, &repo.join(".af"));
    let packages = repo.join(".af/task-packages/fixture");
    for (package, from, to) in [
        (
            "experiment",
            "measures = [\"write\"]",
            format!("measures = [\"{MEASURE}\"]"),
        ),
        (
            "experiment",
            "port = \"write\"",
            format!("port = \"{MEASURE}\""),
        ),
        (
            "experiment-trial",
            "measures = [\"write\"]",
            format!("measures = [\"{MEASURE}\"]"),
        ),
        (
            "experiment-trial",
            "port = \"write\"",
            format!("port = \"{MEASURE}\""),
        ),
        (
            "experiment-trial",
            "objective = \"smaller\"",
            format!("objective = \"{OBJECTIVE}\""),
        ),
    ] {
        let path = packages.join(package).join("pipeline.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{package}: {from}");
        std::fs::write(&path, text.replace(from, &to)).unwrap();
    }
    std::fs::write(packages.join("implementer/worker.py"), IMPLEMENTER).unwrap();
    let staged = std::fs::read_to_string(workspace.join(STAGED)).unwrap();
    std::fs::write(
        repo.join(".af/code-policy.toml"),
        format!("{POLICY}\n{staged}"),
    )
    .unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    for name in [
        "experiment",
        "experiment-trial",
        "experiment-evaluator",
        "implementer",
    ] {
        let package = format!("fixture/{name}");
        catalog["packages"][package.as_str()]["digest"] = toml::Value::String(
            review_config::lock::package_digest(&package, &packages.join(name)).unwrap(),
        );
    }
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
}

const POLICY: &str = r#"schema = "af.code-task-policy/1"
check_wall_ms = 3600000
require_container = false

[checks.sanity]
name = "sanity"
required = true

[checks.sanity.command]
program = "python3"

[[checks.sanity.command.args]]
value = "-B"
provenance = "literal"

[[checks.sanity.command.args]]
value = "-c"
provenance = "literal"

[[checks.sanity.command.args]]
value = "import os; assert os.path.isfile('Cargo.lock')"
provenance = "literal"

# The kernel-provided Cargo target and home a guarded measure builds with, offline.
[warm]
build_cache = ["cargo_target", "cargo_home"]
"#;

/// The implementer: the goal names each file's new text, `{"replace": [old, new]}`,
/// `{"base64": bytes}` or `{"symlink": target}` after `files=`.
const IMPLEMENTER: &str = r#"import base64, json, os, sys
request = json.load(sys.stdin)
text = request['inputs']['requirements'][0]['payload']['text']
files = json.loads(text.split('files=', 1)[1])
for path, body in sorted(files.items()):
    if os.path.dirname(path):
        os.makedirs(os.path.dirname(path), exist_ok=True)
    if isinstance(body, dict) and 'symlink' in body:
        os.symlink(body['symlink'], path)
    elif isinstance(body, dict) and 'base64' in body:
        open(path, 'wb').write(base64.b64decode(body['base64']))
    elif isinstance(body, dict) and 'replace' in body:
        old, new = body['replace']
        current = open(path).read()
        assert old in current, (path, old)
        open(path, 'w').write(current.replace(old, new, 1))
    else:
        open(path, 'w').write(body)
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'report': [{'summary': 'Wrote ' + ', '.join(sorted(files))}]}}))
"#;

fn measurement(cas: &Cas, outcome: &Value, port: &str) -> MeasurementV1 {
    let id = outcome["result"]["outputs"][port]["artifact_ids"][0]
        .as_str()
        .unwrap_or_else(|| panic!("no {port} output: {}", failure_diagnostics(cas, outcome)));
    let envelope = cas.get_json(id).unwrap();
    assert_eq!(envelope["type"], "af/Measurement@1");
    schemas::valid(
        &schemas::validator("measurement-v1.json"),
        &envelope["payload"],
    );
    let value: MeasurementV1 = serde_json::from_value(envelope["payload"].clone()).unwrap();
    value.validate().unwrap();
    value
}

fn comparison(cas: &Cas, outcome: &Value) -> MeasurementComparisonV1 {
    let id = outcome["result"]["outputs"]["comparison"]["artifact_ids"][0]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "no comparison output: {}",
                failure_diagnostics(cas, outcome)
            )
        });
    let envelope = cas.get_json(id).unwrap();
    schemas::valid(
        &schemas::validator("measurement-comparison-v1.json"),
        &envelope["payload"],
    );
    serde_json::from_value(envelope["payload"].clone()).unwrap()
}

/// Failure-only fixture diagnostics: the entire nested inspection plus the invocations,
/// feedback and diagnostics it names. Do not follow arbitrary input/provenance references
/// into source or Worker context. Preserve the scenario and report unreadable CAS objects
/// without masking the original assertion. At most 64 objects, each at most 64 KiB of source
/// JSON (head/tail with explicit byte counts above that); this is not a public receipt schema.
fn failure_diagnostics(cas: &Cas, outcome: &Value) -> Value {
    fn references(value: &Value, ids: &mut std::collections::BTreeSet<String>) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if matches!(
                        key.as_str(),
                        "invocation_id" | "feedback_id" | "diagnostic_id"
                    ) {
                        if let Some(id) = value.as_str() {
                            ids.insert(id.to_owned());
                        }
                    } else if key == "feedback_ids" {
                        for id in value
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                        {
                            ids.insert(id.to_owned());
                        }
                    }
                    references(value, ids);
                }
            }
            Value::Array(values) => {
                for value in values {
                    references(value, ids);
                }
            }
            _ => {}
        }
    }
    fn bounded(value: &Value) -> Value {
        let bytes = serde_json::to_vec(value).unwrap();
        if bytes.len() <= 2 * STREAM_PART {
            json!({"retained": "whole", "value": value})
        } else {
            retained_bytes(&review_store::canonical::blob_content_id(&bytes), &bytes)
        }
    }
    const MAX_ARTIFACTS: usize = 64;
    let mut pending = std::collections::BTreeSet::new();
    let mut artifacts = serde_json::Map::new();
    references(outcome, &mut pending);
    while artifacts.len() < MAX_ARTIFACTS {
        let Some(id) = pending.pop_first() else {
            break;
        };
        if artifacts.contains_key(&id) {
            continue;
        }
        let artifact = match cas.get_json(&id) {
            Ok(value) => {
                references(&value, &mut pending);
                bounded(&value)
            }
            Err(error) => json!({"read_error": error.to_string()}),
        };
        artifacts.insert(id, artifact);
    }
    let omitted = pending
        .iter()
        .filter(|id| !artifacts.contains_key(*id))
        .count();
    json!({
        "scenario": outcome["task_id"],
        "inspection": bounded(outcome),
        "artifacts": artifacts,
        "omitted_artifacts": omitted,
        "artifact_limit": MAX_ARTIFACTS,
    })
}

#[test]
fn missing_measurement_diagnostics_expose_nested_attempts_and_bound_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let cas = Cas::open(root.path()).unwrap();
    let diagnostic = json!({"reason": "resource", "detail": "fixture reservation exhausted"});
    let diagnostic_id = cas.put_json(&diagnostic).unwrap();
    let feedback = json!({"kind": "retry", "diagnostic_id": diagnostic_id});
    let feedback_id = cas.put_json(&feedback).unwrap();
    let invocation =
        json!({"node": "root.nodes.trial.nodes.measure", "feedback_ids": [feedback_id]});
    let invocation_id = cas.put_json(&invocation).unwrap();
    let missing_id = format!("sha256:{}", "f".repeat(64));
    let outcome = json!({
        "task_id": "guarded-missing-candidate",
        "result": {"execution": "exhausted", "outputs": {}},
        "execution_records": [{"record": {
            "kind": "reserved", "invocation_id": invocation_id,
            "deadline_unix_ms": 1700000005000_u64, "reserved_tokens": 50,
        }}],
        "run_reports": [{"diagnostic_id": missing_id}],
    });
    let receipt = failure_diagnostics(&cas, &outcome);
    assert_eq!(receipt["scenario"], "guarded-missing-candidate");
    assert_eq!(receipt["inspection"]["value"], outcome);
    for (id, expected) in [
        (&invocation_id, &invocation),
        (&feedback_id, &feedback),
        (&diagnostic_id, &diagnostic),
    ] {
        assert_eq!(receipt["artifacts"][id]["value"], *expected);
    }
    assert!(receipt["artifacts"][&missing_id]["read_error"].is_string());
    assert_eq!(receipt["omitted_artifacts"], 0);
    let panic = std::panic::catch_unwind(|| measurement(&cas, &outcome, "candidate")).unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("no candidate output"));
    assert!(message.contains("guarded-missing-candidate"));
    assert!(message.contains("fixture reservation exhausted"));

    let mut large = outcome.clone();
    large["padding"] = json!("x".repeat(2 * STREAM_PART + 1));
    let ids: Vec<String> = (0..70)
        .map(|index| cas.put_json(&json!({"index": index})).unwrap())
        .collect();
    large["feedback_ids"] = json!(ids);
    let receipt = failure_diagnostics(&cas, &large);
    assert_eq!(receipt["inspection"]["retained"], "head_and_tail");
    assert!(receipt["inspection"]["omitted_bytes"].as_u64().unwrap() > 0);
    assert_eq!(receipt["artifacts"].as_object().unwrap().len(), 64);
    assert!(receipt["omitted_artifacts"].as_u64().unwrap() > 0);
}

fn text(cas: &Cas, id: Option<&String>) -> String {
    String::from_utf8_lossy(&cas.get(id.expect("a retained stream")).unwrap()).into_owned()
}

fn outcome_of(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// Whether any Attempt was ever reserved for `node`.
fn attempted(cas: &Cas, outcome: &Value, node: &str) -> bool {
    outcome["execution_records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["record"]["kind"] == "reserved")
        .any(|entry| {
            let invocation = cas
                .get_json(entry["record"]["invocation_id"].as_str().unwrap())
                .unwrap();
            invocation["payload"]["node"] == node
        })
}

/// The setup record the adapter wrote on stderr: what the bound target held, observed.
fn setup(stderr: &str) -> Value {
    let line = stderr
        .lines()
        .find_map(|line| line.strip_prefix("kernel-measure setup: "))
        .unwrap_or_else(|| panic!("no setup record in {stderr}"));
    serde_json::from_str(line).unwrap()
}

/// Copy the locked registry dependencies of `lock` from the toolchain's Cargo home into the
/// kernel's warm Cargo home `target`: each package's index entry and `.crate` archive, nothing
/// else and never a credential. This is the operator's dependency preparation; the measure
/// itself never fetches.
fn seed_cargo_home(lock: &Path, target: &Path) {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .expect("a Cargo home holding the workspace's locked dependencies");
    let lock: toml::Value = toml::from_str(&std::fs::read_to_string(lock).unwrap()).unwrap();
    let packages: Vec<(String, String)> = lock["package"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| {
            package
                .get("source")
                .and_then(|source| source.as_str())
                .is_some_and(|source| source.starts_with("registry+"))
        })
        .map(|package| {
            (
                package["name"].as_str().unwrap().to_string(),
                package["version"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let index_path = |name: &str| {
        let name = name.to_lowercase();
        match name.len() {
            1 => format!("1/{name}"),
            2 => format!("2/{name}"),
            3 => format!("3/{}/{name}", &name[..1]),
            _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
        }
    };
    let mut copied = 0;
    for registry in std::fs::read_dir(home.join("registry/index"))
        .unwrap()
        .flatten()
    {
        let name = registry.file_name();
        let index = registry.path();
        let into = target.join("registry/index").join(&name);
        std::fs::create_dir_all(&into).unwrap();
        if index.join("config.json").is_file() {
            std::fs::copy(index.join("config.json"), into.join("config.json")).unwrap();
        }
        let archives = home.join("registry/cache").join(&name);
        let archives_into = target.join("registry/cache").join(&name);
        std::fs::create_dir_all(&archives_into).unwrap();
        for (package, version) in &packages {
            let entry = index.join(".cache").join(index_path(package));
            if entry.is_file() {
                let destination = into.join(".cache").join(index_path(package));
                std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
                std::fs::copy(&entry, &destination).unwrap();
            }
            let archive = archives.join(format!("{package}-{version}.crate"));
            if archive.is_file() {
                std::fs::copy(&archive, archives_into.join(archive.file_name().unwrap())).unwrap();
                copied += 1;
            }
        }
    }
    assert!(
        copied > 0,
        "the toolchain's Cargo home {} holds none of the locked dependencies",
        home.display()
    );
}

/// The warm Cargo home the kernel created for the consumer's measures.
fn warm_cargo_home(root: &Path) -> PathBuf {
    let cache = root.join("cache/af/task-build-cache");
    let mut found = Vec::new();
    for project in std::fs::read_dir(&cache).unwrap().flatten() {
        for key in std::fs::read_dir(project.path()).unwrap().flatten() {
            if key.path().join("cargo_home").is_dir() {
                found.push(key.path().join("cargo_home"));
            }
        }
    }
    assert_eq!(
        found.len(),
        1,
        "one warm key below {}: {found:?}",
        cache.display()
    );
    found.pop().unwrap()
}

/// An unchecked-hash bytecode file for `perflab/__init__.py`, compiled by the `python3` the
/// measure runs, whose code writes `marker`: Python would load it whatever the verified source
/// says. (its path in the repository, its bytes)
fn planted_bytecode(root: &Path, marker: &Path) -> (String, Vec<u8>) {
    let source = root.join("planted.py");
    std::fs::write(
        &source,
        format!(
            "open({:?}, 'w').write('imported')\n",
            marker.display().to_string()
        ),
    )
    .unwrap();
    let compiled = root.join("planted.pyc");
    let output = Command::new("python3")
        .args([
            "-I",
            "-c",
            "import py_compile, sys; py_compile.compile(sys.argv[1], cfile=sys.argv[2], \
             invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH, doraise=True); \
             print(sys.implementation.cache_tag)",
        ])
        .arg(&source)
        .arg(&compiled)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let tag = String::from_utf8(output.stdout).unwrap().trim().to_string();
    (
        format!("scripts/performance/perflab/__pycache__/__init__.{tag}.pyc"),
        std::fs::read(compiled).unwrap(),
    )
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |acc, (index, byte)| {
            acc | (u32::from(*byte) << (16 - 8 * index))
        });
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(TABLE[((value >> (18 - 6 * index)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn decode_base64(encoded: &str) -> Vec<u8> {
    let digit = |byte: u8| match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("invalid base64 digit"),
    };
    assert_eq!(encoded.len() % 4, 0);
    let mut bytes = Vec::with_capacity(encoded.len() / 4 * 3);
    for chunk in encoded.as_bytes().chunks_exact(4) {
        let a = u32::from(digit(chunk[0]));
        let b = u32::from(digit(chunk[1]));
        let c = if chunk[2] == b'=' {
            0
        } else {
            u32::from(digit(chunk[2]))
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            u32::from(digit(chunk[3]))
        };
        let value = (a << 18) | (b << 12) | (c << 6) | d;
        bytes.push((value >> 16) as u8);
        if chunk[2] != b'=' {
            bytes.push((value >> 8) as u8);
        }
        if chunk[3] != b'=' {
            bytes.push(value as u8);
        }
    }
    bytes
}

/// Constructed elapsed receipts, not new measurements or a product speed claim. The old
/// pair was observed under load; credit only one extra sleep although setup validates too.
#[test]
fn guarded_improvement_margin_covers_observed_load_within_sample_timeout() {
    use review_core::task::measurement::{MeasurementRunV1, summarize};

    let staged: toml::Value =
        toml::from_str(&std::fs::read_to_string(workspace().join(STAGED)).unwrap()).unwrap();
    let objective: MeasureObjectiveV1 = staged["objectives"][OBJECTIVE].clone().try_into().unwrap();
    assert_eq!(objective.metric, "elapsed_ms");
    assert_eq!(objective.min_repetitions, 3);
    assert_eq!(
        serde_json::to_value(&objective).unwrap()["min_improvement_ratio"],
        "0.1"
    );
    let measure = &staged["measures"][MEASURE];
    assert_eq!(measure["repetitions"].as_integer(), Some(3));
    let id = format!("sha256:{}", "a".repeat(64));
    let constructed = |elapsed_ms: u64| {
        let runs: Vec<MeasurementRunV1> = (0..3)
            .map(|_| {
                serde_json::from_value(json!({
                    "started_unix_ms": 1, "elapsed_ms": elapsed_ms, "exit_code": 0,
                    "stdout_id": id, "stderr_id": id,
                    "metrics": {"wall_ms": {"value": "1", "unit": "ms"}}
                }))
                .unwrap()
            })
            .collect();
        let mut receipt: MeasurementV1 = serde_json::from_value(json!({
            "plan_id": id, "policy_id": id, "snapshot_id": id, "command_id": id,
            "measure": MEASURE, "warm": false, "repetitions": 3,
            "wall_ms": measure["wall_ms"].as_integer().unwrap(),
            "metrics": {"wall_ms": "ms"}, "outcome": "passed", "runs": runs,
            "summary": {}
        }))
        .unwrap();
        receipt.summary = summarize(&receipt.metrics, &receipt.runs).unwrap();
        receipt
    };
    let compare = |baseline_ms, candidate_ms| {
        compare_measurements(
            &id,
            &id,
            (&id, &constructed(baseline_ms)),
            (&id, &constructed(candidate_ms)),
            ComparisonObjective {
                name: OBJECTIVE,
                objective: &objective,
            },
        )
        .unwrap()
    };
    assert_eq!(
        compare(8148, 10790).metrics["elapsed_ms"]
            .conclusion
            .as_str(),
        "regressed"
    );
    let extra_sleep_ms = SLOW_MS - REFUSAL_SLOW_MS;
    assert_eq!(extra_sleep_ms, 6000);
    assert_eq!(
        compare(8148 + extra_sleep_ms, 10790).metrics["elapsed_ms"]
            .conclusion
            .as_str(),
        "improved"
    );

    let suite: toml::Value = toml::from_str(
        &std::fs::read_to_string(workspace().join("fixtures/performance/suite.toml")).unwrap(),
    )
    .unwrap();
    let timeout_ms = suite["cases"]["canonical_validate_small"]["sample_timeout_seconds"]
        .as_integer()
        .unwrap() as u64
        * 1000;
    assert_eq!(timeout_ms, 30_000);
    // Two setup validations and one timed validation share the one probe sample timeout.
    assert_eq!(3 * SLOW_MS, 22_500);
    assert!(3 * SLOW_MS < timeout_ms);
}

/// A sealed consumer and the digest of the exact staged harness it installs.
fn guarded_consumer() -> (Consumer, String, String) {
    let consumer = Consumer::new();
    let workspace = workspace();
    let canonical = std::fs::read_to_string(workspace.join(CANONICAL)).unwrap();
    assert!(canonical.contains(VALIDATE));
    // The repository check agrees with the staged policy installed here: the consumer holds the
    // workspace's exact protected harness, so the staged digest is its digest.
    let check = Command::new("python3")
        .args([
            "-B",
            "scripts/performance/cli.py",
            "harness-digest",
            "--check",
        ])
        .current_dir(&consumer.repo)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "the staged policy is stale for this tree: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let digest = String::from_utf8(check.stdout).unwrap().trim().to_string();
    let staged = std::fs::read_to_string(workspace.join(STAGED)).unwrap();
    assert!(staged.contains(&format!("value = \"{digest}\"")));

    (consumer, canonical, digest)
}

#[test]
fn guarded_kernel_experiments_refuse_every_tampered_harness() {
    let (mut consumer, _canonical, digest) = guarded_consumer();
    let workspace = workspace();
    // 1. Missing inputs fail: the warm Cargo home is empty and the build is offline and locked.
    let outcome = consumer.start("missing-inputs", json!({"NOTES.md": "unmeasured\n"}), 4);
    let cas = consumer.cas();
    let baseline = measurement(&cas, &outcome, "baseline");
    assert_eq!(outcome_of(&baseline.outcome), "failed");
    let failure = baseline.failure.as_ref().unwrap();
    assert_eq!((failure.repetition, failure.reason.as_str()), (1, "exit"));
    let stderr = text(&cas, baseline.runs[0].stderr_id.as_ref());
    assert!(stderr.contains("protected files match"), "{stderr}");
    assert!(stderr.contains("never fetched"), "{stderr}");
    assert!(
        !text(&cas, baseline.runs[0].stdout_id.as_ref()).contains("af.measure-report/1"),
        "a failed repetition prints no report"
    );
    // An empty offline Cargo home makes an accidental build fail with "never fetched";
    // every tampered baseline must instead reach the guard's REFUSED diagnostic first.
    let unslow = Value::Null;
    let golden =
        std::fs::read_to_string(workspace.join("fixtures/performance/golden/native.json")).unwrap();
    let entry: Value = serde_json::from_str::<Value>(&golden).unwrap()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["instance_id"] == INSTANCE)
        .unwrap()
        .clone();
    let expected = entry["digest"].as_str().unwrap().to_string();
    let build_marker = consumer.markers.join("build-rs-ran");
    let case_marker = consumer.markers.join("case-variant-build-rs-ran");
    let bytecode_marker = consumer.markers.join("bytecode-imported");
    let (bytecode_path, bytecode) = planted_bytecode(&consumer.root, &bytecode_marker);
    let tampered: Vec<(&str, Value, &str)> = vec![
        (
            "timing-boundary",
            json!({
                CANONICAL: unslow,
                "crates/review-pipeline/examples/perf-probe/cases.rs": {"replace": [
                    ".and_then(|envelope| review_store::validate_envelope(&envelope).map(|()| envelope))",
                    ".and_then(|envelope| Ok(envelope))"
                ]},
            }),
            "harness of",
        ),
        (
            "golden-fixture",
            json!({
                CANONICAL: unslow,
                "fixtures/performance/golden/native.json": {"replace": [
                    expected.clone(), format!("sha256:{}", "1".repeat(64))
                ]},
            }),
            "harness of",
        ),
        (
            "build-inputs",
            json!({
                CANONICAL: unslow,
                ".cargo/config.toml": "[build]\nrustflags = [\"-Cdebug-assertions=off\"]\n",
                "crates/review-store/build.rs": format!(
                    "fn main() {{ std::fs::write({:?}, b\"ran\").unwrap(); }}\n",
                    build_marker.display().to_string()
                ),
                "crates/review-store/examples/extra/Cargo.toml": "[package]\nname = \"extra\"\n",
            }),
            "harness of",
        ),
        (
            // Effective on a case-insensitive filesystem, where Cargo runs BUILD.RS as the
            // package's build script and reads .CaRgO/config.toml; the guard folds case.
            "case-variant-build-inputs",
            json!({
                CANONICAL: unslow,
                ".CaRgO/config.toml": "[build]\nrustflags = [\"-Cdebug-assertions=off\"]\n",
                "crates/review-store/BUILD.RS": format!(
                    "fn main() {{ std::fs::write({:?}, b\"ran\").unwrap(); }}\n",
                    case_marker.display().to_string()
                ),
            }),
            "harness of",
        ),
        (
            "symlink-to-file",
            json!({CANONICAL: unslow, "crates/lockfile": {"symlink": "../Cargo.lock"}}),
            "crates/lockfile is a symbolic link",
        ),
        (
            // Sealing leaves bytecode out of a candidate unless the tree stages it (ADR-0207);
            // this candidate stages it, so the guard is what refuses it.
            "bytecode",
            json!({
                CANONICAL: unslow,
                bytecode_path: {"base64": base64(&bytecode)},
                ".af/seal-ignore": "!__pycache__/\n!*.pyc\n",
            }),
            "harness of",
        ),
    ];
    for (task, mut files, why) in tampered {
        // Product slowdown is irrelevant to a baseline refusal. Only the protected harness
        // edit is committed, so the candidate is a trivial unmeasured note.
        files.as_object_mut().unwrap().remove(CANONICAL);
        let mut restore = serde_json::Map::new();
        for path in files.as_object().unwrap().keys() {
            let target = consumer.repo.join(path);
            let original = match target.symlink_metadata() {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    json!({"symlink": std::fs::read_link(&target).unwrap().to_str().unwrap()})
                }
                Ok(_) => json!({"base64": base64(&std::fs::read(&target).unwrap())}),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
                Err(error) => panic!("{path}: {error}"),
            };
            restore.insert(path.clone(), original);
        }
        consumer.commit_source(&files, task);
        let outcome = consumer.start(task, json!({"NOTES.md": "unmeasured\n"}), 4);
        let cas = consumer.cas();
        let baseline = measurement(&cas, &outcome, "baseline");
        assert_eq!(outcome_of(&baseline.outcome), "failed", "{task}");
        let failure = baseline.failure.as_ref().unwrap();
        assert_eq!(
            (failure.repetition, failure.reason.as_str()),
            (1, "exit"),
            "{task}"
        );
        assert_eq!(baseline.runs.len(), 1, "{task}");
        assert_eq!(baseline.runs[0].exit_code, Some(2), "{task}");
        let stderr = text(&cas, baseline.runs[0].stderr_id.as_ref());
        assert!(
            stderr.contains(REFUSED) && stderr.contains(why),
            "{task}: {stderr}"
        );
        assert!(
            !stderr.contains("kernel-measure setup"),
            "{task}: nothing was built"
        );
        assert!(
            text(&cas, baseline.runs[0].stdout_id.as_ref()).is_empty(),
            "{task}"
        );
        // The fixture trial still creates a candidate Measurement after a failed baseline.
        // Its guard refuses the same tampered source without building or sampling the probe.
        let candidate = measurement(&cas, &outcome, "candidate");
        assert_eq!(outcome_of(&candidate.outcome), "failed", "{task}");
        assert_eq!(candidate.runs.len(), 1, "{task}");
        assert_eq!(candidate.runs[0].exit_code, Some(2), "{task}");
        let candidate_stderr = text(&cas, candidate.runs[0].stderr_id.as_ref());
        assert!(
            candidate_stderr.contains(REFUSED),
            "{task}: {candidate_stderr}"
        );
        assert!(!candidate_stderr.contains("kernel-measure setup: "));
        assert!(text(&cas, candidate.runs[0].stdout_id.as_ref()).is_empty());
        let recorded = comparison(&cas, &outcome);
        assert_eq!(outcome_of(&recorded.outcome), "inconclusive", "{task}");
        assert!(!attempted(&cas, &outcome, EVALUATE), "{task}");
        consumer.commit_source(
            &Value::Object(restore),
            &format!("restore source after {task}"),
        );
    }
    for marker in [&build_marker, &case_marker, &bytecode_marker] {
        assert!(
            !marker.exists(),
            "{} ran before the guard refused",
            marker.display()
        );
    }

    // The adapter refuses this unsupported case before it can build or report a sample.
    let policy = std::fs::read_to_string(consumer.repo.join(".af/code-policy.toml")).unwrap();
    let edited = policy.replacen(
        &format!("value = \"{INSTANCE}\""),
        "value = \"source_capture__files_100_tree_source_files_128b\"",
        1,
    );
    consumer.commit(&[(".af/code-policy.toml", edited)], "refused-case");
    let outcome = consumer.start("refused-case", json!({"NOTES.md": "unmeasured\n"}), 4);
    let cas = consumer.cas();
    let baseline = measurement(&cas, &outcome, "baseline");
    assert_eq!(outcome_of(&baseline.outcome), "failed", "refused-case");
    let failure = baseline.failure.as_ref().unwrap();
    assert_eq!(failure.reason.as_str(), "exit", "refused-case");
    assert!(
        failure.detail.contains("the command exited 1"),
        "{}",
        failure.detail
    );
    assert!(baseline.summary.is_empty(), "refused-case");
    let stderr = text(&cas, baseline.runs[0].stderr_id.as_ref());
    assert!(
        stderr.contains("not a nonspawning native library case"),
        "{stderr}"
    );
    // Check every retained Task run, including missing inputs and the policy refusal.
    for task in &consumer.receipts {
        for output in task["outputs"].as_object().unwrap().values() {
            for run in output["streams"].as_array().unwrap() {
                let stderr = &run["stderr"];
                if stderr.is_object() {
                    assert_eq!(stderr["retained"], "whole");
                    for part in ["head", "tail"] {
                        assert!(
                            !stderr[part]
                                .as_str()
                                .unwrap()
                                .contains("kernel-measure setup: "),
                            "{} printed a build setup record",
                            task["task_id"]
                        );
                    }
                }
            }
        }
    }
    // The deliberate missing-inputs build can stage a source copy before Cargo fails. It
    // cannot produce the probe binary or the successful-build source record.
    let cache = consumer.root.join("cache/af/task-build-cache");
    for project in std::fs::read_dir(&cache).unwrap().flatten() {
        if !project.path().is_dir() {
            continue;
        }
        for key in std::fs::read_dir(project.path()).unwrap().flatten() {
            let target = key.path().join("cargo_target");
            assert!(!target.join("release/examples/perf-probe").exists());
            assert!(!target.join("af-kernel-measure-sources.json").exists());
        }
    }
    let home = warm_cargo_home(&consumer.root);
    // Cargo may create housekeeping directories during the deliberate missing-inputs failure;
    // no locked package archive or cached index entry was seeded into the warm home.
    assert!(!home.join("registry/cache").exists(), "{home:?}");
    for path in workspace_files(&home) {
        assert!(
            !path.ends_with(".crate") && !path.contains("/.cache/"),
            "{path}"
        );
    }
    consumer.export(&digest);
}

#[test]
#[ignore = "manual measurement: three cold release builds of perf-probe, about 20 minutes alone; make perf-kernel-measure"]
fn guarded_kernel_experiments_measure_one_judged_sample() {
    let (mut consumer, canonical, digest) = guarded_consumer();
    // Create the kernel's bound warm-home key before copying offline dependencies into it.
    // The refusal test checks this setup failure; only the judged Task is exported here.
    consumer.start("missing-inputs", json!({"NOTES.md": "unmeasured\n"}), 4);
    consumer.receipts.clear();
    seed_cargo_home(
        &consumer.repo.join("Cargo.lock"),
        &warm_cargo_home(&consumer.root),
    );
    let sleep = format!("    std::thread::sleep(std::time::Duration::from_millis({SLOW_MS}));\n");

    // The disposable slow product revision for the successful comparison only.
    consumer.commit(
        &[(
            CANONICAL,
            canonical.replace(VALIDATE, &format!("{VALIDATE}{sleep}")),
        )],
        "disposable controlled slowdown inside the timed operation",
    );
    let unslow = json!({"replace": [sleep, ""]});

    // 2. The guarded comparison improves with the harness byte-identical. The candidate also
    // edits the staged policy fixture: nonexecuted data the captured bootstrap never reads.
    let outcome = consumer.start(
        "improve",
        json!({
            CANONICAL: unslow,
            STAGED: {"replace": [digest.clone(), format!("sha256:{}", "0".repeat(64))]},
        }),
        0,
    );
    assert_eq!(outcome["result"]["domain_conclusion"], "verified");
    let cas = consumer.cas();
    let baseline = measurement(&cas, &outcome, "baseline");
    let candidate = measurement(&cas, &outcome, "candidate");
    assert_eq!(baseline.command_id, candidate.command_id);
    for measured in [&baseline, &candidate] {
        assert_eq!(outcome_of(&measured.outcome), "passed");
        assert_eq!((measured.runs.len(), measured.repetitions), (3, 3));
        assert!(measured.warm && measured.toolchain_id.is_some());
        assert_eq!(measured.summary["wall_ms"].n, 3);
        for run in &measured.runs {
            // One judged sample per repetition: exactly one report line, nothing summarized.
            let stdout = text(&cas, run.stdout_id.as_ref());
            assert_eq!(stdout.matches("af.measure-report/1").count(), 1, "{stdout}");
            let cache = run.cache.as_ref().expect("the bound target is observed");
            assert!(cache.warm, "{cache:?}");
            let stderr = text(&cas, run.stderr_id.as_ref());
            // The guard verified the harness before anything was built or sampled.
            let guard = stderr
                .find(&format!("protected files match {digest}"))
                .expect("the guard's line");
            assert!(
                guard < stderr.find("kernel-measure setup: ").unwrap(),
                "{stderr}"
            );
            let observed = setup(&stderr);
            assert_eq!(observed["target"], "kernel_bound");
            assert_eq!(observed["instance_id"], INSTANCE);
        }
        // The first repetition of each source rebuilt the workspace's own packages from it,
        // whatever the target held; later ones found that exact build.
        let first = setup(&text(&cas, measured.runs[0].stderr_id.as_ref()));
        assert_ne!(first["sources_before"], first["sources"]);
        for run in &measured.runs[1..] {
            let observed = setup(&text(&cas, run.stderr_id.as_ref()));
            assert_eq!(
                (
                    observed["state_before"].as_str(),
                    observed["probe_rebuilt"].as_bool()
                ),
                (Some("populated"), Some(false))
            );
            assert_eq!(observed["sources_before"], first["sources"]);
            assert_eq!(observed["workspace_fingerprints_invalidated"], 0);
        }
    }
    let slow = review_core::task::measurement::MeasureDecimal::from_u64(SLOW_MS);
    assert!(baseline.summary["wall_ms"].median >= slow);
    assert!(candidate.summary["wall_ms"].median < slow);
    let recorded = comparison(&cas, &outcome);
    assert_eq!(outcome_of(&recorded.outcome), "passed");
    assert_eq!(
        recorded.metrics["elapsed_ms"].conclusion.as_str(),
        "improved"
    );
    assert!(attempted(&cas, &outcome, EVALUATE));
    // The guarded inner metric folds the same two receipts to an improvement too; no staged
    // objective selects it until the guard is accepted.
    let inner: MeasureObjectiveV1 = serde_json::from_value(json!({
        "measure": MEASURE, "metric": "wall_ms", "direction": "lower",
        "min_improvement_ratio": "0.1", "min_repetitions": 3
    }))
    .unwrap();
    let ids = |port: &str| {
        outcome["result"]["outputs"][port]["artifact_ids"][0]
            .as_str()
            .unwrap()
            .to_string()
    };
    let folded = compare_measurements(
        &recorded.plan_id,
        &recorded.policy_id,
        (&ids("baseline"), &baseline),
        (&ids("candidate"), &candidate),
        ComparisonObjective {
            name: "perf_canonical_validate_small_inner",
            objective: &inner,
        },
    )
    .unwrap();
    assert_eq!(folded.metrics["wall_ms"].conclusion.as_str(), "improved");

    // These candidate refusals keep round 1's measured, smaller baseline.
    let refusal_sleep =
        format!("    std::thread::sleep(std::time::Duration::from_millis({REFUSAL_SLOW_MS}));\n");
    consumer.commit(
        &[(
            CANONICAL,
            canonical.replace(VALIDATE, &format!("{VALIDATE}{refusal_sleep}")),
        )],
        "disposable smaller slowdown for candidate refusals",
    );
    let unslow = json!({"replace": [refusal_sleep, ""]});
    // The candidate-side refusals need a measured baseline and therefore run manually.
    let workspace = workspace();
    let golden =
        std::fs::read_to_string(workspace.join("fixtures/performance/golden/native.json")).unwrap();
    let entry: Value = serde_json::from_str::<Value>(&golden).unwrap()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["instance_id"] == INSTANCE)
        .unwrap()
        .clone();
    let expected = entry["digest"].as_str().unwrap().to_string();
    let build_marker = consumer.markers.join("build-rs-ran");
    let case_marker = consumer.markers.join("case-variant-build-rs-ran");
    let bytecode_marker = consumer.markers.join("bytecode-imported");
    let (bytecode_path, bytecode) = planted_bytecode(&consumer.root, &bytecode_marker);
    let tampered: Vec<(&str, Value, &str)> = vec![
        (
            "timing-boundary",
            json!({
                CANONICAL: unslow,
                "crates/review-pipeline/examples/perf-probe/cases.rs": {"replace": [
                    ".and_then(|envelope| review_store::validate_envelope(&envelope).map(|()| envelope))",
                    ".and_then(|envelope| Ok(envelope))"
                ]},
            }),
            "harness of",
        ),
        (
            "golden-fixture",
            json!({
                CANONICAL: unslow,
                "fixtures/performance/golden/native.json": {"replace": [
                    expected.clone(), format!("sha256:{}", "1".repeat(64))
                ]},
            }),
            "harness of",
        ),
        (
            "build-inputs",
            json!({
                CANONICAL: unslow,
                ".cargo/config.toml": "[build]\nrustflags = [\"-Cdebug-assertions=off\"]\n",
                "crates/review-store/build.rs": format!(
                    "fn main() {{ std::fs::write({:?}, b\"ran\").unwrap(); }}\n",
                    build_marker.display().to_string()
                ),
                "crates/review-store/examples/extra/Cargo.toml": "[package]\nname = \"extra\"\n",
            }),
            "harness of",
        ),
        (
            // Effective on a case-insensitive filesystem, where Cargo runs BUILD.RS as the
            // package's build script and reads .CaRgO/config.toml; the guard folds case.
            "case-variant-build-inputs",
            json!({
                CANONICAL: unslow,
                ".CaRgO/config.toml": "[build]\nrustflags = [\"-Cdebug-assertions=off\"]\n",
                "crates/review-store/BUILD.RS": format!(
                    "fn main() {{ std::fs::write({:?}, b\"ran\").unwrap(); }}\n",
                    case_marker.display().to_string()
                ),
            }),
            "harness of",
        ),
        (
            "symlink-to-file",
            json!({CANONICAL: unslow, "crates/lockfile": {"symlink": "../Cargo.lock"}}),
            "crates/lockfile is a symbolic link",
        ),
        (
            // Sealing leaves bytecode out of a candidate unless the tree stages it (ADR-0207);
            // this candidate stages it, so the guard is what refuses it.
            "bytecode",
            json!({
                CANONICAL: unslow,
                bytecode_path: {"base64": base64(&bytecode)},
                ".af/seal-ignore": "!__pycache__/\n!*.pyc\n",
            }),
            "harness of",
        ),
    ];
    for (task, files, why) in tampered {
        let outcome = consumer.start(task, files, 4);
        let cas = consumer.cas();
        let baseline = measurement(&cas, &outcome, "baseline");
        assert_eq!(outcome_of(&baseline.outcome), "passed", "{task}");
        let candidate = measurement(&cas, &outcome, "candidate");
        assert_eq!(outcome_of(&candidate.outcome), "failed", "{task}");
        let failure = candidate.failure.as_ref().unwrap();
        assert_eq!(
            (failure.repetition, failure.reason.as_str()),
            (1, "exit"),
            "{task}"
        );
        assert_eq!(candidate.runs.len(), 1, "{task}");
        assert_eq!(candidate.runs[0].exit_code, Some(2), "{task}");
        let stderr = text(&cas, candidate.runs[0].stderr_id.as_ref());
        assert!(
            stderr.contains(REFUSED) && stderr.contains(why),
            "{task}: {stderr}"
        );
        assert!(
            !stderr.contains("kernel-measure setup"),
            "{task}: nothing was built"
        );
        assert!(
            text(&cas, candidate.runs[0].stdout_id.as_ref()).is_empty(),
            "{task}"
        );
        let recorded = comparison(&cas, &outcome);
        assert_eq!(outcome_of(&recorded.outcome), "inconclusive", "{task}");
        assert!(!attempted(&cas, &outcome, EVALUATE), "{task}");
    }
    for marker in [&build_marker, &case_marker, &bytecode_marker] {
        assert!(
            !marker.exists(),
            "{} ran before the guard refused",
            marker.display()
        );
    }
    // These malformed reports require a successful sample before policy validation.
    let policy = std::fs::read_to_string(consumer.repo.join(".af/code-policy.toml")).unwrap();
    let block = format!("[[measures.{MEASURE}.metrics]]\nkey = \"wall_ms\"\nunit = \"ms\"\n");
    assert!(policy.contains(&block));
    for (task, edited, reason, detail) in [
        (
            "wrong-unit",
            policy.replace(
                &block,
                &format!("[[measures.{MEASURE}.metrics]]\nkey = \"wall_ms\"\nunit = \"count\"\n"),
            ),
            "unit_mismatch",
            "unit ms",
        ),
        (
            "undeclared-key",
            policy.replace(
                &block,
                &format!("[[measures.{MEASURE}.metrics]]\nkey = \"region_ms\"\nunit = \"ms\"\n"),
            ),
            "malformed_report",
            "the measure declares",
        ),
    ] {
        consumer.commit(&[(".af/code-policy.toml", edited)], task);
        let outcome = consumer.start(task, json!({"NOTES.md": "unmeasured\n"}), 4);
        let cas = consumer.cas();
        let baseline = measurement(&cas, &outcome, "baseline");
        assert_eq!(outcome_of(&baseline.outcome), "failed", "{task}");
        let failure = baseline.failure.as_ref().unwrap();
        assert_eq!(failure.reason.as_str(), reason, "{task}");
        assert!(
            failure.detail.contains(detail),
            "{task}: {}",
            failure.detail
        );
        assert!(baseline.summary.is_empty(), "{task}");
    }
    consumer.export(&digest);
}

/// The receipts reader's identities are the kernel's own (ADR-0187): every envelope of the
/// published valid fixture, whose IDs the laboratory computed in Python, passes
/// `review_store::validate_envelope`, and each whole stream's ID is its `blob_content_id`; the
/// fixtures that change a payload or a producer under the original IDs fail it, as the reader
/// refuses them.
#[test]
fn receipts_fixtures_carry_the_kernel_s_own_envelope_and_stream_identities() {
    let directory = workspace().join("fixtures/performance/schema/kernel-receipts");
    let read = |name: &str| -> Value {
        serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap()
    };
    let envelopes = |document: &Value| -> Vec<Value> {
        document["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|task| task["outputs"].as_object().unwrap().values())
            .map(|output| output["envelope"].clone())
            .collect()
    };
    let valid = read("valid-fixture.json");
    schemas::valid(
        &schemas::validator("performance-kernel-receipts-v3.json"),
        &valid,
    );
    let all = envelopes(&valid);
    assert_eq!(all.len(), 4);
    for envelope in &all {
        let typed: review_core::ArtifactEnvelope =
            serde_json::from_value(envelope.clone()).unwrap();
        review_store::validate_envelope(&typed).unwrap();
        assert_eq!(serde_json::to_value(&typed).unwrap(), *envelope);
    }
    let mut streams = 0;
    for task in valid["tasks"].as_array().unwrap() {
        for output in task["outputs"].as_object().unwrap().values() {
            for entry in output["streams"].as_array().unwrap() {
                for name in ["stdout", "stderr"] {
                    let stream = &entry[name];
                    assert_eq!(stream["retained"], "whole");
                    let bytes = format!(
                        "{}{}",
                        stream["head"].as_str().unwrap(),
                        stream["tail"].as_str().unwrap()
                    );
                    assert_eq!(
                        review_store::canonical::blob_content_id(bytes.as_bytes()),
                        stream["id"].as_str().unwrap()
                    );
                    streams += 1;
                }
            }
        }
    }
    assert_eq!(streams, 12);
    for (name, error) in [
        (
            "invalid-mutated-payload.json",
            "content_id disagrees with its payload",
        ),
        (
            "invalid-mutated-provenance.json",
            "artifact_id disagrees with its provenance",
        ),
    ] {
        let changed = envelopes(&read(name));
        let refused: Vec<String> = changed
            .iter()
            .filter_map(|envelope| {
                let typed: review_core::ArtifactEnvelope =
                    serde_json::from_value(envelope.clone()).unwrap();
                review_store::validate_envelope(&typed).err()
            })
            .collect();
        assert_eq!(refused.len(), 1, "{name}: {refused:?}");
        assert!(refused[0].contains(error), "{name}: {refused:?}");
    }
}

/// Every envelope and Task result of a receipts document that the kernel's own types refuse:
/// each envelope through `review_store::validate_envelope` (its `ArtifactEnvelope::validate`
/// first), each result through its `TaskResultV1` deserialization, whose sets are strictly
/// ascending, and then `TaskResultV1::validate`. Independently of those checks, every
/// envelope's two IDs are the kernel's own of its content.
fn kernel_refusals(document: &Value) -> Vec<(String, String)> {
    let mut refused = Vec::new();
    for (index, task) in document["tasks"].as_array().unwrap().iter().enumerate() {
        let result =
            serde_json::from_value::<review_core::task::TaskResultV1>(task["result"].clone())
                .map_err(|error| error.to_string())
                .and_then(|result| result.validate());
        if let Err(error) = result {
            refused.push((format!("tasks[{index}].result"), error));
        }
        for (port, output) in task["outputs"].as_object().unwrap() {
            let typed: review_core::ArtifactEnvelope =
                serde_json::from_value(output["envelope"].clone()).unwrap();
            assert_eq!(
                review_store::canonical::content_id(&typed.payload).unwrap(),
                typed.content_id,
                "tasks[{index}].{port}"
            );
            assert_eq!(
                review_store::canonical::artifact_id(&typed).unwrap(),
                typed.artifact_id,
                "tasks[{index}].{port}"
            );
            if let Err(error) = review_store::validate_envelope(&typed) {
                refused.push((format!("tasks[{index}].{port}"), error));
            }
        }
    }
    refused
}

/// The receipts reader refuses what the kernel itself refuses (ADR-0188). Each fixture below
/// keeps every identity the kernel recomputes, and exactly the named envelope or Task result
/// fails its own `review_core` type's validation, with the kernel's message; the valid fixture
/// has no such refusal. The whitespace a blank name may hold is Rust's `char::is_whitespace`,
/// and a whole retained stream is never longer than the exporter below keeps whole.
#[test]
fn receipts_fixtures_refuse_what_the_kernel_s_own_types_refuse() {
    let directory = workspace().join("fixtures/performance/schema/kernel-receipts");
    let read = |name: &str| -> Value {
        serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap()
    };
    assert_eq!(kernel_refusals(&read("valid-fixture.json")), []);
    for (name, place, error) in [
        (
            "invalid-blank-producer.json",
            "tasks[0].baseline",
            "invalid Attempt producer",
        ),
        (
            "invalid-blank-operation.json",
            "tasks[0].comparison",
            "invalid kernel producer",
        ),
        (
            "invalid-blank-conclusion.json",
            "tasks[1].result",
            "needs a domain conclusion",
        ),
        (
            "invalid-satisfied-incomplete.json",
            "tasks[0].result",
            "Satisfied acceptance requires completed execution",
        ),
        (
            "invalid-empty-output.json",
            "tasks[1].result",
            "requires an exact artifact ID",
        ),
        (
            "invalid-output-cardinality.json",
            "tasks[0].result",
            "requires an exact artifact ID",
        ),
        (
            "invalid-unsorted-evidence.json",
            "tasks[0].result",
            "set entries must be sorted and unique",
        ),
        (
            "invalid-unsorted-missing-obligations.json",
            "tasks[1].result",
            "set entries must be sorted and unique",
        ),
        (
            "invalid-duplicate-evidence.json",
            "tasks[0].result",
            "set entries must be sorted and unique",
        ),
        (
            "invalid-duplicate-missing-obligations.json",
            "tasks[1].result",
            "set entries must be sorted and unique",
        ),
    ] {
        let refused = kernel_refusals(&read(name));
        assert_eq!(refused.len(), 1, "{name}: {refused:?}");
        assert_eq!(refused[0].0, place, "{name}: {refused:?}");
        assert!(refused[0].1.contains(error), "{name}: {refused:?}");
    }
    // An empty binding of cardinality `many` passes its own check and is refused as empty.
    let mut empty = read("valid-fixture.json");
    empty["tasks"][1]["result"]["outputs"]["snapshot"]["artifact_ids"] = json!([]);
    empty["tasks"][1]["result"]["outputs"]["snapshot"]["cardinality"] = json!("many");
    let refused = kernel_refusals(&empty);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].1.contains("must contain an artifact"),
        "{refused:?}"
    );

    // The reader's blank test strips exactly the characters Rust's `trim` strips.
    let rust: Vec<String> = (0..=0x10ffff_u32)
        .filter_map(char::from_u32)
        .filter(|character| character.is_whitespace())
        .map(|character| u32::from(character).to_string())
        .collect();
    let python = Command::new("python3")
        .args([
            "-B",
            "-c",
            "import sys; sys.path.insert(0, sys.argv[1]); from perflab import contracts; \
             print(' '.join(str(ord(c)) for c in sorted(contracts._KERNEL_WHITESPACE)))",
        ])
        .arg(workspace().join("scripts/performance"))
        .output()
        .unwrap();
    assert!(
        python.status.success(),
        "{}",
        String::from_utf8_lossy(&python.stderr)
    );
    assert_eq!(
        String::from_utf8(python.stdout)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        rust
    );
    assert!(!"\u{1f}".trim().is_empty());

    // The exporter keeps a stream of twice a part whole, split after the first part, and cuts
    // one byte more to its first and last part, in either encoding.
    let binary: Vec<u8> = (0..=255_u8).cycle().take(2 * STREAM_PART + 1).collect();
    let accent = "é".repeat(STREAM_PART).into_bytes();
    for (bytes, retained, encoding, head, tail, omitted) in [
        (
            vec![b'x'; 2 * STREAM_PART],
            "whole",
            "utf-8",
            STREAM_PART,
            STREAM_PART,
            0,
        ),
        (
            vec![b'x'; 2 * STREAM_PART + 1],
            "head_and_tail",
            "utf-8",
            STREAM_PART,
            STREAM_PART,
            1,
        ),
        (accent, "whole", "utf-8", STREAM_PART, STREAM_PART, 0),
        (binary, "head_and_tail", "base64", 43692, 43692, 1),
    ] {
        let id = review_store::canonical::blob_content_id(&bytes);
        let kept = retained_bytes(&id, &bytes);
        assert_eq!(kept["retained"], retained, "{}", bytes.len());
        assert_eq!(kept["encoding"], encoding, "{}", bytes.len());
        assert_eq!(kept["head"].as_str().unwrap().len(), head);
        assert_eq!(kept["tail"].as_str().unwrap().len(), tail);
        assert_eq!(kept["omitted_bytes"], omitted);
    }
    // The reader's over-bound fixture: a self-consistent whole stream one byte longer than any
    // the exporter keeps whole.
    let fixture = read("invalid-stream-part-bytes.json");
    let stream = &fixture["tasks"][0]["outputs"]["baseline"]["streams"][0]["stdout"];
    let bytes = format!(
        "{}{}",
        stream["head"].as_str().unwrap(),
        stream["tail"].as_str().unwrap()
    )
    .into_bytes();
    assert_eq!(bytes.len(), 2 * STREAM_PART + 1);
    assert_eq!(stream["retained"], "whole");
    let id = stream["id"].as_str().unwrap();
    assert_eq!(review_store::canonical::blob_content_id(&bytes), id);
    assert_eq!(retained_bytes(id, &bytes)["retained"], "head_and_tail");
    assert_eq!(kernel_refusals(&fixture), []);
}

/// The receipts reader admits a Task result's two sets exactly as the kernel deserializes them
/// (ADR-0189): `unique_set` reads `evidence` and `missing_obligations` only in strictly
/// ascending `String` order, and an output's `artifact_ids` is a `Vec` in any order. Each
/// variant of the valid fixture below changes only a result, so every envelope and reference
/// stays the kernel's own; the laboratory's reader and `review_core`'s own types must agree on
/// every one.
#[test]
fn receipts_reader_admits_result_sets_as_the_kernel_deserializes_them() {
    let valid: Value = serde_json::from_slice(
        &std::fs::read(
            workspace().join("fixtures/performance/schema/kernel-receipts/valid-fixture.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let evidence: Vec<Value> = valid["tasks"][0]["result"]["evidence"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(evidence.len(), 4);
    let result = |task: usize, name: &str, items: Value| -> Value {
        let mut document = valid.clone();
        document["tasks"][task]["result"][name] = items;
        document
    };
    let reversed: Vec<Value> = evidence.iter().rev().cloned().collect();
    let repeated: Vec<Value> = std::iter::once(evidence[0].clone())
        .chain(evidence.iter().cloned())
        .collect();
    // Three distinct output IDs in descending order, with the evidence still ascending.
    let mut many = valid.clone();
    let ids: Vec<String> = ["c", "b", "a"]
        .iter()
        .map(|label| format!("sha256:{}", label.repeat(64)))
        .collect();
    many["tasks"][0]["result"]["outputs"]["snapshot"] =
        json!({"artifact_ids": ids, "artifact_type": "af/SourceTree@1", "cardinality": "many"});
    let mut widened: Vec<String> = evidence
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .chain(ids.iter().cloned())
        .collect();
    widened.sort();
    many["tasks"][0]["result"]["evidence"] = json!(widened);
    let cases = [
        ("valid-sorted-control", valid.clone(), true),
        ("empty-evidence", result(1, "evidence", json!([])), true),
        (
            "singleton-evidence",
            result(1, "evidence", json!([evidence[0]])),
            true,
        ),
        (
            "reversed-evidence",
            result(0, "evidence", json!(reversed)),
            false,
        ),
        (
            "duplicate-evidence",
            result(0, "evidence", json!(repeated)),
            false,
        ),
        (
            "sorted-missing-obligations",
            result(1, "missing_obligations", json!(["measured", "verified"])),
            true,
        ),
        (
            "byte-ordered-missing-obligations",
            result(
                1,
                "missing_obligations",
                json!(["Zeta", "a-b", "a_b", "alpha"]),
            ),
            true,
        ),
        (
            "reversed-missing-obligations",
            result(1, "missing_obligations", json!(["verified", "measured"])),
            false,
        ),
        (
            "case-folded-missing-obligations",
            result(1, "missing_obligations", json!(["alpha", "Zeta"])),
            false,
        ),
        (
            "duplicate-missing-obligations",
            result(1, "missing_obligations", json!(["verified", "verified"])),
            false,
        ),
        ("unsorted-many-vector-remains-valid", many, true),
    ];
    let documents: Vec<&Value> = cases.iter().map(|(_, document, _)| document).collect();
    let python = Command::new("python3")
        .args([
            "-B",
            "-c",
            "import json, sys; sys.path.insert(0, sys.argv[1]); from perflab import contracts\n\
             for doc in json.loads(sys.stdin.read()):\n\
             \x20   try:\n\
             \x20       contracts.validate_kernel_receipts(doc)\n\
             \x20       print('accepted')\n\
             \x20   except contracts.ContractError as error:\n\
             \x20       print('refused', str(error).replace('\\n', ' '))",
        ])
        .arg(workspace().join("scripts/performance"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write as _;
        let mut stdin = python.stdin.as_ref().unwrap();
        stdin
            .write_all(&serde_json::to_vec(&documents).unwrap())
            .unwrap();
    }
    let python = python.wait_with_output().unwrap();
    assert!(
        python.status.success(),
        "{}",
        String::from_utf8_lossy(&python.stderr)
    );
    let verdicts = String::from_utf8(python.stdout).unwrap();
    let verdicts: Vec<&str> = verdicts.lines().collect();
    assert_eq!(verdicts.len(), cases.len(), "{verdicts:?}");
    for ((name, document, accepted), verdict) in cases.iter().zip(&verdicts) {
        let refused = kernel_refusals(document);
        assert_eq!(refused.is_empty(), *accepted, "{name}: {refused:?}");
        assert_eq!(
            verdict.starts_with("accepted"),
            *accepted,
            "{name}: {verdict}"
        );
        if !accepted {
            assert_eq!(refused.len(), 1, "{name}: {refused:?}");
            assert!(
                refused[0]
                    .1
                    .contains("set entries must be sorted and unique"),
                "{name}: {refused:?}"
            );
            assert!(verdict.contains(".result."), "{name}: {verdict}");
        }
    }
}
