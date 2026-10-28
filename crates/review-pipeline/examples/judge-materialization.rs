//! One judging Attempt's materialization on one tree: materialized from the CAS, as the kernel
//! does, or cloned from warm templates, the wiring #331 item 6 asked for (ADR-0254).
//!
//! `cargo run -p review-pipeline --example judge-materialization -- [DIR] [CHANGED] [JUDGES]`
//! captures the regular files under DIR (default: the `crates/` of `AF_WORKSPACE_ROOT` or of
//! this workspace, `target/` left out) as a base Snapshot, changes CHANGED of them (default 20)
//! in a candidate whose parent is that base, and times JUDGES (default 4) judging Attempts of the candidate each way:
//!
//! - materialized: `SnapshotTaskEnvironment::materialize`, which checks the typed Diff Subject
//!   and materializes the tree and the base from the CAS for each Attempt;
//! - cloned: the same check, then a copy-on-write clone of a tree template and a base template
//!   the first Attempt materialized, each checked unchanged before it is cloned, and every cloned
//!   file stamped now, as a materialization stamps it (#266).
//!
//! The two alternate, so a change of machine load weighs on both alike. Each line is one judge's
//! wall time each way in milliseconds, and the last their sums; nothing is written outside a
//! temporary directory, and no Worker runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use review_core::Producer;
use review_core::task::ArtifactInputV1;
use review_core::task::diff_subject::{DIFF_SUBJECT_OPERATION, DIFF_SUBJECT_PORT, DIFF_SUBJECT_V1};
use review_core::task::execution::TaskInvocationV1;
use review_core::task::pipeline::PipelineContractV1;
use review_graph::task::OperatorSignature;
use review_pipeline::task::host::TaskEnvironment;
use review_pipeline::task::source::{
    SnapshotTaskEnvironment, check_diff_subject, diff_subject, recorded_base, source_snapshot,
};
use review_sandbox::{Mode, Policy, Sandbox, SandboxTemplate};
use review_source_git::task::{capture_snapshot, read_snapshot, source_tree};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::Cas;

fn files(root: &Path, at: &Path, found: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(at)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() && entry.file_name() != "target" {
            files(root, &path, found)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
            if let Some(relative) = relative.to_str() {
                found.push((relative.to_owned(), path));
            }
        }
    }
    Ok(())
}

/// The tree and the base of one judging Attempt cloned from `templates`, materialized by the
/// first Attempt that needed each: what wiring `attach_base_template` would do, with the checks a
/// safe wiring needs.
fn cloned(
    cas: &Cas,
    invocation: &TaskInvocationV1,
    judge: &OperatorSignature,
    templates: &mut BTreeMap<String, Arc<SandboxTemplate>>,
) -> Result<Sandbox, String> {
    let budget = Duration::from_secs(300);
    check_diff_subject(cas, invocation, judge)?;
    let (_, snapshot, manifest) = source_snapshot(cas, &invocation.inputs["source"])?;
    let base_id = recorded_base(cas, &snapshot)?.ok_or("the candidate records no base")?;
    let (_, base) = read_snapshot(cas, &base_id)?;
    let mut template = |manifest: &Manifest| -> Result<Arc<SandboxTemplate>, String> {
        let key = manifest.content_digest();
        if let Some(held) = templates.get(&key)
            && held.intact().map_err(|e| e.to_string())?
        {
            return Ok(held.clone());
        }
        let made =
            Arc::new(SandboxTemplate::materialize(manifest, cas).map_err(|e| e.to_string())?);
        templates.insert(key, made.clone());
        Ok(made)
    };
    let tree = template(&manifest)?;
    let base = template(&base)?;
    let sandbox = Sandbox::from_template(&tree, Mode::ReadOnly).map_err(|e| e.to_string())?;
    sandbox
        .refresh_source_mtimes(None, None, budget)
        .map_err(|e| e.to_string())?;
    review_sandbox::admit(Policy::trusted_local(), &sandbox).map_err(|e| e.to_string())?;
    sandbox
        .attach_base_template(&base, &[], budget)
        .map_err(|e| e.to_string())?;
    Ok(sandbox)
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    // The workspace's `crates/`: AF_WORKSPACE_ROOT when set, else where this was compiled.
    let root = args.next().map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("AF_WORKSPACE_ROOT")
            .map(|root| PathBuf::from(root).join("crates"))
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."))
    });
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    let changed: usize = args
        .next()
        .map_or(Ok(20), |n| n.parse())
        .map_err(|e| format!("CHANGED is a count of files: {e}"))?;
    let judges: usize = args
        .next()
        .map_or(Ok(4), |n| n.parse())
        .map_err(|e| format!("JUDGES is a count of Attempts: {e}"))?;
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    files(&root, &root, &mut found).map_err(|e| e.to_string())?;
    found.sort();
    let mut bytes = 0u64;
    let mut base = Vec::new();
    let mut tree = Vec::new();
    for (index, (path, on_disk)) in found.iter().enumerate() {
        let content = std::fs::read(on_disk).map_err(|e| e.to_string())?;
        bytes += content.len() as u64;
        let entry = |content: &[u8]| -> Result<Entry, String> {
            Ok(Entry {
                path: path.clone(),
                kind: EntryKind::File,
                content: cas.put(content).map_err(|e| e.to_string())?,
                size: content.len() as u64,
            })
        };
        base.push(entry(&content)?);
        if index < changed {
            let mut edited = content.clone();
            edited.extend_from_slice(b"\n// changed by the candidate\n");
            tree.push(entry(&edited)?);
        } else {
            tree.push(entry(&content)?);
        }
    }
    let base = Manifest::new(base).map_err(|e| e.to_string())?;
    let tree = Manifest::new(tree).map_err(|e| e.to_string())?;
    let origin = cas
        .put_json(&serde_json::json!({"schema": "af.task-source-origin/1"}))
        .map_err(|e| e.to_string())?;
    let base_id = capture_snapshot(&cas, &base, &origin, None)?;
    let tree_id = capture_snapshot(&cas, &tree, &origin, Some(&base_id))?;
    let kernel = |node: Option<&str>, operation: &str| Producer::KernelOperation {
        run_id: "task:measurement".into(),
        node_id: node.map(str::to_owned),
        operation_id: operation.into(),
    };
    let source = source_tree(&cas, kernel(None, "task-builtin@1"), &tree_id, vec![])?;
    let node = "root.nodes.review.nodes.bugs";
    let subject = diff_subject(&base_id, &base, &tree_id, &tree)?;
    let (subject_id, _) = cas
        .put_artifact(
            DIFF_SUBJECT_V1,
            kernel(Some(node), DIFF_SUBJECT_OPERATION),
            vec![base_id.clone(), tree_id.clone()],
            Some(tree_id.clone()),
            serde_json::to_value(&subject).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    let invocation = TaskInvocationV1 {
        plan_id: cas.put(b"measurement plan").map_err(|e| e.to_string())?,
        node: node.into(),
        inputs: BTreeMap::from([
            ("source".to_owned(), source),
            (
                DIFF_SUBJECT_PORT.to_owned(),
                ArtifactInputV1 {
                    artifact_ids: vec![subject_id],
                    artifact_type: DIFF_SUBJECT_V1.into(),
                    cardinality: review_core::PortCardinality::One,
                    snapshot_id: Some(tree_id.clone()),
                },
            ),
        ]),
    };
    let contract: PipelineContractV1 = serde_json::from_value(serde_json::json!({
        "inputs": {
            "source": {"artifact_type": "af/SourceTree@1", "cardinality": "one",
                "optional": false, "affinity": {"kind": "unbound"}, "covers": []},
            "diff_subject": {"artifact_type": DIFF_SUBJECT_V1, "cardinality": "one",
                "optional": true, "affinity": {"kind": "same_as", "input": "source"},
                "covers": []}
        },
        "outputs": {}
    }))
    .map_err(|e| e.to_string())?;
    let judge = OperatorSignature {
        unbound_allowed: Default::default(),
        contract,
        effects: ["read-source".to_owned()].into(),
        evidence: BTreeMap::new(),
        retains: BTreeMap::new(),
        roles: ["review".to_owned()].into(),
        worker_input_type: None,
        worker_output_type: None,
        outcome_port: None,
        attempt: None,
    };
    println!(
        "tree: {} files, {bytes} bytes, {changed} changed, {judges} judging Attempts",
        found.len()
    );
    let environment = SnapshotTaskEnvironment {
        policy: Policy::trusted_local(),
    };
    let mut templates = BTreeMap::new();
    let (mut materialized, mut clones) = (Vec::new(), Vec::new());
    for attempt in 1..=judges {
        let started = Instant::now();
        let sandbox = environment.materialize(&cas, &invocation, &judge)?;
        materialized.push(started.elapsed().as_secs_f64() * 1000.0);
        drop(sandbox);
        let started = Instant::now();
        let sandbox = cloned(&cas, &invocation, &judge, &mut templates)?;
        clones.push(started.elapsed().as_secs_f64() * 1000.0);
        drop(sandbox);
        println!(
            "judge {attempt}: materialized {:.1} ms, cloned {:.1} ms",
            materialized[attempt - 1],
            clones[attempt - 1]
        );
    }
    let total = |times: &[f64]| times.iter().sum::<f64>();
    println!(
        "all {judges} judges: materialized {:.1} ms, cloned {:.1} ms",
        total(&materialized),
        total(&clones)
    );
    Ok(())
}
