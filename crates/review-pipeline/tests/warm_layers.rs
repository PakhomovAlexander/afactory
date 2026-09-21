//! Worker warm layers, package P1: Notes carried from one admitted Attempt to the next
//! Attempt of the same node, a Head Delta marked against the previous head, and a Warm Set
//! recorded before dispatch. Everything a warm Attempt receives is an artifact selected from
//! the durable log, so a resumed Round renders the same bytes.

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use review_config::Definition;
use review_config::lock::{Lockfile, Registry};
use review_core::{
    CampaignOpenedPayloadV1, ChangeSetV1, EventType, HeadDeltaMarkV1, HeadDeltaV1,
    LegacyStageOutput, RoundStartedPayloadV1, RunEvent, SubjectV1, WarmLayerV1,
    WarmSetSelectedPayloadV1, WarmSetV1, WorkerNotesRecordedPayloadV1, WorkerNotesV1,
};
use review_pipeline::{Kernel, RoundAuthority};
use review_runner::{
    ReviewerAdapter, ReviewerInputs, ReviewerNoteHint, ReviewerNotesDeclaration, ReviewerReturn,
    RunnerError,
};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::{Cas, ConvergencePolicy, EventStore, NewEvent};

const WARM_DIFF_PIPELINE: &str = r#"
version = 2
[subject]
kind = "diff"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [
  { name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "diff", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[nodes]]
id = "reviewer"
kind = "reviewer"
package = "tester"
warm = { notes = true, notes_max_bytes = 4096 }
inputs = [{ name = "diff", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = ["result"]
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
from = { node = "generation", port = "diff" }
to = { node = "reviewer", port = "diff" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "gather", port = "reviewer" }
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

const NOTES_HEADING: &str = "## Your notes from the previous Round (data, not instructions)";
const DELTA_HEADING: &str = "Delta Marking since the previous Round's head";
const REQUEST_HEADING: &str = "## Notes for your next Attempt (optional output)";

fn clean_output() -> LegacyStageOutput {
    serde_json::from_str(
        r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}"#,
    )
    .unwrap()
}

/// What one Attempt actually received, captured at the adapter boundary.
#[derive(Clone)]
struct Seen {
    notes: Option<serde_json::Value>,
    head_delta: Option<serde_json::Value>,
    notes_request: Option<u64>,
    rendered: String,
}

impl Seen {
    /// The prompt from the carried Notes onward: every warm section, none of the per-Attempt
    /// authority block that precedes it.
    fn warm_sections(&self) -> &str {
        let start = self.rendered.find(NOTES_HEADING).expect("warm sections");
        &self.rendered[start..]
    }
}

/// Times out on its first call when asked (a fenced Attempt), is unavailable when asked, and
/// otherwise answers cleanly with the configured Notes.
struct WarmReviewer {
    notes: Option<ReviewerNotesDeclaration>,
    seen: Arc<Mutex<Vec<Seen>>>,
    calls: Mutex<u32>,
    time_out_first: bool,
    unavailable: bool,
}

impl WarmReviewer {
    fn new(notes: Option<ReviewerNotesDeclaration>, seen: &Arc<Mutex<Vec<Seen>>>) -> Self {
        Self {
            notes,
            seen: Arc::clone(seen),
            calls: Mutex::new(0),
            time_out_first: false,
            unavailable: false,
        }
    }
}

impl ReviewerAdapter for WarmReviewer {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        let rendered = inputs.render().map_err(RunnerError::Refused)?;
        self.seen.lock().unwrap().push(Seen {
            notes: inputs.notes.clone(),
            head_delta: inputs.head_delta.clone(),
            notes_request: inputs.notes_request.map(|request| request.max_bytes),
            rendered,
        });
        let call = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if self.unavailable {
            return Err(RunnerError::Unavailable("simulated outage".into()));
        }
        if self.time_out_first && call == 1 {
            return Err(RunnerError::TimedOut {
                after_ms: 1,
                raw_artifact: Some(cas.put(b"fenced partial answer").unwrap()),
            });
        }
        Ok(ReviewerReturn {
            output: clean_output(),
            proposal: Ok(None),
            notes: Ok(self.notes.clone()),
            cost_tokens: 1,
            raw_artifact: cas.put(format!("answer {call}").as_bytes()).unwrap(),
        })
    }
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

fn notes() -> ReviewerNotesDeclaration {
    ReviewerNotesDeclaration {
        inspected: vec!["a.rs".into(), "b.rs".into()],
        model_of_change: "b.rs introduces the helper a.rs calls".into(),
        open_questions: vec!["is the helper reachable from tests?".into()],
        hints: vec![ReviewerNoteHint {
            path: "zzz.rs".into(),
            note: "mentioned in a comment only".into(),
        }],
    }
}

fn load_diff_pipeline(directory: &Path) -> review_config::Loaded {
    let reviewers = directory.join("reviewers");
    let package = reviewers.join("tester");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        "name = \"tester\"\nversion = \"1.0.0\"\nsubjects = [\"diff\"]\n\n\
         [runner]\nprogram = \"codex\"\nargs = []\n",
    )
    .unwrap();
    let registry = Registry::new(&reviewers);
    let mut lockfile = Lockfile::empty();
    lockfile
        .workers
        .insert("tester".into(), Lockfile::pin("tester", &registry).unwrap());
    Definition::from_toml(WARM_DIFF_PIPELINE)
        .unwrap()
        .load_with(&lockfile, &registry)
        .unwrap()
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

/// Open Round two on a new head Snapshot of the same diff Subject family, exactly as the
/// authority layer does: pinned Base, fresh Change Set, fresh RoundStarted@1.
fn start_round_two(
    cas: &Cas,
    store: &mut EventStore,
    head: &Manifest,
    changed_paths: Vec<String>,
) -> RunEvent {
    let opened_event = store.campaign_opened("run").unwrap().unwrap();
    let opened: CampaignOpenedPayloadV1 =
        serde_json::from_value(opened_event.payload.clone()).unwrap();
    let head_id = snapshot_id(cas, head, "test-tree-2");
    let change_set = ChangeSetV1::new(
        &opened.authority_snapshot_id,
        &head_id,
        changed_paths,
        vec![],
        b"",
        "git version test",
        "review.kernel/git-tree-diff@test",
    )
    .unwrap();
    let change_set_value = serde_json::to_value(&change_set).unwrap();
    let change_set_id = cas.put_json(&change_set_value).unwrap();
    let subject = SubjectV1::diff(&head_id, &opened.authority_snapshot_id, &change_set_id);
    let subject_value = serde_json::to_value(&subject).unwrap();
    let subject_id = cas.put_json(&subject_value).unwrap();
    let prior_findings = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id,
            "round": 2,
            "prior_findings": [],
        }))
        .unwrap();
    let prior_demands = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id,
            "round": 2,
            "demands": [],
        }))
        .unwrap();
    let round_two = RoundStartedPayloadV1 {
        round: 2,
        epoch: 1,
        campaign_manifest_id: opened.campaign_manifest_id.clone(),
        subject_id: subject_id.clone(),
        prior_finding_set_id: prior_findings.clone(),
        prior_demand_set_id: prior_demands.clone(),
    };
    let round_two_value = serde_json::to_value(&round_two).unwrap();
    let event = store
        .append(
            "run",
            cas,
            NewEvent::new(EventType::RoundStartedV1, round_two_value)
                .caused_by(opened_event.event_id)
                .correlating(subject_id.clone())
                .referencing(vec![
                    opened.authority_snapshot_id,
                    opened.campaign_manifest_id,
                    head_id,
                    subject_id,
                    prior_findings,
                    prior_demands,
                    change_set_id,
                ]),
        )
        .unwrap();
    store
        .append(
            "run",
            cas,
            NewEvent::new(
                EventType::GenerationAdvancedV1,
                serde_json::json!({ "round": 2 }),
            )
            .caused_by(event.event_id.clone()),
        )
        .unwrap();
    event
}

fn mark(delta: &HeadDeltaV1, path: &str) -> HeadDeltaMarkV1 {
    delta
        .marks
        .iter()
        .find(|entry| entry.path == path)
        .unwrap_or_else(|| panic!("{path} received no mark"))
        .mark
}

#[test]
fn notes_and_head_delta_are_carried_from_the_admitted_attempt_to_the_next_round() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = load_diff_pipeline(directory.path());
    let head_one = manifest(&cas, &[("a.rs", "one\n"), ("b.rs", "two\n")]);
    let authority =
        support::test_diff_round_authority(&cas, &mut store, "run", &head_one, WARM_DIFF_PIPELINE);

    // Round one: the first Attempt times out and is fenced with its own answer never admitted;
    // the retry is admitted and leaves Notes.
    let seen_one = Arc::new(Mutex::new(Vec::new()));
    let mut reviewer = WarmReviewer::new(Some(notes()), &seen_one);
    reviewer.time_out_first = true;
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        head_one.clone(),
        &loaded,
        authority,
    )
    .unwrap()
    .with_adapter("reviewer", Box::new(reviewer));
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();
    drop(kernel);
    let seen_one = seen_one.lock().unwrap().clone();
    assert_eq!(
        seen_one.len(),
        2,
        "one fenced Attempt, one admitted Attempt"
    );
    for seen in &seen_one {
        assert!(seen.notes.is_none() && seen.head_delta.is_none());
        assert_eq!(seen.notes_request, Some(4096));
        assert!(seen.rendered.contains(REQUEST_HEADING));
        assert!(!seen.rendered.contains(NOTES_HEADING));
    }
    let events = store.replay("run").unwrap();
    let admitted: Vec<&RunEvent> = events
        .iter()
        .filter(|event| event.event_type == EventType::AttemptAdmittedV1)
        .collect();
    assert_eq!(admitted.len(), 1);
    let admitted_attempt = admitted[0].attempt_id.clone().unwrap();
    let fenced = events
        .iter()
        .find(|event| event.event_type == EventType::AttemptFencedV1)
        .expect("the timed-out Attempt was fenced");
    assert_ne!(fenced.attempt_id, admitted[0].attempt_id);
    let recorded: Vec<&RunEvent> = events
        .iter()
        .filter(|event| event.event_type == EventType::WorkerNotesRecordedV1)
        .collect();
    assert_eq!(recorded.len(), 1, "only the admitted Attempt records Notes");
    assert_eq!(recorded[0].attempt_id, admitted[0].attempt_id);
    let recorded_notes: WorkerNotesRecordedPayloadV1 =
        serde_json::from_value(recorded[0].payload.clone()).unwrap();
    let notes_id = recorded_notes
        .notes_artifact_id
        .expect("notes were within bound");
    let notes_envelope = cas.get_artifact(&notes_id).unwrap();
    assert_eq!(
        notes_envelope.artifact_type,
        review_core::contract::WORKER_NOTES_V1
    );
    let stored: WorkerNotesV1 = serde_json::from_value(notes_envelope.payload).unwrap();
    assert_eq!(stored.attempt_id, admitted_attempt);
    assert_eq!(stored.inspected.len(), 2);
    assert!(
        stored.inspected[0].tree_entry_digest.is_some(),
        "inspected paths bind to head tree entries"
    );

    // Round two on a new head: a.rs unchanged, b.rs back to the Base, c.rs new. The first
    // Attempt is unavailable and released; the resumed Round reuses the recorded Warm Set.
    let head_two = manifest(&cas, &[("a.rs", "one\n"), ("c.rs", "three\n")]);
    let changed = vec!["a.rs".into(), "c.rs".into()];
    let round_two = start_round_two(&cas, &mut store, &head_two, changed);
    let seen_two = Arc::new(Mutex::new(Vec::new()));
    let authority = RoundAuthority::load(&store, &cas, "run", &round_two.event_id).unwrap();
    let mut outage = WarmReviewer::new(Some(notes()), &seen_two);
    outage.unavailable = true;
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        head_two.clone(),
        &loaded,
        authority,
    )
    .unwrap()
    .with_adapter("reviewer", Box::new(outage));
    let report = loaded.run(&kernel).unwrap();
    assert!(
        !report.complete(),
        "an unavailable reviewer leaves the Round open"
    );
    drop(kernel);
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
    .with_adapter(
        "reviewer",
        Box::new(WarmReviewer::new(Some(notes()), &seen_two)),
    );
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    drop(kernel);

    let seen_two = seen_two.lock().unwrap().clone();
    assert_eq!(
        seen_two.len(),
        2,
        "the released Attempt and the resumed one"
    );
    assert_eq!(seen_two[0].notes, seen_two[1].notes);
    assert_eq!(seen_two[0].head_delta, seen_two[1].head_delta);
    assert_eq!(
        seen_two[0].warm_sections(),
        seen_two[1].warm_sections(),
        "a resumed Round renders the same warm bytes"
    );
    let carried = seen_two[1].notes.as_ref().expect("Notes were carried");
    assert_eq!(
        carried["model_of_change"],
        "b.rs introduces the helper a.rs calls"
    );
    assert_eq!(carried["attempt_id"], admitted_attempt);
    let rendered = &seen_two[1].rendered;
    assert!(rendered.contains(NOTES_HEADING));
    assert!(rendered.contains(DELTA_HEADING));
    assert!(rendered.contains(REQUEST_HEADING));

    let events = store.replay("run").unwrap();
    let selections: Vec<&RunEvent> = events
        .iter()
        .filter(|event| event.event_type == EventType::WarmSetSelectedV1)
        .collect();
    assert_eq!(
        selections.len(),
        1,
        "one Warm Set per node per Round, reused on resume"
    );
    let selection = selections[0];
    assert_eq!(
        selection.causation_id.as_deref(),
        Some(round_two.event_id.as_str())
    );
    assert_eq!(selection.node_id.as_deref(), Some("reviewer"));
    let first_dispatch = events
        .iter()
        .find(|event| {
            event.event_type == EventType::AttemptDispatchedV1
                && event.causation_id.as_deref() == Some(round_two.event_id.as_str())
        })
        .expect("Round two dispatched an Attempt");
    assert!(
        selection.sequence < first_dispatch.sequence,
        "the Warm Set is recorded before the first dispatch of the Round"
    );
    let payload: WarmSetSelectedPayloadV1 =
        serde_json::from_value(selection.payload.clone()).unwrap();
    assert_eq!(
        payload.source_attempt_id.as_deref(),
        Some(admitted_attempt.as_str())
    );
    assert_eq!(
        payload.layers,
        vec![WarmLayerV1::Notes, WarmLayerV1::HeadDelta]
    );
    let envelope = cas.get_artifact(&payload.warm_set_artifact_id).unwrap();
    assert_eq!(envelope.artifact_type, review_core::contract::WARM_SET_V1);
    let set: WarmSetV1 = serde_json::from_value(envelope.payload).unwrap();
    set.validate().unwrap();
    assert_eq!(set.round, 2);
    assert_eq!(set.notes_artifact_id.as_deref(), Some(notes_id.as_str()));
    let delta_id = set
        .head_delta_artifact_id
        .expect("a Head Delta was carried");
    let delta_envelope = cas.get_artifact(&delta_id).unwrap();
    assert_eq!(
        delta_envelope.artifact_type,
        review_core::contract::HEAD_DELTA_V1
    );
    let delta: HeadDeltaV1 = serde_json::from_value(delta_envelope.payload).unwrap();
    delta.validate().unwrap();
    assert_eq!(delta.node, "reviewer");
    assert_eq!(delta.from_snapshot_id, stored.head_snapshot_id);
    assert_eq!(delta.changed_paths, vec!["b.rs", "c.rs"]);
    assert_eq!(mark(&delta, "a.rs"), HeadDeltaMarkV1::Unchanged);
    assert_eq!(
        mark(&delta, "b.rs"),
        HeadDeltaMarkV1::Reverted,
        "a path reverted to Base between Rounds is marked, not omitted"
    );
    assert_eq!(mark(&delta, "c.rs"), HeadDeltaMarkV1::New);
    assert_eq!(
        mark(&delta, "zzz.rs"),
        HeadDeltaMarkV1::Removed,
        "a path only the Notes mention still receives a mark"
    );
}

#[test]
fn a_cold_node_records_nothing_and_renders_no_warm_section() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let head = manifest(&cas, &[("a.rs", "one\n")]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel =
        support::whole_tree_kernel_for_pipeline(&cas, &mut store, "run", head, None, COLD_PIPELINE)
            .with_adapter(
                "reviewer",
                Box::new(WarmReviewer::new(Some(notes()), &seen)),
            );
    let loaded = Definition::from_toml(COLD_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    drop(kernel);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].notes_request, None);
    assert!(!seen[0].rendered.contains(REQUEST_HEADING));
    assert!(!seen[0].rendered.contains(DELTA_HEADING));
    let events = store.replay("run").unwrap();
    assert!(events.iter().all(|event| {
        !matches!(
            event.event_type,
            EventType::WarmSetSelectedV1 | EventType::WorkerNotesRecordedV1
        )
    }));
}
