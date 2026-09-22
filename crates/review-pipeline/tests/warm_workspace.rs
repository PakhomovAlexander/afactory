//! Worker warm layers, package P3: a reviewer node with `warm = { workspace = "rebase" }` keeps
//! one stable template root per Campaign, materialized in full once, re-based to the next head
//! by tree diff and verified against its Tree Digest, reused untouched on an unchanged head, and
//! rebuilt from the CAS with a recorded reason when the root cannot be trusted. Per-Attempt
//! sandboxes stay fresh clones, so the sealed diff is what the reviewer wrote and nothing else.

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use review_check::{Arg, Command};
use review_config::Definition;
use review_core::event::AttemptAdmittedPayloadV1;
use review_core::{
    CampaignOpenedPayloadV1, EventType, RoundStartedPayloadV1, RunEvent, SubjectV1, WarmLayerV1,
    WarmSetSelectedPayloadV1, WarmSetV1, WorkspaceBasisV1, WorkspaceFallbackReasonV1,
    WorkspaceRebasedPayloadV1, is_workspace_id,
};
use review_pipeline::{Kernel, RoundAuthority};
use review_sandbox::{RecordedPreparation, WorkspaceRoot, prepare_workspace, workspace_id};
use review_source_git::{Entry, EntryKind, Manifest, materialize, scan_tree};
use review_store::{Cas, ConvergencePolicy, EventStore, NewEvent};

const WARM_WORKSPACE_PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = ["result"]
warm = { notes = false, workspace = "rebase" }
runner = { program = "/bin/true" }
[[nodes]]
id = "reader"
kind = "reviewer"
outputs = ["result"]
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = ["reviewer", "reader"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "gather", port = "reviewer" }
[[edges]]
from = { node = "reader", port = "result" }
to = { node = "gather", port = "reader" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

const COLD_PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = ["result"]
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = ["reviewer"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "gather", port = "reviewer" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

const APPROVE: &str =
    r#"'{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}'"#;

fn shell(script: &str) -> Command {
    Command::new(
        "/bin/sh",
        vec![Arg::literal("-c"), Arg::literal(script.to_string())],
    )
}

/// A reviewer that checks the tree it was given and answers cleanly.
fn checking_reviewer(checks: &str) -> Command {
    shell(&format!("{checks} && printf '%s\\n' {APPROVE}"))
}

/// A reviewer that checks the second head, edits one file and answers cleanly.
fn editing_reviewer() -> Command {
    checking_reviewer(
        "test \"$(cat a.rs)\" = uno && test ! -e b.rs && test \"$(cat d.rs)\" = four \
         && test \"$(cat src/deep/c.rs)\" = three && printf edited > edited.txt",
    )
}

fn reading_reviewer() -> Command {
    checking_reviewer("test -f a.rs")
}

fn manifest(cas: &Cas, files: &[(&str, &str)]) -> Manifest {
    Manifest::new(
        files
            .iter()
            .map(|(path, text)| Entry {
                path: (*path).into(),
                kind: EntryKind::File,
                content: cas.put(text.as_bytes()).unwrap(),
                size: text.len() as u64,
            })
            .collect(),
    )
    .unwrap()
}

fn head_one(cas: &Cas) -> Manifest {
    manifest(
        cas,
        &[
            ("a.rs", "one\n"),
            ("b.rs", "two\n"),
            ("src/deep/c.rs", "three\n"),
        ],
    )
}

fn head_two(cas: &Cas) -> Manifest {
    manifest(
        cas,
        &[
            ("a.rs", "uno\n"),
            ("d.rs", "four\n"),
            ("src/deep/c.rs", "three\n"),
        ],
    )
}

fn snapshot_id(cas: &Cas, manifest: &Manifest, tree: &str) -> String {
    let manifest_value = serde_json::to_value(manifest).unwrap();
    let manifest_id = cas.put_json(&manifest_value).unwrap();
    cas.put_json(&serde_json::json!({
        "repository_id": "test/repository",
        "vcs": "git",
        "capture": { "kind": "committed", "tree_id": tree },
        "content_digest": manifest.content_digest(),
        "source_revision": tree,
        "artifact_manifest": manifest_id,
    }))
    .unwrap()
}

/// Open the next Round on a new head Snapshot of the same whole-tree Campaign, exactly as the
/// authority layer does: fresh Subject, fresh RoundStarted@1.
fn start_round(cas: &Cas, store: &mut EventStore, head: &Manifest, round: u32) -> RunEvent {
    let opened_event = store.campaign_opened("run").unwrap().unwrap();
    let opened: CampaignOpenedPayloadV1 =
        serde_json::from_value(opened_event.payload.clone()).unwrap();
    let head_id = snapshot_id(cas, head, &format!("test-tree-{round}"));
    let subject = SubjectV1::whole_tree(&head_id);
    let subject_id = cas
        .put_json(&serde_json::to_value(&subject).unwrap())
        .unwrap();
    let prior_findings = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id,
            "round": round,
            "prior_findings": [],
        }))
        .unwrap();
    let prior_demands = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id,
            "round": round,
            "demands": [],
        }))
        .unwrap();
    let payload = RoundStartedPayloadV1 {
        round,
        epoch: 1,
        campaign_manifest_id: opened.campaign_manifest_id.clone(),
        subject_id: subject_id.clone(),
        prior_finding_set_id: prior_findings.clone(),
        prior_demand_set_id: prior_demands.clone(),
    };
    let event = store
        .append(
            "run",
            cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(&payload).unwrap(),
            )
            .caused_by(opened_event.event_id)
            .correlating(subject_id.clone())
            .referencing(vec![
                opened.authority_snapshot_id,
                opened.campaign_manifest_id,
                head_id,
                subject_id,
                prior_findings,
                prior_demands,
            ]),
        )
        .unwrap();
    store
        .append(
            "run",
            cas,
            NewEvent::new(
                EventType::GenerationAdvancedV1,
                serde_json::json!({ "round": round }),
            )
            .caused_by(event.event_id.clone()),
        )
        .unwrap();
    event
}

fn events_of(store: &EventStore, event_type: EventType) -> Vec<RunEvent> {
    store
        .replay("run")
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == event_type)
        .collect()
}

fn rebased(event: &RunEvent) -> WorkspaceRebasedPayloadV1 {
    let value = event.payload.clone();
    let payload: WorkspaceRebasedPayloadV1 = serde_json::from_value(value).unwrap();
    payload.validate().unwrap();
    payload
}

fn selected(event: &RunEvent) -> WarmSetSelectedPayloadV1 {
    let selection: WarmSetSelectedPayloadV1 =
        serde_json::from_value(event.payload.clone()).unwrap();
    selection.validate().unwrap();
    selection
}

/// The Warm Set artifact a selection names.
fn warm_set(cas: &Cas, selection: &WarmSetSelectedPayloadV1) -> WarmSetV1 {
    let set: WarmSetV1 = serde_json::from_value(
        cas.get_artifact(&selection.warm_set_artifact_id)
            .unwrap()
            .payload,
    )
    .unwrap();
    set.validate().unwrap();
    set
}

/// The sealed mutation set and provenance of `node`'s admitted Attempt in one Round.
fn sealed_mutations(
    cas: &Cas,
    store: &EventStore,
    round_event_id: &str,
    node: &str,
) -> (serde_json::Value, serde_json::Value) {
    let admitted = events_of(store, EventType::AttemptAdmittedV1)
        .into_iter()
        .find(|event| {
            event.node_id.as_deref() == Some(node)
                && event.causation_id.as_deref() == Some(round_event_id)
        })
        .expect("admitted Attempt");
    let payload: AttemptAdmittedPayloadV1 = serde_json::from_value(admitted.payload).unwrap();
    assert_eq!(payload.selection, "selected");
    let provenance = cas
        .get_json(payload.provenance_artifact.as_deref().unwrap())
        .unwrap();
    let artifact = provenance["sandbox_mutations"]["artifact"]
        .as_str()
        .unwrap()
        .to_string();
    (cas.get_json(&artifact).unwrap(), provenance)
}

/// Every regular file below `root`, keyed by relative path.
fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let key = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(key, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Round one of a warm-workspace Campaign: the reviewer sees the first head, the Round closes,
/// and the node's root is materialized in full because nothing existed to re-base.
fn run_round_one(
    cas: &Cas,
    store: &mut EventStore,
    workspaces: &Path,
    loaded: &review_config::Loaded,
    head: &Manifest,
) -> (String, WorkspaceRebasedPayloadV1) {
    let authority = support::test_round_authority_for_pipeline(
        cas,
        store,
        "run",
        head,
        WARM_WORKSPACE_PIPELINE,
    );
    let head_id = authority.head_snapshot_id().to_string();
    let kernel = Kernel::from_loaded(cas, store, "run", head.clone(), loaded, authority)
        .unwrap()
        .with_workspace_cache_root(workspaces.to_path_buf())
        .with_reviewer(
            "reviewer",
            checking_reviewer(
                "test \"$(cat a.rs)\" = one && test \"$(cat src/deep/c.rs)\" = three",
            ),
        )
        .with_reviewer("reader", reading_reviewer());
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();
    drop(kernel);

    let events = events_of(store, EventType::WorkspaceRebasedV1);
    assert_eq!(events.len(), 1, "one preparation per warm node per run");
    assert_eq!(events[0].node_id.as_deref(), Some("reviewer"));
    let first = rebased(&events[0]);
    assert_eq!(first.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        first.fallback,
        Some(WorkspaceFallbackReasonV1::NoVerifiedTemplate),
        "the first Round has no template to re-base and says so"
    );
    assert_eq!(first.from_snapshot_id, None);
    assert_eq!(first.to_snapshot_id, head_id);
    assert_eq!(first.verified_digest, head.content_digest());
    assert_eq!(first.entries_touched, 3);
    assert!(is_workspace_id(&first.workspace_id));
    assert!(events[0].artifact_refs.contains(&head_id));

    let selections = events_of(store, EventType::WarmSetSelectedV1);
    assert_eq!(selections.len(), 1, "the cold node selects nothing");
    assert_eq!(selections[0].node_id.as_deref(), Some("reviewer"));
    assert!(
        events[0].sequence < selections[0].sequence,
        "the workspace is verified before the Warm Set that names it is recorded"
    );
    let first_dispatch = events_of(store, EventType::AttemptDispatchedV1)
        .into_iter()
        .find(|event| event.node_id.as_deref() == Some("reviewer"))
        .expect("the warm node was dispatched");
    assert!(selections[0].sequence < first_dispatch.sequence);
    let selection = selected(&selections[0]);
    assert!(
        selection.layers.is_empty(),
        "a full materialization carries nothing"
    );
    let set = warm_set(cas, &selection);
    assert_eq!(set.round, 1);
    assert_eq!(set.workspace, Some(WorkspaceBasisV1::Full));
    assert_eq!(
        set.workspace_id.as_deref(),
        Some(first.workspace_id.as_str())
    );
    let root = workspaces.join(&first.workspace_id).join("tree");
    assert_eq!(scan_tree(root).unwrap(), *head);
    (head_id, first)
}

#[test]
fn a_warm_workspace_is_materialized_once_then_rebased_and_reused_per_head() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let workspaces = dir.path().join("workspaces");
    let loaded = Definition::from_toml(WARM_WORKSPACE_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let head_one = head_one(&cas);
    let (head_one_id, first) = run_round_one(&cas, &mut store, &workspaces, &loaded, &head_one);
    let root = workspaces.join(&first.workspace_id).join("tree");

    // Round two on a new head: a.rs changed, b.rs removed, d.rs added, c.rs untouched. The
    // first Attempt cannot start and is released, but the template was already re-based and
    // verified before the Warm Set that names it was recorded.
    let head_two = head_two(&cas);
    let round_two = start_round(&cas, &mut store, &head_two, 2);
    let head_two_id = snapshot_id(&cas, &head_two, "test-tree-2");
    let authority = RoundAuthority::load(&store, &cas, "run", &round_two.event_id).unwrap();
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        head_two.clone(),
        &loaded,
        authority,
    )
    .unwrap()
    .with_workspace_cache_root(workspaces.clone())
    .with_reviewer("reviewer", Command::new("/nonexistent/reviewer", vec![]))
    .with_reviewer("reader", reading_reviewer());
    let open = loaded.run(&kernel).unwrap();
    assert!(!open.complete(), "{:?}", open.outcomes);
    drop(kernel);

    let events = events_of(&store, EventType::WorkspaceRebasedV1);
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[1].causation_id.as_deref(),
        Some(round_two.event_id.as_str())
    );
    let second = rebased(&events[1]);
    assert_eq!(second.basis, WorkspaceBasisV1::Rebased);
    assert_eq!(second.fallback, None);
    assert_eq!(
        second.from_snapshot_id.as_deref(),
        Some(head_one_id.as_str())
    );
    assert_eq!(second.to_snapshot_id, head_two_id);
    assert_eq!(second.verified_digest, head_two.content_digest());
    assert_eq!(
        second.entries_touched, 3,
        "a.rs modified, b.rs removed, d.rs added"
    );
    assert_eq!(
        second.workspace_id, first.workspace_id,
        "one stable root per node per Campaign"
    );
    let fresh = dir.path().join("fresh");
    materialize(&head_two, &cas, &fresh).unwrap();
    assert_eq!(
        tree_bytes(&root),
        tree_bytes(&fresh),
        "the re-based template is byte-identical to a full materialization"
    );
    assert_eq!(scan_tree(&root).unwrap(), head_two);

    // The resumed Round reads its recorded Warm Set back; the head is unchanged, so the
    // template is reused and nothing is materialized. The Attempt's sandbox is a fresh clone:
    // its edit is sealed and never reaches the stable root.
    let authority = RoundAuthority::load(&store, &cas, "run", &round_two.event_id).unwrap();
    let resumed = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        head_two.clone(),
        &loaded,
        authority,
    )
    .unwrap()
    .with_workspace_cache_root(workspaces.clone())
    .with_reviewer("reviewer", editing_reviewer())
    .with_reviewer("reader", reading_reviewer());
    let report = loaded.run(&resumed).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    drop(resumed);

    let events = events_of(&store, EventType::WorkspaceRebasedV1);
    assert_eq!(events.len(), 3);
    let third = rebased(&events[2]);
    assert_eq!(third.basis, WorkspaceBasisV1::Reused);
    assert_eq!(third.fallback, None);
    assert_eq!(
        third.entries_touched, 0,
        "a Round on an unchanged head materializes nothing"
    );
    assert_eq!(
        third.from_snapshot_id.as_deref(),
        Some(head_two_id.as_str())
    );
    assert_eq!(third.to_snapshot_id, head_two_id);
    assert!(
        events
            .iter()
            .all(|event| event.node_id.as_deref() == Some("reviewer")),
        "a node without the policy keeps a temporary template and records nothing"
    );
    let selections = events_of(&store, EventType::WarmSetSelectedV1);
    assert_eq!(
        selections.len(),
        2,
        "one Warm Set per node per Round, reused on resume"
    );
    let selection = selected(&selections[1]);
    assert_eq!(selection.layers, vec![WarmLayerV1::Workspace]);
    let set = warm_set(&cas, &selection);
    assert_eq!(set.round, 2);
    assert_eq!(set.workspace, Some(WorkspaceBasisV1::Rebased));
    assert_eq!(
        set.workspace_id.as_deref(),
        Some(first.workspace_id.as_str())
    );

    let (mutations, provenance) = sealed_mutations(&cas, &store, &round_two.event_id, "reviewer");
    assert_eq!(
        mutations,
        serde_json::json!({"added": ["edited.txt"], "modified": [], "deleted": []})
    );
    assert!(
        provenance["context_manifest"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "warm_set"),
        "the manifest names the Warm Set that records the workspace basis"
    );
    assert!(
        !root.join("edited.txt").exists(),
        "a sandbox write never reaches the template"
    );
    assert_eq!(tree_bytes(&root), tree_bytes(&fresh));
}

#[test]
fn a_corrupted_template_fails_closed_into_a_full_materialization_that_records_why() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let workspaces = dir.path().join("workspaces");
    let loaded = Definition::from_toml(WARM_WORKSPACE_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let head_one = head_one(&cas);
    let (head_one_id, first) = run_round_one(&cas, &mut store, &workspaces, &loaded, &head_one);
    let root = workspaces.join(&first.workspace_id).join("tree");

    // An entry the next head does not touch changed under an intact marker. The clone is
    // scanned before any entry is unlinked and disagrees with the previous manifest, so the
    // head is rebuilt from the CAS and the reviewer sees the real tree.
    std::fs::write(root.join("src/deep/c.rs"), b"tampered\n").unwrap();
    let head_two = head_two(&cas);
    let round_two = start_round(&cas, &mut store, &head_two, 2);
    let head_two_id = snapshot_id(&cas, &head_two, "test-tree-2");
    let authority = RoundAuthority::load(&store, &cas, "run", &round_two.event_id).unwrap();
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        head_two.clone(),
        &loaded,
        authority,
    )
    .unwrap()
    .with_workspace_cache_root(workspaces.clone())
    .with_reviewer("reviewer", editing_reviewer())
    .with_reviewer("reader", reading_reviewer());
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    drop(kernel);

    let events = events_of(&store, EventType::WorkspaceRebasedV1);
    assert_eq!(events.len(), 2);
    let second = rebased(&events[1]);
    assert_eq!(second.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        second.fallback,
        Some(WorkspaceFallbackReasonV1::TemplateCorrupt),
        "the drifted template is refused before the rebase and the reason recorded"
    );
    assert_eq!(
        second.from_snapshot_id.as_deref(),
        Some(head_one_id.as_str())
    );
    assert_eq!(second.to_snapshot_id, head_two_id);
    assert_eq!(second.verified_digest, head_two.content_digest());
    assert_eq!(second.entries_touched, 3);
    let fresh = dir.path().join("fresh");
    materialize(&head_two, &cas, &fresh).unwrap();
    assert_eq!(tree_bytes(&root), tree_bytes(&fresh));
    assert_eq!(
        std::fs::read(root.join("src/deep/c.rs")).unwrap(),
        b"three\n"
    );
    let selections = events_of(&store, EventType::WarmSetSelectedV1);
    assert_eq!(selections.len(), 2);
    let selection = selected(&selections[1]);
    assert!(selection.layers.is_empty());
    let set = warm_set(&cas, &selection);
    assert_eq!(set.workspace, Some(WorkspaceBasisV1::Full));
    let (mutations, _) = sealed_mutations(&cas, &store, &round_two.event_id, "reviewer");
    assert_eq!(
        mutations,
        serde_json::json!({"added": ["edited.txt"], "modified": [], "deleted": []})
    );
}

#[test]
fn a_preparation_the_log_never_recorded_is_rebuilt_with_the_recorded_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let workspaces = dir.path().join("workspaces");
    let loaded = Definition::from_toml(WARM_WORKSPACE_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let head_one = head_one(&cas);
    let (head_one_id, first) = run_round_one(&cas, &mut store, &workspaces, &loaded, &head_one);
    assert!(first.preparation_ms < 600_000, "measured, not discarded");

    // Round two's preparation swapped the tree and wrote its marker for the new head, then the
    // process died before `WorkspaceRebased@1` was appended: the root holds head two under a
    // marker the log never recorded.
    let head_two = head_two(&cas);
    let head_two_id = snapshot_id(&cas, &head_two, "test-tree-2");
    let root = WorkspaceRoot::new(&workspaces, &first.workspace_id).unwrap();
    let recorded = RecordedPreparation {
        snapshot_id: first.to_snapshot_id.clone(),
        verified_digest: first.verified_digest.clone(),
    };
    let interrupted =
        prepare_workspace(&root, &head_two, &head_two_id, &cas, Some(&recorded)).unwrap();
    assert_eq!(interrupted.basis, WorkspaceBasisV1::Rebased);

    let round_two = start_round(&cas, &mut store, &head_two, 2);
    let authority = RoundAuthority::load(&store, &cas, "run", &round_two.event_id).unwrap();
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        head_two.clone(),
        &loaded,
        authority,
    )
    .unwrap()
    .with_workspace_cache_root(workspaces.clone())
    .with_reviewer("reviewer", editing_reviewer())
    .with_reviewer("reader", reading_reviewer());
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    drop(kernel);

    let events = events_of(&store, EventType::WorkspaceRebasedV1);
    assert_eq!(events.len(), 2);
    let second = rebased(&events[1]);
    assert_eq!(second.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        second.fallback,
        Some(WorkspaceFallbackReasonV1::UnrecordedPreparation),
        "an unrecorded marker is not reinterpreted as reuse"
    );
    assert_eq!(
        second.from_snapshot_id.as_deref(),
        Some(head_one_id.as_str()),
        "the previous head is the log's, never the marker's"
    );
    assert_eq!(second.to_snapshot_id, head_two_id);
    assert_eq!(second.entries_touched, 3);
    assert_eq!(scan_tree(root.tree()).unwrap(), head_two);
    let (mutations, _) = sealed_mutations(&cas, &store, &round_two.event_id, "reviewer");
    assert_eq!(
        mutations,
        serde_json::json!({"added": ["edited.txt"], "modified": [], "deleted": []})
    );
}

#[test]
fn a_preparation_failure_reaches_the_report_without_a_host_path() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let workspaces = dir.path().join("distinctive-cache-root-7f3a");
    let loaded = Definition::from_toml(WARM_WORKSPACE_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let head = head_one(&cas);
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &head,
        WARM_WORKSPACE_PIPELINE,
    );
    let opened_event = store.campaign_opened("run").unwrap().unwrap();
    let opened: CampaignOpenedPayloadV1 =
        serde_json::from_value(opened_event.payload.clone()).unwrap();
    // The node's root exists but is a symlink: the preparation refuses it.
    let id = workspace_id("run", &opened.campaign_manifest_id, "reviewer");
    std::fs::create_dir_all(&workspaces).unwrap();
    std::fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("elsewhere"), workspaces.join(&id)).unwrap();
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", head.clone(), &loaded, authority)
        .unwrap()
        .with_workspace_cache_root(workspaces.clone())
        .with_reviewer("reviewer", reading_reviewer())
        .with_reviewer("reader", reading_reviewer());
    let report = loaded.run(&kernel).unwrap();
    assert!(!report.complete());
    let outcomes = format!("{:?}", report.outcomes);
    assert!(
        outcomes.contains("warm workspace root is unavailable"),
        "{outcomes}"
    );
    assert!(
        !outcomes.contains("distinctive-cache-root") && !outcomes.contains(&id),
        "no host path in a durable outcome: {outcomes}"
    );
    drop(kernel);
    for event in store.replay("run").unwrap() {
        let bytes = serde_json::to_string(&event).unwrap();
        assert!(
            !bytes.contains("distinctive-cache-root"),
            "no host path in the log: {bytes}"
        );
    }
}

#[test]
fn a_node_without_the_policy_records_nothing_and_touches_no_cache_root() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let workspaces = dir.path().join("workspaces");
    let head = head_one(&cas);
    let kernel =
        support::whole_tree_kernel_for_pipeline(&cas, &mut store, "run", head, None, COLD_PIPELINE)
            .with_workspace_cache_root(workspaces.clone())
            .with_reviewer("reviewer", reading_reviewer());
    let loaded = Definition::from_toml(COLD_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    drop(kernel);
    assert!(events_of(&store, EventType::WorkspaceRebasedV1).is_empty());
    assert!(events_of(&store, EventType::WarmSetSelectedV1).is_empty());
    assert!(
        !workspaces.exists(),
        "a cold node never creates a stable root"
    );
}
