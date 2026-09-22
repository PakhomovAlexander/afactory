//! Worker warm layers on the Task host: Notes and a Head Delta carried from the previous
//! closed Round's selected Attempt, the Gate's Build Cache cloned into a Worker's sandbox, and
//! a Warm Workspace re-based per head. Every layer is an artifact recorded in the Warm Set
//! before the node's first Attempt of the Round, so a retried Attempt renders the same bytes.

use super::domain::{admit_source, answering, events_of, hosted, requesting, runner};
use super::*;
use review_core::task::review_compat::{TaskReviewContextV1, TaskReviewResultSelectedV1};
use review_core::{
    BuildCacheCapturedPayloadV1, HeadDeltaMarkV1, HeadDeltaV1, WarmLayerV1,
    WarmSetSelectedPayloadV1, WarmSetV1, WorkerNotesRecordedPayloadV1, WorkerNotesV1,
    WorkspaceBasisV1, WorkspaceFallbackReasonV1, WorkspaceRebasedPayloadV1,
};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::store::task::TaskLease;

/// Close the Round `host` executed and continue its Task into the next Round on `head`, as a
/// heavy `af review run` does after the operator changed the tree: a fresh Snapshot and
/// Subject over the same pinned authority, the Ledger's exact prior Finding and Demand Sets,
/// and one ClosedRound handoff. Returns the successor compiler and its Round event.
fn continue_on_head(
    cas: &Cas,
    shared: &SharedEventStore<'_>,
    compiler: &LegacyReviewPlanCompiler,
    host: &LegacyReviewTaskHost<'_, '_>,
    lease: &TaskLease,
    head: &BTreeMap<String, Vec<u8>>,
) -> (LegacyReviewPlanCompiler, String) {
    use review_core::task::review_handoff::{TaskReviewHandoffEvidenceV1, TaskReviewHandoffV1};
    let conclusion = host.publish_recorded_round_conclusion(cas).unwrap();
    assert!(conclusion.can_continue, "{conclusion:?}");
    let mut store = shared.lock().unwrap();
    let state = store
        .task_projection(cas, lease.task_id())
        .unwrap()
        .unwrap();
    let execution = state.execution.as_ref().unwrap();
    let ledger = execution
        .graph
        .nodes
        .iter()
        .find(|(_, node)| {
            matches!(
                node.operator,
                review_graph::task::CompiledOperator::ReviewDomain {
                    operation: review_graph::task::ReviewOperation::Ledger,
                    ..
                }
            )
        })
        .unwrap()
        .0;
    let port = |ty: &str| {
        execution.outputs[ledger]
            .1
            .outputs
            .values()
            .find(|port| port.artifact_type == ty)
            .unwrap()
            .artifact_ids[0]
            .clone()
    };
    let finding_set_id = port(review_core::contract::FINDING_SET_V1);
    let prior_demand_set_id = port(review_core::contract::DEMAND_SET_V1);
    let old = store.latest_round_started("review").unwrap().unwrap();
    let old: review_core::RoundStartedPayloadV1 = serde_json::from_value(old.payload).unwrap();
    let old_subject: review_core::SubjectV1 =
        serde_json::from_value(cas.get_json(&old.subject_id).unwrap()).unwrap();
    let old_snapshot = cas.get_json(&old_subject.head_snapshot_id).unwrap();
    let old_manifest: Manifest = serde_json::from_value(
        cas.get_json(old_snapshot["artifact_manifest"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    // The pinned authority files stay; the reviewed source is the operator's new tree.
    let mut entries: Vec<Entry> = old_manifest
        .entries
        .iter()
        .filter(|entry| entry.path.starts_with(".af/"))
        .cloned()
        .collect();
    entries.extend(head.iter().map(|(path, bytes)| Entry {
        path: path.clone(),
        kind: EntryKind::File,
        content: cas.put(bytes).unwrap(),
        size: bytes.len() as u64,
    }));
    let manifest = Manifest::new(entries).unwrap();
    let manifest_id = cas
        .put_json(&serde_json::to_value(&manifest).unwrap())
        .unwrap();
    let round = old.round + 1;
    let head_snapshot_id = cas
        .put_json(&serde_json::json!({
            "repository_id": old_snapshot["repository_id"], "vcs": "git",
            "capture": {"kind": "committed", "tree_id": format!("fixture-tree-{round}")},
            "source_revision": format!("fixture-{round}"),
            "content_digest": manifest.content_digest(), "artifact_manifest": manifest_id,
        }))
        .unwrap();
    let subject_id = cas
        .put_json(
            &serde_json::to_value(review_core::SubjectV1::whole_tree(&head_snapshot_id)).unwrap(),
        )
        .unwrap();
    // The CLI's flat Round assignment of the open Findings; the canonical set stays separate.
    let findings: review_core::FindingSetV1 =
        serde_json::from_value(cas.get_artifact(&finding_set_id).unwrap().payload).unwrap();
    let prior_rows: Vec<_> = findings
        .findings
        .iter()
        .map(|finding| {
            serde_json::json!({
                "key": finding.finding_id,
                "severity": format!("{:?}", finding.severity).to_lowercase(),
                "status": finding.status, "file": finding.file, "line": finding.line,
                "title": finding.title, "body": finding.body, "source": finding.source,
                "last_seen_round": finding.last_seen_round,
            })
        })
        .collect();
    let prior_finding_set_id = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id, "round": round, "prior_findings": prior_rows,
        }))
        .unwrap();
    let opened = store.campaign_opened("review").unwrap().unwrap();
    let mut refs = vec![
        opened.payload["authority_snapshot_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        old.campaign_manifest_id.clone(),
        head_snapshot_id,
        manifest_id,
        subject_id.clone(),
        prior_finding_set_id.clone(),
        prior_demand_set_id.clone(),
    ];
    refs.sort();
    refs.dedup();
    let next = store
        .append(
            "review",
            cas,
            review_store::NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(review_core::RoundStartedPayloadV1 {
                    round,
                    epoch: 1,
                    campaign_manifest_id: old.campaign_manifest_id,
                    subject_id,
                    prior_finding_set_id,
                    prior_demand_set_id,
                })
                .unwrap(),
            )
            .caused_by(opened.event_id)
            .referencing(refs),
        )
        .unwrap()
        .event_id;
    let projection = review_store::LedgerProjection::from_events(
        "review",
        &store.replay("review").unwrap(),
        cas,
    )
    .unwrap();
    let mut ingest = review_store::Ingest::from_projection(&mut store, cas, "review", projection)
        .unwrap()
        .under_round(&next);
    while ingest.ledger().round < round {
        ingest.advance().unwrap();
    }
    drop(ingest);
    let successor = LegacyReviewPlanCompiler::reopen(
        cas,
        CapturedLegacyReviewRound::load(cas, &store, "review", &next).unwrap(),
        &state.revision.provenance.adapter_id,
        compiler.policy_id(),
    )
    .unwrap();
    let revision = successor
        .prepare_continuation_revision(cas, &state.revision_id, &state.revision)
        .unwrap();
    let revision_id = plan::artifact(cas, review_core::task::TASK_REVISION_V1, &revision);
    let (plan, _) = successor.compile(cas, &revision_id).unwrap();
    let plan_id = plan::artifact(cas, review_core::task::EXECUTION_PLAN_V1, &plan);
    let handoff = TaskReviewHandoffV1 {
        task_id: lease.task_id().into(),
        predecessor_revision_id: state.revision_id.clone(),
        predecessor_plan_id: state.plan_id.clone().unwrap(),
        successor_revision_id: revision_id,
        successor_plan_id: plan_id,
        predecessor_round_id: state.revision.inputs["round"].artifact_ids[0].clone(),
        successor_round_id: revision.inputs["round"].artifact_ids[0].clone(),
        evidence: TaskReviewHandoffEvidenceV1::ClosedRound {
            report_event_id: conclusion.canonical_report_event_id,
        },
    };
    let handoff_id =
        review_store::store::task::review_handoff::capture_task_review_handoff(cas, &handoff)
            .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(&successor, host, &NoTaskDeveloper);
    store
        .continue_task_review(cas, lease, &handoff_id, &authority)
        .unwrap();
    store.admit_task_plan(cas, lease, &authority).unwrap();
    (successor, next)
}

/// Configure a host to keep its Warm Workspaces below `root`.
fn rooted<'s, 'c>(
    root: &std::path::Path,
) -> impl FnOnce(LegacyReviewTaskHost<'s, 'c>) -> LegacyReviewTaskHost<'s, 'c> + '_ {
    move |host| host.with_workspace_cache_root(root)
}

fn round_event(shared: &SharedEventStore<'_>) -> String {
    shared
        .lock()
        .unwrap()
        .latest_round_started("review")
        .unwrap()
        .unwrap()
        .event_id
}

/// The selected Attempt of `node` in the Round `round_event_id`.
fn selected_attempt(
    shared: &SharedEventStore<'_>,
    round_event_id: &str,
    node: &str,
) -> (String, TaskReviewResultSelectedV1) {
    let event = events_of(shared, EventType::TaskReviewResultSelectedV1)
        .into_iter()
        .find(|event| {
            event.node_id.as_deref() == Some(node)
                && event.causation_id.as_deref() == Some(round_event_id)
        })
        .unwrap_or_else(|| panic!("{node} has no selected Attempt"));
    (
        event.attempt_id.unwrap(),
        serde_json::from_value(event.payload).unwrap(),
    )
}

/// The complete sealed mutation set of a selected Attempt: its CAS ID and value.
fn sealed_mutations(
    cas: &Cas,
    selected: &TaskReviewResultSelectedV1,
) -> (String, serde_json::Value) {
    let provenance = cas.get_artifact(&selected.provenance_artifact_id).unwrap();
    let id = provenance.payload["mutations_artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mutations = cas.get_json(&id).unwrap();
    (id, mutations)
}

/// The context manifest entry names of a selected Attempt, with their recorded values.
fn manifest_entries(cas: &Cas, selected: &TaskReviewResultSelectedV1) -> Vec<serde_json::Value> {
    let context: TaskReviewContextV1 =
        serde_json::from_value(cas.get_artifact(&selected.context_id).unwrap().payload).unwrap();
    cas.get_json(&context.context_manifest_id).unwrap()["entries"]
        .as_array()
        .unwrap()
        .clone()
}

fn warm_set(cas: &Cas, event: &review_core::RunEvent) -> (WarmSetSelectedPayloadV1, WarmSetV1) {
    let selection: WarmSetSelectedPayloadV1 =
        serde_json::from_value(event.payload.clone()).unwrap();
    selection.validate().unwrap();
    let set: WarmSetV1 = serde_json::from_value(
        cas.get_artifact(&selection.warm_set_artifact_id)
            .unwrap()
            .payload,
    )
    .unwrap();
    set.validate().unwrap();
    (selection, set)
}

const WARM_REVIEW: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
warm = { notes = true, notes_max_bytes = 4096, session = "always" }
REVIEWER
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "reports", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "reviewer", port = "prior_findings" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "ledger", port = "reports" }
"#;

fn mark(delta: &HeadDeltaV1, path: &str) -> HeadDeltaMarkV1 {
    delta
        .marks
        .iter()
        .find(|entry| entry.path == path)
        .unwrap_or_else(|| panic!("{path} received no mark"))
        .mark
}

/// Round one's selected Attempt leaves Notes; its refused sibling answered with Notes too, and
/// they are never recorded. Round two on a new head receives the selected Notes, a Head Delta
/// against Round one's head and the open prior Findings through its wired port, from one Warm
/// Set recorded before its first Attempt is reserved, so a retry renders the same warm bytes.
/// The Task host runs no session protocol, so the session layer is dropped with that reason
/// and the Attempt runs on Notes.
#[test]
fn notes_head_delta_and_prior_findings_carry_from_the_selected_attempt_to_the_next_round() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let inputs = directory.path().join("inputs");
    std::fs::create_dir(&inputs).unwrap();
    let notes = serde_json::json!({
        "inspected": ["a.rs", "b.rs"],
        "model_of_change": "b.rs introduces the helper a.rs calls",
        "open_questions": ["is the helper reachable from tests?"],
        "hints": [{"path": "zzz.rs", "note": "mentioned in a comment only"}],
    });
    let mut first = requesting(vec![serde_json::json!({
        "severity": "major", "file": "a.rs", "line": 1, "title": "helper is unreachable",
        "body": "nothing calls it", "fix": "call it", "confidence": 0.9,
    })]);
    first["notes"] = notes.clone();
    // Round two answers the one assigned prior Finding, whose ID it reads from its input.
    let mut second: serde_json::Value = serde_json::from_str(super::domain::APPROVE).unwrap();
    second["notes"] = notes;
    second["dispositions"] = serde_json::json!([{
        "finding_id": "FINDING", "position": "corroborate", "reason": "the helper is still unreachable",
    }]);
    let second = second.to_string();
    let (second_head, second_tail) = second.split_once("FINDING").unwrap();
    // A Finding path outside FindingReport admission: refused before selection, Notes and all.
    let mut refused = requesting(vec![serde_json::json!({
        "severity": "major", "file": "./a.rs", "line": 1, "title": "refused path",
        "body": "body", "fix": "fix", "confidence": 0.9,
    })]);
    refused["notes"] = serde_json::json!({
        "inspected": ["a.rs"], "model_of_change": "notes of a refused answer",
        "open_questions": [], "hints": [],
    });
    // Every Attempt records the input it received; the first Attempt of each Round is refused.
    let script = format!(
        "n=$(ls '{dir}' | wc -l | tr -d ' '); n=$((n + 1)); cat > '{dir}/'$n.json; \
         finding=$(sed -n 's/.*\"finding_id\":\"\\(sha256:[0-9a-f]*\\)\".*/\\1/p' '{dir}/'$n.json); \
         case $n in 1|3) printf '%s' '{refused}' ;; 2) printf '%s' '{first}' ;; \
         *) printf '%s%s%s' '{second_head}' \"$finding\" '{second_tail}' ;; esac",
        dir = inputs.display(),
        refused = refused.to_string().replace('\'', "'\\''"),
        first = first.to_string().replace('\'', "'\\''"),
        second_head = second_head.replace('\'', "'\\''"),
        second_tail = second_tail.replace('\'', "'\\''"),
    );
    let definition = WARM_REVIEW.replace("REVIEWER", &runner(&script));
    let head_one = BTreeMap::from([
        ("a.rs".to_owned(), b"one\n".to_vec()),
        ("b.rs".to_owned(), b"two\n".to_vec()),
    ]);
    let (compiler, lease) =
        admit_source(&cas, &mut store, &definition, head_one, &["reviewer"], true);
    let shared = SharedEventStore::new(&mut store);
    let round_one = round_event(&shared);
    let head_two = BTreeMap::from([
        ("a.rs".to_owned(), b"one\n".to_vec()),
        ("c.rs".to_owned(), b"three\n".to_vec()),
    ]);
    let (successor, round_two) = hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |h| h,
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            continue_on_head(&cas, &shared, &compiler, host, &lease, &head_two)
        },
    );
    let round_two_attempts = hosted(
        &cas,
        &shared,
        &successor,
        &lease,
        |h| h,
        |host, runtime, report| {
            assert!(report.complete(), "{report:?}");
            host.publish_recorded_round_conclusion(&cas).unwrap();
            let state = runtime.projection().unwrap();
            let plan = state.plan_id.clone().unwrap();
            state
                .execution
                .unwrap()
                .attempt_accounting()
                .into_iter()
                .filter(|attempt| attempt.plan_id == plan)
                .map(|attempt| attempt.attempt_id)
                .collect::<std::collections::BTreeSet<_>>()
        },
    );
    assert_eq!(
        round_two_attempts.len(),
        2,
        "the refused Attempt and its retry"
    );
    let seen: Vec<serde_json::Value> = (1..=4)
        .map(|n| {
            serde_json::from_slice(&std::fs::read(inputs.join(format!("{n}.json"))).unwrap())
                .unwrap()
        })
        .collect();

    // Round one: every Attempt is asked for Notes and carries none; only the selected one
    // records them, bound to its head tree entries.
    for input in &seen[..2] {
        assert_eq!(
            input["notes_request"],
            serde_json::json!({"max_bytes": 4096})
        );
        assert!(input.get("notes").is_none() && input.get("head_delta").is_none());
        assert!(
            input.get("prior_findings").is_none(),
            "an empty set delivers none"
        );
    }
    let (selected_one, _) = selected_attempt(&shared, &round_one, "reviewer");
    let recorded: Vec<_> = events_of(&shared, EventType::WorkerNotesRecordedV1)
        .into_iter()
        .filter(|event| event.causation_id.as_deref() == Some(round_one.as_str()))
        .collect();
    assert_eq!(recorded.len(), 1, "only the selected Attempt records Notes");
    assert_eq!(
        recorded[0].attempt_id.as_deref(),
        Some(selected_one.as_str())
    );
    assert!(
        seen[1]["refused_attempts"]
            .as_array()
            .is_some_and(|refused| refused.len() == 1),
        "the Notes-carrying first answer was refused, not lost: {}",
        seen[1]
    );
    let recorded: WorkerNotesRecordedPayloadV1 =
        serde_json::from_value(recorded[0].payload.clone()).unwrap();
    let notes_id = recorded.notes_artifact_id.expect("notes were within bound");
    let envelope = cas.get_artifact(&notes_id).unwrap();
    assert_eq!(
        envelope.artifact_type,
        review_core::contract::WORKER_NOTES_V1
    );
    let stored: WorkerNotesV1 = serde_json::from_value(envelope.payload).unwrap();
    assert_eq!(stored.attempt_id, selected_one);
    assert_eq!(stored.inspected.len(), 2);
    assert!(
        stored.inspected[0].tree_entry_digest.is_some(),
        "inspected paths bind to head tree entries"
    );

    // Round two: the failed first Attempt and its retry received the same warm inputs.
    assert_eq!(seen[2]["notes"], seen[3]["notes"]);
    assert_eq!(seen[2]["head_delta"], seen[3]["head_delta"]);
    assert_eq!(seen[2]["prior_findings"], seen[3]["prior_findings"]);
    let carried = &seen[3]["notes"];
    assert_eq!(
        carried["model_of_change"],
        "b.rs introduces the helper a.rs calls"
    );
    assert_eq!(carried["attempt_id"], selected_one.as_str());
    assert_eq!(
        seen[3]["prior_findings"]["findings"][0]["title"], "helper is unreachable",
        "open prior Findings arrive through the wired port"
    );
    let selections: Vec<_> = events_of(&shared, EventType::WarmSetSelectedV1)
        .into_iter()
        .filter(|event| event.causation_id.as_deref() == Some(round_two.as_str()))
        .collect();
    assert_eq!(selections.len(), 1, "one Warm Set per node per Round");
    assert_eq!(selections[0].node_id.as_deref(), Some("reviewer"));
    let invocation = events_of(&shared, EventType::NodeInvocationV1)
        .into_iter()
        .find(|event| {
            event.node_id.as_deref() == Some("reviewer")
                && event.causation_id.as_deref() == Some(round_two.as_str())
        })
        .unwrap();
    assert!(
        invocation.sequence < selections[0].sequence,
        "the Warm Set is recorded when the invocation is published"
    );
    // The Campaign and Task streams share one append-only table, whose rowid is the global
    // append order: the Warm Set is durable before the Round's first Attempt is reserved.
    let task_run = review_store::store::task::task_run_id(lease.task_id()).unwrap();
    let first_reservation = shared
        .lock()
        .unwrap()
        .replay(&task_run)
        .unwrap()
        .into_iter()
        .find(|event| {
            let Ok(transition) = review_store::store::task::read_task_transition(event) else {
                return false;
            };
            let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
                transition.change
            else {
                return false;
            };
            matches!(
                review_store::store::task::execution::read_execution_record(&cas, &record_id)
                    .map(|recorded| recorded.record),
                Ok(review_core::task::execution::TaskExecutionRecordV1::Reserved { attempt_id, .. })
                    if round_two_attempts.contains(&attempt_id)
            )
        })
        .expect("Round two reserved its first Attempt");
    let appended = |event_id: &str| -> i64 {
        rusqlite::Connection::open(directory.path().join("events.sqlite"))
            .unwrap()
            .query_row(
                "SELECT rowid FROM events WHERE event_id = ?1",
                [event_id],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert!(
        appended(&selections[0].event_id) < appended(&first_reservation.event_id),
        "the Warm Set is recorded before the Round's first Attempt is reserved"
    );
    let (selection, set) = warm_set(&cas, &selections[0]);
    assert_eq!(
        selection.source_attempt_id.as_deref(),
        Some(selected_one.as_str())
    );
    assert_eq!(
        selection.layers,
        [WarmLayerV1::Notes, WarmLayerV1::HeadDelta]
    );
    assert_eq!(set.round, 2);
    assert_eq!(set.notes_artifact_id.as_deref(), Some(notes_id.as_str()));
    assert_eq!(set.session_artifact_id, None);
    assert_eq!(
        set.session_dropped,
        Some(review_core::SessionDropReasonV1::HostUnsupported),
        "the Task host records why the session layer was not carried"
    );
    let delta_id = set
        .head_delta_artifact_id
        .expect("a Head Delta was carried");
    let envelope = cas.get_artifact(&delta_id).unwrap();
    assert_eq!(envelope.artifact_type, review_core::contract::HEAD_DELTA_V1);
    let delta: HeadDeltaV1 = serde_json::from_value(envelope.payload).unwrap();
    delta.validate().unwrap();
    assert_eq!(serde_json::to_value(&delta).unwrap(), seen[3]["head_delta"]);
    assert_eq!(delta.node, "reviewer");
    assert_eq!(delta.from_snapshot_id, stored.head_snapshot_id);
    assert_eq!(delta.changed_paths, ["b.rs", "c.rs"]);
    assert_eq!(mark(&delta, "a.rs"), HeadDeltaMarkV1::Unchanged);
    assert_eq!(mark(&delta, "b.rs"), HeadDeltaMarkV1::Removed);
    assert_eq!(mark(&delta, "c.rs"), HeadDeltaMarkV1::New);
    assert_eq!(
        mark(&delta, "zzz.rs"),
        HeadDeltaMarkV1::Removed,
        "a path only the Notes mention still receives a mark"
    );
    let (_, selected_two) = selected_attempt(&shared, &round_two, "reviewer");
    let entries = manifest_entries(&cas, &selected_two);
    for name in ["warm_set", "warm_notes", "warm_head_delta"] {
        assert!(
            entries.iter().any(|entry| entry["name"] == name),
            "{name} is missing from {entries:?}"
        );
    }
}

/// When the Round cannot say what moved within the Head Delta bound, selection drops the
/// layer and records why: the Warm Set carries Notes alone and the Attempt runs on them, with
/// no Head Delta rendered or stored.
#[test]
fn an_over_bound_head_delta_is_dropped_at_selection_and_the_attempt_runs_on_notes() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let inputs = directory.path().join("inputs");
    std::fs::create_dir(&inputs).unwrap();
    let mut answer: serde_json::Value = serde_json::from_str(super::domain::APPROVE).unwrap();
    answer["notes"] = serde_json::json!({
        "inspected": ["src"], "model_of_change": "every file changes between the heads",
        "open_questions": [], "hints": [],
    });
    let script = format!(
        "n=$(ls '{dir}' | wc -l | tr -d ' '); cat > '{dir}/'$((n + 1)).json; printf '%s' '{}'",
        answer.to_string().replace('\'', "'\\''"),
        dir = inputs.display(),
    );
    let definition = WARM_REVIEW.replace("REVIEWER", &runner(&script));
    // Two heads that differ in every one of many long paths, so the canonical Head Delta of
    // the change exceeds its bound.
    let head = |content: &[u8]| -> BTreeMap<String, Vec<u8>> {
        (0..1_400)
            .map(|index| {
                (
                    format!("src/{}/f{index:04}.rs", "deep".repeat(40)),
                    content.to_vec(),
                )
            })
            .collect()
    };
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        head(b"one\n"),
        &["reviewer"],
        true,
    );
    let shared = SharedEventStore::new(&mut store);
    let round_one = round_event(&shared);
    let (successor, round_two) = hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |h| h,
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            continue_on_head(&cas, &shared, &compiler, host, &lease, &head(b"two\n"))
        },
    );
    hosted(
        &cas,
        &shared,
        &successor,
        &lease,
        |h| h,
        |_, _, report| assert!(report.complete(), "{report:?}"),
    );

    let (selected_one, _) = selected_attempt(&shared, &round_one, "reviewer");
    let selections: Vec<_> = events_of(&shared, EventType::WarmSetSelectedV1)
        .into_iter()
        .filter(|event| event.causation_id.as_deref() == Some(round_two.as_str()))
        .collect();
    assert_eq!(selections.len(), 1);
    let (selection, set) = warm_set(&cas, &selections[0]);
    assert_eq!(
        set.head_delta_dropped,
        Some(review_core::HeadDeltaDropReasonV1::OverBound)
    );
    assert_eq!(set.head_delta_artifact_id, None);
    assert!(set.notes_artifact_id.is_some());
    assert_eq!(selection.layers, [WarmLayerV1::Notes]);
    assert_eq!(
        selection.source_attempt_id.as_deref(),
        Some(selected_one.as_str())
    );
    let input: serde_json::Value =
        serde_json::from_slice(&std::fs::read(inputs.join("2.json")).unwrap()).unwrap();
    assert_eq!(
        input["notes"]["model_of_change"],
        "every file changes between the heads"
    );
    assert!(
        input.get("head_delta").is_none(),
        "the Attempt runs on Notes alone"
    );
    let (_, selected_two) = selected_attempt(&shared, &round_two, "reviewer");
    let entries = manifest_entries(&cas, &selected_two);
    assert!(entries.iter().any(|entry| entry["name"] == "warm_notes"));
    assert!(
        !entries
            .iter()
            .any(|entry| entry["name"] == "warm_head_delta"),
        "{entries:?}"
    );
}

const COLD_REVIEW: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
REVIEWER
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{ name = "reports", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "reviewer", port = "prior_findings" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "ledger", port = "reports" }
"#;

/// A node without warm policy is asked for nothing, records no Warm Set or Notes in any
/// Round, and never creates a Warm Workspace root.
#[test]
fn a_cold_node_records_nothing_and_touches_no_workspace_root() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let inputs = directory.path().join("inputs");
    std::fs::create_dir(&inputs).unwrap();
    let workspaces = directory.path().join("workspaces");
    let script = format!(
        "n=$(ls '{dir}' | wc -l | tr -d ' '); cat > '{dir}/'$((n + 1)).json; printf '%s' '{}'",
        super::domain::APPROVE,
        dir = inputs.display(),
    );
    let definition = COLD_REVIEW.replace("REVIEWER", &runner(&script));
    let head = BTreeMap::from([("a.rs".to_owned(), b"one\n".to_vec())]);
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        head.clone(),
        &["reviewer"],
        true,
    );
    let shared = SharedEventStore::new(&mut store);
    let (successor, _) = hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        rooted(&workspaces),
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            continue_on_head(&cas, &shared, &compiler, host, &lease, &head)
        },
    );
    hosted(
        &cas,
        &shared,
        &successor,
        &lease,
        rooted(&workspaces),
        |_, _, report| {
            assert!(report.complete(), "{report:?}");
        },
    );
    for n in 1..=2 {
        let input: serde_json::Value =
            serde_json::from_slice(&std::fs::read(inputs.join(format!("{n}.json"))).unwrap())
                .unwrap();
        for field in ["notes_request", "notes", "head_delta"] {
            assert!(input.get(field).is_none(), "Attempt {n} received {field}");
        }
    }
    for event_type in [
        EventType::WarmSetSelectedV1,
        EventType::WorkerNotesRecordedV1,
        EventType::WorkspaceRebasedV1,
    ] {
        assert!(events_of(&shared, event_type).is_empty(), "{event_type}");
    }
    assert!(
        !workspaces.exists(),
        "a cold node never creates a stable root"
    );
}

const WORKSPACE_REVIEW: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
warm = { notes = false, workspace = "rebase" }
REVIEWER
[[nodes]]
id = "reader"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
READER
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "reviewer", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }, { name = "reader", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "reviewer", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "reader", port = "prior_findings" }
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

fn head_one() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("a.rs".to_owned(), b"one\n".to_vec()),
        ("b.rs".to_owned(), b"two\n".to_vec()),
        ("src/deep/c.rs".to_owned(), b"three\n".to_vec()),
    ])
}

fn head_two() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("a.rs".to_owned(), b"uno\n".to_vec()),
        ("d.rs".to_owned(), b"four\n".to_vec()),
        ("src/deep/c.rs".to_owned(), b"three\n".to_vec()),
    ])
}

/// The warm reviewer checks the head it was given: head one in Round one; from Round two on
/// it checks head two and edits one file. The first Attempt of Round two fails when `retry`.
fn workspace_review(state: &std::path::Path, retry: bool) -> String {
    let approve: serde_json::Value = serde_json::from_str(super::domain::APPROVE).unwrap();
    let round_one = answering(
        "test \"$(cat a.rs)\" = one && test \"$(cat src/deep/c.rs)\" = three",
        &approve,
    );
    let round_two = answering(
        "test \"$(cat a.rs)\" = uno && test ! -e b.rs && test \"$(cat d.rs)\" = four \
         && test \"$(cat src/deep/c.rs)\" = three && printf edited > edited.txt",
        &approve,
    );
    let calls = state.join("calls");
    let script = format!(
        "printf x >> '{calls}'; if test \"$(cat a.rs)\" = one; then {round_one}; \
         elif {fail} test \"$(wc -c < '{calls}' | tr -d ' ')\" = 2; then exit 1; else {round_two}; fi",
        calls = calls.display(),
        fail = if retry { "true &&" } else { "false &&" },
    );
    // The reader keeps one open major Finding, so no Round converges before the last.
    let mut open = super::domain::finding("major", "Still open", Some("open"));
    open["file"] = serde_json::json!("a.rs");
    let open = requesting(vec![open]);
    WORKSPACE_REVIEW
        .replace("REVIEWER", &runner(&script))
        .replace("READER", &runner(&answering("test -f a.rs", &open)))
}

fn rebased(shared: &SharedEventStore<'_>) -> Vec<WorkspaceRebasedPayloadV1> {
    events_of(shared, EventType::WorkspaceRebasedV1)
        .into_iter()
        .map(|event| {
            assert_eq!(event.node_id.as_deref(), Some("reviewer"));
            let payload: WorkspaceRebasedPayloadV1 = serde_json::from_value(event.payload).unwrap();
            payload.validate().unwrap();
            payload
        })
        .collect()
}

/// Every regular file below `root`, keyed by relative path.
fn tree_bytes(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &std::path::Path, at: &std::path::Path, out: &mut BTreeMap<String, Vec<u8>>) {
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

/// How Round two finds the warm node's stable root.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RootState {
    /// As Round one recorded it.
    Recorded,
    /// An entry the next head does not touch changed under an intact marker.
    Tampered,
    /// A preparation swapped the tree and wrote its marker, then died before the log
    /// recorded it.
    Unrecorded,
}

/// Round one materializes the warm node's stable root in full. Round two re-bases it to the
/// new head by tree diff and verifies it, unless the root cannot be trusted: then it is
/// rebuilt from the CAS with the recorded reason, the previous head taken from the log. A
/// retried Attempt reuses the recorded Warm Set, and a sandbox edit is sealed but never
/// reaches the template.
fn run_workspace(root_state: RootState) {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let workspaces = directory.path().join("workspaces");
    let retry = root_state == RootState::Recorded;
    let definition = workspace_review(directory.path(), retry);
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        head_one(),
        &["reviewer", "reader"],
        true,
    );
    let shared = SharedEventStore::new(&mut store);
    let (successor, round_two) = hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        rooted(&workspaces),
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            continue_on_head(&cas, &shared, &compiler, host, &lease, &head_two())
        },
    );
    let prepared = rebased(&shared);
    assert_eq!(prepared.len(), 1, "one preparation per warm node per Round");
    let first = &prepared[0];
    assert_eq!(first.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        first.fallback,
        Some(WorkspaceFallbackReasonV1::NoVerifiedTemplate),
        "the first Round has no template to re-base and says so"
    );
    assert_eq!(first.from_snapshot_id, None);
    assert_eq!(
        first.entries_touched, 5,
        "three sources and the pinned authority files"
    );
    assert!(review_core::is_workspace_id(&first.workspace_id));
    let root = workspaces.join(&first.workspace_id).join("tree");
    let selections = events_of(&shared, EventType::WarmSetSelectedV1);
    assert_eq!(selections.len(), 1, "the cold reader selects nothing");
    let (selection, set) = warm_set(&cas, &selections[0]);
    assert!(
        selection.layers.is_empty(),
        "a full materialization carries nothing"
    );
    assert_eq!(set.workspace, Some(WorkspaceBasisV1::Full));
    assert_eq!(
        set.workspace_id.as_deref(),
        Some(first.workspace_id.as_str())
    );
    let round_one_head: review_core::SourceSnapshot =
        serde_json::from_value(cas.get_json(&first.to_snapshot_id).unwrap()).unwrap();
    let round_one_manifest: Manifest = serde_json::from_value(
        cas.get_json(round_one_head.artifact_manifest.as_deref().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        review_source_git::scan_tree(&root).unwrap(),
        round_one_manifest
    );

    let expected_head: review_core::RoundStartedPayloadV1 = serde_json::from_value(
        events_of(&shared, EventType::RoundStartedV1)
            .into_iter()
            .find(|event| event.event_id == round_two)
            .unwrap()
            .payload,
    )
    .unwrap();
    let head_two_id = review_store::resolve_subject(&cas, &expected_head.subject_id)
        .unwrap()
        .subject
        .head_snapshot_id;
    let head_two_snapshot: review_core::SourceSnapshot =
        serde_json::from_value(cas.get_json(&head_two_id).unwrap()).unwrap();
    let head_two_manifest: Manifest = serde_json::from_value(
        cas.get_json(head_two_snapshot.artifact_manifest.as_deref().unwrap())
            .unwrap(),
    )
    .unwrap();
    match root_state {
        RootState::Recorded => {}
        RootState::Tampered => {
            std::fs::write(root.join("src/deep/c.rs"), b"tampered\n").unwrap();
        }
        RootState::Unrecorded => {
            let workspace =
                review_sandbox::WorkspaceRoot::new(&workspaces, &first.workspace_id).unwrap();
            let recorded = review_sandbox::RecordedPreparation {
                snapshot_id: first.to_snapshot_id.clone(),
                verified_digest: first.verified_digest.clone(),
            };
            let interrupted = review_sandbox::prepare_workspace(
                &workspace,
                &head_two_manifest,
                &head_two_id,
                &cas,
                Some(&recorded),
            )
            .unwrap();
            assert_eq!(interrupted.basis, WorkspaceBasisV1::Rebased);
        }
    }
    let third = hosted(
        &cas,
        &shared,
        &successor,
        &lease,
        rooted(&workspaces),
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            if root_state == RootState::Recorded {
                Some(continue_on_head(
                    &cas,
                    &shared,
                    &successor,
                    host,
                    &lease,
                    &head_two(),
                ))
            } else {
                host.publish_recorded_round_conclusion(&cas).unwrap();
                None
            }
        },
    );

    let prepared = rebased(&shared);
    assert_eq!(prepared.len(), 2, "one preparation per warm node per Round");
    let second = &prepared[1];
    assert_eq!(
        second.from_snapshot_id.as_deref(),
        Some(first.to_snapshot_id.as_str()),
        "the previous head is the log's, never the marker's"
    );
    assert_eq!(second.to_snapshot_id, head_two_id);
    assert_eq!(second.verified_digest, head_two_manifest.content_digest());
    assert_eq!(
        second.workspace_id, first.workspace_id,
        "one stable root per node per Campaign"
    );
    let (basis, fallback) = match root_state {
        RootState::Recorded => (WorkspaceBasisV1::Rebased, None),
        RootState::Tampered => (
            WorkspaceBasisV1::Full,
            Some(WorkspaceFallbackReasonV1::TemplateCorrupt),
        ),
        RootState::Unrecorded => (
            WorkspaceBasisV1::Full,
            Some(WorkspaceFallbackReasonV1::UnrecordedPreparation),
        ),
    };
    assert_eq!((second.basis, second.fallback), (basis, fallback));
    assert_eq!(
        second.entries_touched,
        if basis == WorkspaceBasisV1::Rebased {
            3
        } else {
            5
        },
        "a re-base touches what moved (a.rs, b.rs, d.rs); a rebuild writes every entry"
    );
    let fresh = directory.path().join("fresh");
    review_source_git::materialize(&head_two_manifest, &cas, &fresh).unwrap();
    assert_eq!(
        tree_bytes(&root),
        tree_bytes(&fresh),
        "the prepared template is byte-identical to a full materialization"
    );
    let selections: Vec<_> = events_of(&shared, EventType::WarmSetSelectedV1)
        .into_iter()
        .filter(|event| event.causation_id.as_deref() == Some(round_two.as_str()))
        .collect();
    assert_eq!(
        selections.len(),
        1,
        "one Warm Set per node per Round, reused on retry"
    );
    let (selection, set) = warm_set(&cas, &selections[0]);
    let layers = if basis == WorkspaceBasisV1::Rebased {
        vec![WarmLayerV1::Workspace]
    } else {
        vec![]
    };
    assert_eq!(selection.layers, layers);
    assert_eq!(set.round, 2);
    assert_eq!(set.workspace, Some(basis));

    let (_, selected) = selected_attempt(&shared, &round_two, "reviewer");
    assert_eq!(
        sealed_mutations(&cas, &selected).1,
        serde_json::json!({"added": ["edited.txt"], "modified": [], "deleted": []})
    );
    assert!(
        manifest_entries(&cas, &selected)
            .iter()
            .any(|entry| entry["name"] == "warm_set"),
        "the manifest names the Warm Set that records the workspace basis"
    );
    assert!(
        !root.join("edited.txt").exists(),
        "a sandbox write never reaches the template"
    );
    assert!(
        events_of(&shared, EventType::WarmSetSelectedV1)
            .iter()
            .all(|event| event.node_id.as_deref() == Some("reviewer")),
        "a node without the policy keeps a temporary template and records nothing"
    );

    // A Round on an unchanged tree reuses the verified template and writes nothing.
    let Some((third, round_three)) = third else {
        return;
    };
    hosted(
        &cas,
        &shared,
        &third,
        &lease,
        rooted(&workspaces),
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            host.publish_recorded_round_conclusion(&cas).unwrap();
        },
    );
    let prepared = rebased(&shared);
    assert_eq!(prepared.len(), 3);
    let reused = &prepared[2];
    assert_eq!(
        (reused.basis, reused.fallback),
        (WorkspaceBasisV1::Reused, None)
    );
    assert_eq!(
        reused.entries_touched, 0,
        "a Round on an unchanged head materializes nothing"
    );
    assert_eq!(
        reused.from_snapshot_id.as_deref(),
        Some(head_two_id.as_str())
    );
    assert_eq!(tree_bytes(&root), tree_bytes(&fresh));
    let selections: Vec<_> = events_of(&shared, EventType::WarmSetSelectedV1)
        .into_iter()
        .filter(|event| event.causation_id.as_deref() == Some(round_three.as_str()))
        .collect();
    assert_eq!(selections.len(), 1);
    assert_eq!(
        warm_set(&cas, &selections[0]).1.workspace,
        Some(WorkspaceBasisV1::Reused)
    );
}

#[test]
fn a_warm_workspace_is_materialized_once_then_rebased_per_head() {
    run_workspace(RootState::Recorded);
}

#[test]
fn a_corrupted_template_fails_closed_into_a_full_materialization_that_records_why() {
    run_workspace(RootState::Tampered);
}

#[test]
fn a_preparation_the_log_never_recorded_is_rebuilt_with_the_recorded_lineage() {
    run_workspace(RootState::Unrecorded);
}

/// A root the preparation cannot trust fails the warm node closed, and neither its outcome nor
/// the Campaign log names the host path.
#[test]
fn a_workspace_preparation_failure_names_no_host_path() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let workspaces = directory.path().join("distinctive-cache-root-7f3a");
    let definition = workspace_review(directory.path(), false);
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        head_one(),
        &["reviewer", "reader"],
        true,
    );
    let opened = store.campaign_opened("review").unwrap().unwrap();
    let id = review_sandbox::workspace_id(
        "review",
        opened.payload["campaign_manifest_id"].as_str().unwrap(),
        "reviewer",
    );
    // The node's root exists but is a symlink: the preparation refuses it.
    std::fs::create_dir_all(&workspaces).unwrap();
    std::fs::create_dir_all(directory.path().join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(directory.path().join("elsewhere"), workspaces.join(&id)).unwrap();
    let shared = SharedEventStore::new(&mut store);
    let outcomes = hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |host| host.with_workspace_cache_root(&workspaces),
        |_, _, report| {
            assert!(!report.complete());
            format!("{:?}", report.outcomes)
        },
    );
    assert!(
        outcomes.contains("warm workspace root is unavailable"),
        "{outcomes}"
    );
    assert!(
        !outcomes.contains("distinctive-cache-root") && !outcomes.contains(&id),
        "no host path in a durable outcome: {outcomes}"
    );
    for event in shared.lock().unwrap().replay("review").unwrap() {
        let bytes = serde_json::to_string(&event).unwrap();
        assert!(
            !bytes.contains("distinctive-cache-root"),
            "no host path in the log: {bytes}"
        );
    }
}

const BUILD_CACHE_REVIEW: &str = r#"
version = 3
[subject]
kind = "whole-tree"
[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"
build_caches = ["cargo_target"]
[[checks]]
name = "build"
program = "/bin/sh"
BUILD
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "tdd"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
warm = { notes = false, build_cache = ["cargo_target"] }
TDD
[[nodes]]
id = "reader"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
READER
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "tdd", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }, { name = "reader", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "tdd", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "reader", port = "prior_findings" }
[[edges]]
from = { node = "tdd", port = "result" }
to = { node = "gather", port = "tdd" }
[[edges]]
from = { node = "reader", port = "result" }
to = { node = "gather", port = "reader" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

/// The build-cache pipeline with the given Gate build and reviewers; `warm = false` drops the
/// Gate's capture and the reviewer's carry, leaving the same reviewers cold.
fn build_cache_review(build: &str, tdd: &str, warm: bool) -> String {
    let approve: serde_json::Value = serde_json::from_str(super::domain::APPROVE).unwrap();
    let definition = BUILD_CACHE_REVIEW
        .replace(
            "BUILD",
            &format!(
                "args = [{{ value = \"-c\" }}, {{ value = {} }}]",
                serde_json::to_string(build).unwrap()
            ),
        )
        .replace("TDD", &runner(&answering(tdd, &approve)))
        .replace(
            "READER",
            &runner(&answering(
                "test -z \"${CARGO_TARGET_DIR:-}\" && test ! -e .af-cache && cat src/main.rs >/dev/null",
                &approve,
            )),
        );
    if warm {
        definition
    } else {
        definition
            .replace("build_caches = [\"cargo_target\"]\n", "")
            .replace(
                "warm = { notes = false, build_cache = [\"cargo_target\"] }\n",
                "",
            )
    }
}

fn run_build_cache(directory: &std::path::Path, definition: &str) -> (Cas, EventStore, String) {
    let cas = Cas::open(directory.join("cas")).unwrap();
    let mut store = EventStore::open(directory.join("events.sqlite")).unwrap();
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        definition,
        BTreeMap::from([("src/main.rs".to_owned(), b"fn main() {}\n".to_vec())]),
        &["tdd", "reader"],
        false,
    );
    {
        let shared = SharedEventStore::new(&mut store);
        hosted(
            &cas,
            &shared,
            &compiler,
            &lease,
            |h| h,
            |host, _, report| {
                assert!(report.complete(), "{report:?}");
                host.publish_recorded_round_conclusion(&cas).unwrap();
            },
        );
    }
    let round = store
        .latest_round_started("review")
        .unwrap()
        .unwrap()
        .event_id;
    (cas, store, round)
}

/// A TDD reviewer reuses the Gate's build through `CARGO_TARGET_DIR` instead of rebuilding.
/// The capture is an explicitly unsafe, candidate-built `BuildCache@1`, only the node that
/// declares the kind receives it, and the cloned bytes leave before seal: the sealed diff is
/// byte-identical to a cold run of the same reviewer.
#[test]
fn a_tdd_reviewer_reuses_the_gate_build_and_seals_the_same_diff_as_a_cold_run() {
    let build = "test -n \"$CARGO_TARGET_DIR\" && mkdir -p \"$CARGO_TARGET_DIR/debug/deps\" \
         && printf compiled > \"$CARGO_TARGET_DIR/debug/deps/libfixture.rlib\" \
         && printf '#!/bin/sh\\nexit 0\\n' > \"$CARGO_TARGET_DIR/debug/fixture-test\" \
         && chmod 755 \"$CARGO_TARGET_DIR/debug/fixture-test\"";
    let warm_tdd = "test \"$(cat \"$CARGO_TARGET_DIR/debug/deps/libfixture.rlib\")\" = compiled \
         && \"$CARGO_TARGET_DIR/debug/fixture-test\" \
         && case \"$CARGO_TARGET_DIR\" in */.af-cache/cargo_target) ;; *) exit 7 ;; esac \
         && test -f .af-cache/cargo_target/debug/deps/libfixture.rlib \
         && printf edited > edited.txt";
    let warm_dir = tempfile::tempdir().unwrap();
    let (cas, mut store, round) =
        run_build_cache(warm_dir.path(), &build_cache_review(build, warm_tdd, true));
    let shared = SharedEventStore::new(&mut store);
    let captured = events_of(&shared, EventType::BuildCacheCapturedV1);
    assert_eq!(captured.len(), 1, "one capture per Gate per Round");
    assert_eq!(captured[0].node_id.as_deref(), Some("gate"));
    let capture: BuildCacheCapturedPayloadV1 =
        serde_json::from_value(captured[0].payload.clone()).unwrap();
    capture.validate().unwrap();
    assert_eq!(capture.gate_node, "gate");
    assert_eq!(capture.kind, review_core::BuildCacheKindV1::CargoTarget);
    assert_eq!(
        capture.limits,
        review_core::BuildCacheLimitsV1::default_v1()
    );
    assert_eq!(capture.refused, None);
    assert_eq!((capture.entries, capture.bytes), (2, 8 + 17));
    let build_cache_id = capture.build_cache_artifact_id.clone().unwrap();
    assert!(captured[0].artifact_refs.contains(&build_cache_id));
    let envelope = cas.get_artifact(&build_cache_id).unwrap();
    assert_eq!(
        envelope.artifact_type,
        review_core::contract::BUILD_CACHE_V1
    );
    assert_eq!(
        envelope.subject_snapshot_id.as_deref(),
        Some(capture.head_snapshot_id.as_str())
    );
    let cache: review_core::BuildCacheV1 = serde_json::from_value(envelope.payload).unwrap();
    cache.validate().unwrap();
    assert_eq!(cache.trust, review_core::BuildCacheTrustV1::CandidateBuilt);
    assert!(captured[0].artifact_refs.contains(&cache.manifest_id));
    let cache_manifest: Manifest =
        serde_json::from_value(cas.get_json(&cache.manifest_id).unwrap()).unwrap();
    assert_eq!(cache_manifest.content_digest(), cache.content_digest);
    assert_eq!(
        cache_manifest.get("debug/fixture-test").unwrap().kind,
        EntryKind::Executable
    );

    let selections = events_of(&shared, EventType::WarmSetSelectedV1);
    assert_eq!(
        selections.len(),
        1,
        "only the node that declares the kind selects"
    );
    assert_eq!(selections[0].node_id.as_deref(), Some("tdd"));
    let (selection, set) = warm_set(&cas, &selections[0]);
    assert_eq!(selection.layers, [WarmLayerV1::BuildCache]);
    assert_eq!(
        selection.source_attempt_id, None,
        "Round one has no previous Attempt"
    );
    assert_eq!(
        set.build_cache_artifact_id.as_deref(),
        Some(build_cache_id.as_str())
    );

    let (_, tdd) = selected_attempt(&shared, &round, "tdd");
    let (warm_artifact, warm_mutations) = sealed_mutations(&cas, &tdd);
    assert_eq!(
        warm_mutations,
        serde_json::json!({"added": ["edited.txt"], "modified": [], "deleted": []}),
        "the cloned build cache never enters the sealed diff"
    );
    let entries = manifest_entries(&cas, &tdd);
    assert!(
        entries.iter().any(|entry| entry["name"] == "warm_set"),
        "{entries:?}"
    );
    let carried = entries
        .iter()
        .find(|entry| entry["name"] == "warm_build_cache")
        .expect("the manifest names the carried build cache");
    assert_eq!(carried["artifact_id"], build_cache_id.as_str());
    assert_eq!(carried["rendered_bytes"], 0);
    let (_, reader) = selected_attempt(&shared, &round, "reader");
    assert_eq!(
        sealed_mutations(&cas, &reader).1,
        serde_json::json!({"added": [], "modified": [], "deleted": []})
    );
    assert!(
        manifest_entries(&cas, &reader)
            .iter()
            .all(|entry| entry["name"] != "warm_build_cache"),
        "a node that declares no kind receives nothing"
    );

    let cold_dir = tempfile::tempdir().unwrap();
    let (cold_cas, mut cold_store, cold_round) = run_build_cache(
        cold_dir.path(),
        &build_cache_review(
            "exit 0",
            "test -z \"${CARGO_TARGET_DIR:-}\" && test ! -e .af-cache && printf edited > edited.txt",
            false,
        ),
    );
    let cold = SharedEventStore::new(&mut cold_store);
    assert!(events_of(&cold, EventType::BuildCacheCapturedV1).is_empty());
    assert!(events_of(&cold, EventType::WarmSetSelectedV1).is_empty());
    let (_, cold_tdd) = selected_attempt(&cold, &cold_round, "tdd");
    let (cold_artifact, cold_mutations) = sealed_mutations(&cold_cas, &cold_tdd);
    assert_eq!(cold_mutations, warm_mutations);
    assert_eq!(
        cold_artifact, warm_artifact,
        "the sealed diff of the warm Attempt is byte-identical to the cold run's"
    );
}

/// A link in the Gate's cache directory refuses the capture with a recorded reason. The Gate
/// verdict does not change, and the warm node runs cold with the drop recorded.
#[test]
fn a_symlink_in_the_gate_cache_directory_refuses_capture_with_a_recorded_reason() {
    let directory = tempfile::tempdir().unwrap();
    let (cas, mut store, round) = run_build_cache(
        directory.path(),
        &build_cache_review(
            "mkdir -p \"$CARGO_TARGET_DIR/debug\" && printf built > \"$CARGO_TARGET_DIR/debug/artifact\" \
             && ln -s /etc/hosts \"$CARGO_TARGET_DIR/debug/linked\"",
            "test -z \"${CARGO_TARGET_DIR:-}\" && test ! -e .af-cache && printf edited > edited.txt",
            true,
        ),
    );
    let shared = SharedEventStore::new(&mut store);
    let decision: review_check::GateDecision = serde_json::from_value(
        cas.get_json(
            events_of(&shared, EventType::NodeOutputReceiptV1)
                .into_iter()
                .find(|event| event.node_id.as_deref() == Some("gate"))
                .unwrap()
                .payload["outputs"][0]["artifact_ids"][0]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        decision.passed(),
        "a refused capture never changes the Gate verdict"
    );
    let captured = events_of(&shared, EventType::BuildCacheCapturedV1);
    assert_eq!(captured.len(), 1);
    assert!(captured[0].artifact_refs.iter().all(|artifact| {
        cas.get_optional_artifact(artifact)
            .ok()
            .flatten()
            .is_none_or(|envelope| envelope.artifact_type != review_core::contract::BUILD_CACHE_V1)
    }));
    let capture: BuildCacheCapturedPayloadV1 =
        serde_json::from_value(captured[0].payload.clone()).unwrap();
    capture.validate().unwrap();
    assert_eq!(capture.build_cache_artifact_id, None);
    assert_eq!(
        capture.refused,
        Some(review_core::BuildCacheRefusalReasonV1::UnsafeContent)
    );
    assert_eq!((capture.entries, capture.bytes), (0, 0));
    let selections = events_of(&shared, EventType::WarmSetSelectedV1);
    assert_eq!(selections.len(), 1);
    let (selection, set) = warm_set(&cas, &selections[0]);
    assert!(selection.layers.is_empty());
    assert_eq!(set.build_cache_artifact_id, None);
    assert_eq!(
        set.build_cache_dropped,
        Some(review_core::BuildCacheDropReasonV1::Refused)
    );
    let (_, tdd) = selected_attempt(&shared, &round, "tdd");
    assert_eq!(
        sealed_mutations(&cas, &tdd).1,
        serde_json::json!({"added": ["edited.txt"], "modified": [], "deleted": []})
    );
}
