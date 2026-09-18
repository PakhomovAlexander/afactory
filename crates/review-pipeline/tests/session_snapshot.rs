//! Worker warm layers, package P4: the Session Snapshot's two-phase capture and its recovery,
//! the forked resume that sends only the delta prompt, and the compiled Cold Closeout.
//!
//! The acceptance this file carries:
//!
//! - a crash between capture and deletion recovers to a consistent state, with no orphaned CAS
//!   object and no ambient transcript, and without a provider call;
//! - a resumed Attempt records cache-read tokens separately from input tokens, and its manifest
//!   names both the transcript and the delta;
//! - Cold Closeout dispatches only when the warm result would make the Round clean, and its
//!   reservation is protected from the node's own retries.

mod support;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use review_config::Definition;
use review_core::{
    CampaignOpenedPayloadV1, ColdCloseoutDispatchedPayloadV1, EventType, LegacyStageOutput,
    RoundStartedPayloadV1, RunEvent, SessionCleanupOutcomeV1, SessionDropReasonV1,
    SessionSnapshotCleanedPayloadV1, SessionSnapshotPreparedPayloadV1, SessionSnapshotV1,
    SessionSourceV1, SubjectV1, WarmLayerV1, WarmSetSelectedPayloadV1, WarmSetV1,
    session_id_for_attempt,
};
use review_pipeline::{Kernel, RoundAuthority};
use review_runner::{
    CapturedSession, ReceiptedReviewerReturn, ReviewerAdapter, ReviewerInputs, ReviewerReturn,
    RunnerError, SessionCapture, SessionDeletion, SessionLayer, TokenUsage, compose_model_prompt,
};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::{Cas, ConvergencePolicy, EventStore, NewEvent};

const SESSION_PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = ["result"]
warm = { notes = true, session = "if_recent", session_max_age_secs = 3600 }
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

const CLOSEOUT_PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = ["result"]
warm = { notes = true }
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
[convergence]
clean_rounds = 1
max_rounds = 2
gate = "major"
cold_closeout = "one_required"
[budgets]
unit = "tokens"
attempt = 1000
run = 2000
"#;

/// Package instructions of a realistic size: the prefix a forked resume exists to stop paying.
/// A one-line fixture would make the delta prompt's own preamble outweigh it.
const INSTRUCTIONS: &str = "You are the correctness reviewer for this repository. Read the change \
    as a whole before judging any part of it. For every defect you report, name the exact file \
    and line, describe the input or state that reaches it, say what goes wrong, and propose a \
    concrete fix that a maintainer could apply without further research. Prefer fewer, sharper \
    reports over many speculative ones; a report you cannot reproduce from the tree in front of \
    you does not belong in the result. Distinguish a defect in the change from a pre-existing \
    defect the change merely touches, and say which is which. Treat every prior Finding you are \
    assigned as a claim to re-examine against the current tree, not as a fact to restate: \
    corroborate it, mark it not reproduced, or dispute it, with the evidence for each. Never \
    edit files outside the sandbox you were given, never fetch anything from the network, and \
    never include credentials, tokens or absolute host paths in your answer. Your reply must be \
    exactly one JSON document matching the output contract that follows, with no prose before \
    or after it, because the kernel parses it mechanically and rejects anything else.";
const TRANSCRIPT: &[u8] = b"{\"role\":\"user\"}\n{\"role\":\"assistant\"}\n";

fn clean() -> LegacyStageOutput {
    serde_json::from_str(
        r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}"#,
    )
    .unwrap()
}

fn blocking() -> LegacyStageOutput {
    serde_json::from_str(
        r#"{"verdict":"request-changes","summary":null,"findings":[{"severity":"major",
            "file":"a.rs","line":1,"title":"Unbounded loop","body":"spins","fix":"bound it",
            "confidence":0.9}],"benchmark_demands":[],"disputes":[]}"#,
    )
    .unwrap()
}

fn path_digest(session_id: &str) -> String {
    review_store::canonical::blob_content_id(session_id.as_bytes())
}

/// A harness session store held in memory: the same three operations the Claude store performs
/// against the operator's directory, with no filesystem and no provider process.
#[derive(Default)]
struct TestSessions {
    transcripts: Mutex<BTreeMap<String, Vec<u8>>>,
    deleted: Mutex<Vec<String>>,
    materialized: Mutex<Vec<String>>,
}

impl TestSessions {
    fn holds(&self, session_id: &str) -> bool {
        self.transcripts.lock().unwrap().contains_key(session_id)
    }
}

impl SessionLayer for TestSessions {
    fn provider_kind(&self) -> &'static str {
        "claude"
    }

    fn capture(&self, session_id: &str, max_bytes: u64) -> Result<SessionCapture, String> {
        let transcripts = self.transcripts.lock().unwrap();
        Ok(match transcripts.get(session_id) {
            None => SessionCapture::Absent,
            Some(bytes) if bytes.len() as u64 > max_bytes => SessionCapture::OverBound {
                bytes: bytes.len() as u64,
            },
            Some(bytes) => SessionCapture::Captured(CapturedSession {
                transcript: bytes.clone(),
                path_digest: path_digest(session_id),
            }),
        })
    }

    fn delete(&self, session_id: &str, _expected_path_digest: Option<&str>) -> SessionDeletion {
        self.deleted.lock().unwrap().push(session_id.to_string());
        match self.transcripts.lock().unwrap().remove(session_id) {
            Some(_) => SessionDeletion::Deleted,
            None => SessionDeletion::AlreadyAbsent,
        }
    }

    fn materialize(
        &self,
        _working_directory: &Path,
        source_session_id: &str,
        transcript: &[u8],
    ) -> Result<String, String> {
        self.materialized
            .lock()
            .unwrap()
            .push(source_session_id.to_string());
        self.transcripts
            .lock()
            .unwrap()
            .insert(source_session_id.to_string(), transcript.to_vec());
        Ok(path_digest(source_session_id))
    }
}

/// What one Attempt received and what it wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SeenAttempt {
    session_id: Option<String>,
    resumed_from: Option<String>,
    prompt_bytes: usize,
    /// What the same inputs would have cost as a cold prompt: the prefix a resume replaces.
    cold_equivalent_bytes: usize,
    has_instructions: bool,
}

struct SessionReviewer {
    sessions: Arc<TestSessions>,
    seen: Arc<Mutex<Vec<SeenAttempt>>>,
    output: LegacyStageOutput,
    usage: TokenUsage,
    /// Hosts a session at all. A reviewer without one is every adapter but Claude.
    hosts_sessions: bool,
}

impl SessionReviewer {
    fn new(sessions: &Arc<TestSessions>, seen: &Arc<Mutex<Vec<SeenAttempt>>>) -> Self {
        Self {
            sessions: Arc::clone(sessions),
            seen: Arc::clone(seen),
            output: clean(),
            usage: TokenUsage {
                input_tokens: Some(4_000),
                output_tokens: Some(900),
                cache_read_tokens: Some(180_000),
                cache_write_tokens: Some(0),
                reasoning_tokens: None,
                chargeable_tokens: 4_900,
            },
            hosts_sessions: true,
        }
    }
}

impl ReviewerAdapter for SessionReviewer {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        let (prompt, _) =
            compose_model_prompt(INSTRUCTIONS, inputs).map_err(RunnerError::Refused)?;
        let mut cold = inputs.clone();
        cold.session_resume = None;
        let (cold_prompt, _) =
            compose_model_prompt(INSTRUCTIONS, &cold).map_err(RunnerError::Refused)?;
        self.seen.lock().unwrap().push(SeenAttempt {
            session_id: inputs.session_id.clone(),
            resumed_from: inputs
                .session_resume
                .as_ref()
                .map(|resume| resume.session_id.clone()),
            prompt_bytes: prompt.len(),
            cold_equivalent_bytes: cold_prompt.len(),
            has_instructions: prompt.contains(INSTRUCTIONS),
        });
        // The harness writes this Attempt's transcript under the identity the kernel assigned.
        if let Some(session_id) = &inputs.session_id {
            self.sessions
                .transcripts
                .lock()
                .unwrap()
                .insert(session_id.clone(), TRANSCRIPT.to_vec());
        }
        Ok(ReviewerReturn {
            output: self.output.clone(),
            proposal: Ok(None),
            notes: Ok(None),
            cost_tokens: 1,
            raw_artifact: cas.put(b"answer").unwrap(),
        })
    }

    fn invoke_receipted(
        &self,
        cas: &Cas,
        root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReceiptedReviewerReturn, RunnerError> {
        let (_, context_manifest) =
            compose_model_prompt(INSTRUCTIONS, inputs).map_err(RunnerError::Refused)?;
        let returned = self.invoke(cas, root, inputs)?;
        Ok(ReceiptedReviewerReturn {
            returned,
            usage: self.usage.clone(),
            context_manifest,
        })
    }

    fn session_layer(&self) -> Option<&dyn SessionLayer> {
        self.hosts_sessions
            .then(|| &*self.sessions as &dyn SessionLayer)
    }
}

/// Times out once before answering, so a Round's retry meets the protected reservation.
struct RetryingReviewer {
    calls: Mutex<u32>,
    output: LegacyStageOutput,
}

impl ReviewerAdapter for RetryingReviewer {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        _inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        let call = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if call == 1 {
            return Err(RunnerError::TimedOut {
                after_ms: 1,
                raw_artifact: Some(cas.put(b"fenced").unwrap()),
            });
        }
        Ok(ReviewerReturn {
            output: self.output.clone(),
            proposal: Ok(None),
            notes: Ok(None),
            cost_tokens: 1,
            raw_artifact: cas.put(b"answer").unwrap(),
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

fn snapshot_id(cas: &Cas, manifest: &Manifest, tree: &str) -> String {
    let manifest_id = cas
        .put_json(&serde_json::to_value(manifest).unwrap())
        .unwrap();
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

/// Open the next Round of the same whole-tree Campaign, exactly as the authority layer does.
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
            "subject_id": subject_id, "round": round, "prior_findings": [],
        }))
        .unwrap();
    let prior_demands = cas
        .put_json(&serde_json::json!({
            "subject_id": subject_id, "round": round, "demands": [],
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

fn warm_set(cas: &Cas, store: &EventStore, round: usize) -> WarmSetV1 {
    let selections = events_of(store, EventType::WarmSetSelectedV1);
    let selection: WarmSetSelectedPayloadV1 =
        serde_json::from_value(selections[round].payload.clone()).unwrap();
    selection.validate().unwrap();
    let envelope = cas.get_artifact(&selection.warm_set_artifact_id).unwrap();
    let set: WarmSetV1 = serde_json::from_value(envelope.payload).unwrap();
    set.validate().unwrap();
    assert_eq!(set.layers(), selection.layers);
    set
}

#[test]
fn a_session_is_captured_at_seal_and_resumed_forked_in_the_next_round() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(SESSION_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let head = manifest(&cas, &[("a.rs", "one\n")]);
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &head,
        SESSION_PIPELINE,
    );

    let sessions = Arc::new(TestSessions::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", head.clone(), &loaded, authority)
        .unwrap()
        .with_adapter("reviewer", Box::new(SessionReviewer::new(&sessions, &seen)));
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();
    drop(kernel);

    // Round one assigns the kernel's own session, captures it at seal, and deletes the harness
    // copy. Both phases are durable, in that order.
    let first = seen.lock().unwrap()[0].clone();
    let session_id = first.session_id.clone().expect("the kernel assigned one");
    assert!(first.resumed_from.is_none(), "round one starts cold");
    let admitted = events_of(&store, EventType::AttemptAdmittedV1);
    assert_eq!(
        session_id_for_attempt(admitted[0].attempt_id.as_deref().unwrap()),
        Some(session_id.clone()),
        "the session identity is derived from the Attempt, never chosen by the provider"
    );
    let prepared = events_of(&store, EventType::SessionSnapshotPreparedV1);
    let cleaned = events_of(&store, EventType::SessionSnapshotCleanedV1);
    assert_eq!(prepared.len(), 1);
    assert_eq!(cleaned.len(), 1);
    assert!(
        prepared[0].sequence < cleaned[0].sequence,
        "the capture is durable before anything is deleted"
    );
    let capture: SessionSnapshotPreparedPayloadV1 =
        serde_json::from_value(prepared[0].payload.clone()).unwrap();
    capture.validate().unwrap();
    assert_eq!(capture.session_id, session_id);
    assert_eq!(capture.bytes, TRANSCRIPT.len() as u64);
    assert_eq!(
        capture.source,
        SessionSourceV1 {
            provider_kind: "claude".into(),
            path_digest: path_digest(&session_id),
        }
    );
    let cleanup: SessionSnapshotCleanedPayloadV1 =
        serde_json::from_value(cleaned[0].payload.clone()).unwrap();
    assert_eq!(cleanup.outcome, SessionCleanupOutcomeV1::Deleted);
    assert!(cleanup.completed());
    assert!(
        !sessions.holds(&session_id),
        "no ambient transcript survives the seal"
    );
    let snapshot: SessionSnapshotV1 = serde_json::from_value(
        cas.get_artifact(&capture.session_artifact_id)
            .unwrap()
            .payload,
    )
    .unwrap();
    snapshot.validate().unwrap();
    assert_eq!(
        cas.get_bounded(&snapshot.transcript_artifact_id, 4096)
            .unwrap(),
        TRANSCRIPT
    );

    // Round two: the Warm Set carries the transcript, the Attempt resumes it forked, and its
    // prompt is the delta only.
    let round_two = start_round(&cas, &mut store, &head, 2);
    let authority = RoundAuthority::load(&store, &cas, "run", &round_two.event_id).unwrap();
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", head.clone(), &loaded, authority)
        .unwrap()
        .with_adapter("reviewer", Box::new(SessionReviewer::new(&sessions, &seen)));
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();
    let evidence = kernel.selected_attempt_evidence().unwrap();
    drop(kernel);

    let set = warm_set(&cas, &store, 0);
    assert_eq!(
        set.session_artifact_id.as_deref(),
        Some(capture.session_artifact_id.as_str())
    );
    assert!(set.session_dropped.is_none());
    assert!(set.layers().contains(&WarmLayerV1::Session));
    let second = seen.lock().unwrap()[1].clone();
    assert_eq!(second.resumed_from.as_deref(), Some(session_id.as_str()));
    assert_ne!(second.session_id, first.session_id, "a fork of its own");
    assert!(
        !second.has_instructions,
        "the fork already holds the package instructions"
    );
    assert_eq!(
        first.prompt_bytes, first.cold_equivalent_bytes,
        "a cold Attempt is the cold prompt"
    );
    assert!(
        second.prompt_bytes < second.cold_equivalent_bytes,
        "a resumed Attempt sends the delta, not the prefix: delta {} bytes, the cold prompt for \
         the same Round {} bytes",
        second.prompt_bytes,
        second.cold_equivalent_bytes
    );
    assert_eq!(
        sessions.materialized.lock().unwrap().clone(),
        vec![session_id.clone()],
        "the source transcript is re-materialized under the identity --resume names"
    );
    assert!(
        !sessions.holds(&session_id),
        "the working copy leaves no ambient transcript either"
    );

    // A resumed Attempt records its cache reads separately from its input tokens.
    let attempt = evidence
        .iter()
        .find(|attempt| attempt.node == "reviewer")
        .expect("the resumed Attempt");
    assert_eq!(attempt.usage.input_tokens, Some(4_000));
    assert_eq!(attempt.usage.cache_read_tokens, Some(180_000));
    assert_ne!(attempt.usage.input_tokens, attempt.usage.cache_read_tokens);
    let names: Vec<&str> = attempt
        .context_manifest
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert!(names.contains(&"warm_session"), "{names:?}");
    assert!(names.contains(&"warm_session_delta"), "{names:?}");
}

#[test]
fn recovery_finishes_an_interrupted_cleanup_without_a_provider_call() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(SESSION_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let head = manifest(&cas, &[("a.rs", "one\n")]);
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &head,
        SESSION_PIPELINE,
    );

    // A kernel that died between the two phases: the capture is durable and the harness copy is
    // still there. Nothing else in the log says so.
    let sessions = Arc::new(TestSessions::default());
    let orphan_attempt = "a".repeat(26);
    let orphan = session_id_for_attempt(&orphan_attempt).unwrap();
    sessions
        .transcripts
        .lock()
        .unwrap()
        .insert(orphan.clone(), TRANSCRIPT.to_vec());
    let transcript_id = cas.put(TRANSCRIPT).unwrap();
    let snapshot = SessionSnapshotV1 {
        node: "reviewer".into(),
        attempt_id: orphan_attempt.clone(),
        session_id: orphan.clone(),
        head_snapshot_id: authority.head_snapshot_id().to_string(),
        source: SessionSourceV1 {
            provider_kind: "claude".into(),
            path_digest: path_digest(&orphan),
        },
        transcript_artifact_id: transcript_id.clone(),
        bytes: TRANSCRIPT.len() as u64,
        estimated_tokens: 16,
    };
    snapshot.validate().unwrap();
    let (snapshot_id, _) = cas
        .put_artifact(
            review_core::contract::SESSION_SNAPSHOT_V1,
            review_core::Producer::Attempt {
                run_id: "run".into(),
                node_id: "reviewer".into(),
                attempt_id: orphan_attempt.clone(),
            },
            vec![transcript_id.clone()],
            Some(authority.head_snapshot_id().to_string()),
            serde_json::to_value(&snapshot).unwrap(),
        )
        .unwrap();
    let prepared = SessionSnapshotPreparedPayloadV1 {
        session_id: orphan.clone(),
        session_artifact_id: snapshot_id.clone(),
        transcript_artifact_id: transcript_id.clone(),
        source: snapshot.source.clone(),
        bytes: snapshot.bytes,
        estimated_tokens: snapshot.estimated_tokens,
        captured_at_unix_ms: 0,
    };
    prepared.validate().unwrap();
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                EventType::SessionSnapshotPreparedV1,
                serde_json::to_value(&prepared).unwrap(),
            )
            .node("reviewer")
            .attempt(orphan_attempt.clone())
            .caused_by(authority.round_event_id().to_string())
            .referencing(vec![snapshot_id.clone(), transcript_id.clone()]),
        )
        .unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", head.clone(), &loaded, authority)
        .unwrap()
        .with_adapter("reviewer", Box::new(SessionReviewer::new(&sessions, &seen)));
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();
    drop(kernel);

    let cleaned = events_of(&store, EventType::SessionSnapshotCleanedV1);
    let owed: Vec<SessionSnapshotCleanedPayloadV1> = cleaned
        .iter()
        .map(|event| serde_json::from_value(event.payload.clone()).unwrap())
        .filter(|payload: &SessionSnapshotCleanedPayloadV1| payload.session_id == orphan)
        .collect();
    assert_eq!(owed.len(), 1, "recovery finished the cleanup exactly once");
    assert!(owed[0].completed());
    assert!(
        !sessions.holds(&orphan),
        "no ambient transcript survives recovery"
    );
    assert!(
        cas.contains(&transcript_id) && cas.contains(&snapshot_id),
        "the captured object is not orphaned: it stays exactly as the log describes it"
    );
    assert_eq!(
        sessions.deleted.lock().unwrap().first().map(String::as_str),
        Some(orphan.as_str()),
        "the sweep runs before the Round's first Attempt, with no provider call"
    );
    let round_one_prompts = seen.lock().unwrap().len();
    assert_eq!(round_one_prompts, 1, "recovery dispatched no extra Attempt");
}

#[test]
fn a_host_that_does_not_run_the_protocol_falls_back_to_notes_alone() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(SESSION_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let head = manifest(&cas, &[("a.rs", "one\n")]);
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &head,
        SESSION_PIPELINE,
    );
    let sessions = Arc::new(TestSessions::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut reviewer = SessionReviewer::new(&sessions, &seen);
    // Every adapter but Claude, Codex included: no session layer, so the node runs on Notes.
    reviewer.hosts_sessions = false;
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", head.clone(), &loaded, authority)
        .unwrap()
        .with_adapter("reviewer", Box::new(reviewer));
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();
    drop(kernel);
    assert!(seen.lock().unwrap()[0].session_id.is_none());
    assert!(events_of(&store, EventType::SessionSnapshotPreparedV1).is_empty());

    let round_two = start_round(&cas, &mut store, &head, 2);
    let authority = RoundAuthority::load(&store, &cas, "run", &round_two.event_id).unwrap();
    let mut reviewer = SessionReviewer::new(&sessions, &seen);
    reviewer.hosts_sessions = false;
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", head.clone(), &loaded, authority)
        .unwrap()
        .with_adapter("reviewer", Box::new(reviewer));
    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();
    drop(kernel);
    let set = warm_set(&cas, &store, 0);
    assert_eq!(
        set.session_dropped,
        Some(SessionDropReasonV1::ProviderUnsupported)
    );
    assert!(set.session_artifact_id.is_none());
    assert!(!set.layers().contains(&WarmLayerV1::Session));
    assert!(seen.lock().unwrap()[1].resumed_from.is_none());
}

#[test]
fn cold_closeout_confirms_only_a_would_be_clean_warm_result() {
    for (output, confirms) in [(clean(), true), (blocking(), false)] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let loaded = Definition::from_toml(CLOSEOUT_PIPELINE)
            .unwrap()
            .load()
            .unwrap();
        assert_eq!(
            loaded.cold_closeout_nodes().to_vec(),
            vec!["reviewer".to_string()]
        );
        let head = manifest(&cas, &[("a.rs", "one\n")]);
        let authority = support::test_round_authority_for_pipeline(
            &cas,
            &mut store,
            "run",
            &head,
            CLOSEOUT_PIPELINE,
        );
        let sessions = Arc::new(TestSessions::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut reviewer = SessionReviewer::new(&sessions, &seen);
        reviewer.hosts_sessions = false;
        reviewer.output = output.clone();
        let kernel = Kernel::from_loaded(&cas, &mut store, "run", head.clone(), &loaded, authority)
            .unwrap()
            .with_adapter("reviewer", Box::new(reviewer))
            .with_budgets(1_000, 2_000);
        let report = loaded.run(&kernel).unwrap();
        assert!(report.complete(), "{:?}", report.outcomes);
        kernel
            .publish_report(&report, ConvergencePolicy::default())
            .unwrap();
        drop(kernel);

        let dispatched = events_of(&store, EventType::ColdCloseoutDispatchedV1);
        let attempts = seen.lock().unwrap().len();
        if !confirms {
            assert!(
                dispatched.is_empty(),
                "a warm result that already blocks needs no cold confirmation"
            );
            assert_eq!(attempts, 1, "and dispatches no second Attempt");
            continue;
        }
        assert_eq!(dispatched.len(), 1);
        assert_eq!(attempts, 2, "the warm Attempt and its cold confirmation");
        let payload: ColdCloseoutDispatchedPayloadV1 =
            serde_json::from_value(dispatched[0].payload.clone()).unwrap();
        payload.validate().unwrap();
        assert_eq!(payload.node, "reviewer");
        assert_eq!(payload.round, 1);
        assert_eq!(
            payload.reserved_tokens,
            Some(1_000),
            "the confirmation holds a full Attempt reservation of its own"
        );
        assert_ne!(payload.cold_attempt_id, payload.warm_attempt_id);
        let cold_result = payload
            .cold_result_artifact_id
            .expect("the confirmation answered");
        // Results are content-addressed: a confirmation that answers exactly as the warm
        // Attempt did names the same result artifact, and the record tells the two apart by
        // Attempt. What matters is that the cold answer is a readable result of its own.
        assert_eq!(
            cas.get_json(&cold_result).unwrap(),
            cas.get_json(&payload.warm_result_artifact_id).unwrap(),
            "an identical answer is the same content"
        );
        let admitted = events_of(&store, EventType::AttemptAdmittedV1);
        assert_eq!(
            admitted.len(),
            1,
            "a confirmation is never the node's selected Attempt"
        );
        assert_eq!(
            admitted[0].attempt_id.as_deref(),
            Some(payload.warm_attempt_id.as_str())
        );
        // The confirmation carried nothing: no session, no resume, the whole package prompt,
        // and no Notes contract, because a confirmation leaves no state for the next Round.
        let seen = seen.lock().unwrap();
        assert!(seen[1].session_id.is_none() && seen[1].resumed_from.is_none());
        assert!(seen[1].has_instructions);
        assert!(seen[1].prompt_bytes < seen[0].prompt_bytes);
    }
}

#[test]
fn a_retry_cannot_consume_the_confirmations_protected_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let loaded = Definition::from_toml(CLOSEOUT_PIPELINE)
        .unwrap()
        .load()
        .unwrap();
    let head = manifest(&cas, &[("a.rs", "one\n")]);
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &head,
        CLOSEOUT_PIPELINE,
    );
    // The run admits two Attempts. The confirmation's reservation is taken before the warm
    // Attempt's, so the timed-out Attempt's retry meets a cap that is already protecting it.
    let kernel = Kernel::from_loaded(&cas, &mut store, "run", head.clone(), &loaded, authority)
        .unwrap()
        .with_adapter(
            "reviewer",
            Box::new(RetryingReviewer {
                calls: Mutex::new(0),
                output: clean(),
            }),
        )
        .with_budgets(1_000, 2_000);
    let report = loaded.run(&kernel).unwrap();
    let outcome = report.outcome("reviewer").expect("the node ran");
    assert!(
        matches!(outcome, review_graph::NodeOutcome::Failed { error, .. }
            if error.contains("budget exhausted")),
        "{outcome:?}"
    );
    assert!(
        events_of(&store, EventType::ColdCloseoutDispatchedV1).is_empty(),
        "a node with no admitted result has no would-be-clean Round to confirm"
    );
}
