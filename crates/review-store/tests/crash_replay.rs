//! Crash and replay.
//!
//! The design puts this before any model is ever invoked, and the reason is worth stating: a
//! projection that can be rebuilt is only useful if the rebuild is *the same* rebuild. These
//! tests kill the process at each boundary the store crosses and check what the log says
//! afterwards.
//!
//! The failure being hunted is not a lost event. It is a run that replays into a different state
//! than it committed, silently, so that "rebuild the ledger" quietly becomes "invent one".

mod support;

use std::path::Path;

use review_core::ReviewerStageOutput;
use review_store::{Cas, EventStore, Finding, Ingest, Ledger, LedgerProjection, NewEvent, Status};
use support::{add_flat_results, opened_round};

fn stage(json: &str) -> ReviewerStageOutput {
    serde_json::from_str(json).unwrap()
}

fn one_finding(severity: &str, file: &str, title: &str) -> ReviewerStageOutput {
    stage(&format!(
        r#"{{"reports":[{{"severity":"{severity}","file":"{file}","line":7,
                          "title":"{title}","body":"b","fix":"f","confidence":0.9}}],
            "benchmark_demands":[],"dispositions":[]}}"#
    ))
}

type Snapshot = (Vec<Finding>, Vec<review_core::DemandSetEntryV1>);

fn snapshot(ledger: &Ledger) -> Snapshot {
    (ledger.finding_views(), ledger.demand_views())
}

fn key_of(ledger: &Ledger, title: &str) -> String {
    ledger
        .findings()
        .into_iter()
        .find(|finding| finding.title == title)
        .unwrap_or_else(|| panic!("no Finding titled `{title}`"))
        .key
        .clone()
}

/// Admit one reviewer result under a freshly opened Round of `run_id`.
fn ingest_one(store: &mut EventStore, cas: &Cas, run_id: &str, output: &ReviewerStageOutput) {
    let round = opened_round(store, cas, run_id);
    let mut ingest = Ingest::new(store, cas, run_id)
        .unwrap()
        .under_round(&round.round_event_id);
    add_flat_results(
        &mut ingest,
        cas,
        run_id,
        &round,
        &[("deep", "01jd8m4qz9k7v3n2p6r8t0w201", output)],
    )
    .unwrap();
}

/// Build a Round against a store on disk and hand back its projection. Two reviewers report
/// three claims and a Demand; a third reviewer then corroborates one claim and disputes another.
fn build_run(dir: &Path) -> Snapshot {
    let mut store = EventStore::open(dir.join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.join("cas")).unwrap();
    let round = opened_round(&mut store, &cas, "run");
    let mut ingest = Ingest::new(&mut store, &cas, "run")
        .unwrap()
        .under_round(&round.round_event_id);
    let cross = stage(
        r#"{"reports":[
              {"severity":"blocker","file":"src/queue.rs","line":3,"title":"Queue grows without bound",
               "body":"b","fix":"f","confidence":0.8},
              {"severity":"minor","file":"src/lib.rs","line":1,"title":"Misleading comment",
               "body":"b","fix":"f","confidence":0.5}],
            "benchmark_demands":[{"claim":"Enqueue stays O(1)","why":"the queue is on the hot path",
                                  "suggested_method":"time 10^6 enqueues"}],
            "dispositions":[]}"#,
    );
    add_flat_results(
        &mut ingest,
        &cas,
        "run",
        &round,
        &[
            (
                "deep",
                "01jd8m4qz9k7v3n2p6r8t0w201",
                &one_finding("major", "src/a.rs", "Retry loop can spin forever"),
            ),
            ("cross", "01jd8m4qz9k7v3n2p6r8t0w202", &cross),
        ],
    )
    .unwrap();

    let retry = key_of(ingest.ledger(), "Retry loop can spin forever");
    let queue = key_of(ingest.ledger(), "Queue grows without bound");
    let positions = stage(&format!(
        r#"{{"reports":[],"benchmark_demands":[],
            "dispositions":[
              {{"finding_id":"{retry}","position":"corroborate","reason":"reproduced"}},
              {{"finding_id":"{queue}","position":"dispute","reason":"the producer is bounded"}}]}}"#
    ));
    add_flat_results(
        &mut ingest,
        &cas,
        "run",
        &round,
        &[("performance", "01jd8m4qz9k7v3n2p6r8t0w203", &positions)],
    )
    .unwrap();
    snapshot(ingest.ledger())
}

#[test]
fn the_projection_survives_process_death() {
    let dir = tempfile::tempdir().unwrap();
    let live = build_run(dir.path());
    // Everything above is dropped here — connection closed, no in-memory state left.

    let store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let ledger = LedgerProjection::rebuild(&store, &cas, "run")
        .unwrap()
        .into_ledger();
    let rebuilt = snapshot(&ledger);
    assert_eq!(live, rebuilt, "rebuild must reproduce the committed state");
    assert_eq!(
        ledger
            .convergence(support::default_policy())
            .authority_failures_recent,
        0,
        "the history is one well-formed Round"
    );

    // Every kind of transition really was exercised, so this is not a trivial equality.
    let (findings, demands) = &rebuilt;
    let findings: Vec<(&str, Status, usize)> = findings
        .iter()
        .map(|f| (f.title.as_str(), f.status, f.reports.len()))
        .collect();
    assert_eq!(
        findings,
        [
            ("Retry loop can spin forever", Status::Open, 2),
            ("Queue grows without bound", Status::Contested, 1),
            ("Misleading comment", Status::Open, 1),
        ],
        "the corroboration attached a second Report and the dispute contested its claim"
    );
    assert_eq!(demands.len(), 1);
}

#[test]
fn rebuilding_twice_is_the_same_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    build_run(dir.path());
    let store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let first = snapshot(
        &LedgerProjection::rebuild(&store, &cas, "run")
            .unwrap()
            .into_ledger(),
    );
    let second = snapshot(
        &LedgerProjection::rebuild(&store, &cas, "run")
            .unwrap()
            .into_ledger(),
    );
    assert_eq!(first, second);
}

/// Claim content comes from the referenced `FindingReport@1` artifact. Fields a writer copies
/// into the event payload beside `report_id` are never projection authority.
#[test]
fn the_report_artifact_not_the_event_copy_is_projection_authority() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let round = opened_round(&mut store, &cas, "run");
    let (report_id, _) = cas
        .put_artifact(
            review_core::contract::FINDING_REPORT_V1,
            review_core::Producer::Attempt {
                run_id: "run".into(),
                node_id: "architecture".into(),
                attempt_id: "01jd8m4qz9k7v3n2p6r8t0w201".into(),
            },
            Vec::new(),
            Some(round.head.clone()),
            serde_json::to_value(review_core::FindingReport {
                title: "Canonical title".into(),
                severity: review_core::Severity::Blocker,
                locations: vec![review_core::Location {
                    path: "src/authority.rs".into(),
                    line: Some(19),
                    end_line: None,
                }],
                body: "canonical body".into(),
                fix: "canonical fix".into(),
                confidence: 0.99,
                failure_trace: None,
                rule_id: None,
                occurrence_key: None,
                relations: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();
    let key = review_store::ingest::canonical_finding_id(&report_id);
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                review_store::ledger::EVENT_FINDING_REPORTED,
                serde_json::json!({
                    "key": key,
                    "round": 1,
                    "source": "architecture",
                    "severity": "minor",
                    "file": "forged.rs",
                    "line": 1,
                    "title": "Forged title",
                    "body": "forged body",
                    "confidence": 0.1,
                    "report_id": report_id,
                }),
            )
            .caused_by(round.round_event_id)
            .correlating(key.clone())
            .referencing(vec![report_id]),
        )
        .unwrap();

    let ledger = LedgerProjection::rebuild(&store, &cas, "run")
        .unwrap()
        .into_ledger();
    let finding = ledger.get(&key).unwrap();
    assert_eq!(finding.title, "Canonical title");
    assert_eq!(finding.body, "canonical body");
    assert_eq!(finding.fix, "canonical fix");
    assert_eq!(finding.file, "src/authority.rs");
    assert_eq!(finding.line, Some(19));
    assert_eq!(finding.severity, review_core::Severity::Blocker);
    assert_eq!(finding.confidence, Some(0.99));
}

/// A crash between publishing an artifact and appending the event that references it leaves an
/// unreferenced object. That is the *safe* direction: garbage is collectible, a dangling
/// reference is not recoverable.
#[test]
fn a_crash_after_publish_but_before_append_leaves_only_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();

    let orphan = cas.put(b"an artifact whose event never landed").unwrap();
    assert!(cas.contains(&orphan));
    assert!(store.is_empty("run").unwrap());

    // The run continues normally; the orphan is inert.
    ingest_one(
        &mut store,
        &cas,
        "run",
        &one_finding("major", "src/a.rs", "t"),
    );
    let ledger = LedgerProjection::rebuild(&store, &cas, "run")
        .unwrap()
        .into_ledger();
    assert_eq!(ledger.findings().len(), 1);

    let referenced: Vec<String> = store
        .replay("run")
        .unwrap()
        .into_iter()
        .flat_map(|e| e.artifact_refs)
        .collect();
    assert!(
        !referenced.contains(&orphan),
        "an orphan must never become reachable"
    );
}

/// Appending is all-or-nothing: a refused event leaves the sequence untouched, so the next
/// successful append is not written into a hole.
#[test]
fn a_refused_append_does_not_consume_a_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();

    ingest_one(
        &mut store,
        &cas,
        "run",
        &one_finding("major", "src/a.rs", "t"),
    );

    let missing = review_store::canonical::blob_content_id(b"not stored");
    let before = store.len("run").unwrap();
    let err = store.append(
        "run",
        &cas,
        review_store::NewEvent::new(
            review_core::EventType::SourceCapturedV1,
            serde_json::json!({}),
        )
        .referencing(vec![missing]),
    );
    assert!(err.is_err());
    assert_eq!(store.len("run").unwrap(), before, "no partial write");

    let next = store
        .append(
            "run",
            &cas,
            review_store::NewEvent::new(
                review_core::EventType::SourceCapturedV1,
                serde_json::json!({}),
            ),
        )
        .unwrap();
    assert_eq!(next.sequence, before, "the sequence stayed dense");
}

/// Two runs in one store never see each other's events — the projection is per-run by
/// construction, not by convention.
#[test]
fn runs_are_isolated_in_one_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();

    ingest_one(
        &mut store,
        &cas,
        "run-a",
        &one_finding("major", "src/a.rs", "only in a"),
    );
    ingest_one(
        &mut store,
        &cas,
        "run-b",
        &one_finding("minor", "src/b.rs", "only in b"),
    );

    let a = LedgerProjection::rebuild(&store, &cas, "run-a")
        .unwrap()
        .into_ledger();
    let b = LedgerProjection::rebuild(&store, &cas, "run-b")
        .unwrap()
        .into_ledger();
    assert_eq!(a.findings().len(), 1);
    assert_eq!(b.findings().len(), 1);
    assert_eq!(a.findings()[0].title, "only in a");
    assert_eq!(b.findings()[0].title, "only in b");
}
