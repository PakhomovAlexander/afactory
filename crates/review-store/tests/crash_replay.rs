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

use review_core::LegacyStageOutput;
use review_store::{Cas, EventStore, Ingest, Ledger, LedgerProjection, NewEvent, Status};
use support::opened_legacy_round;

fn stage(json: &str) -> LegacyStageOutput {
    serde_json::from_str(json).unwrap()
}

fn one_finding(severity: &str, file: &str, title: &str) -> LegacyStageOutput {
    stage(&format!(
        r#"{{"verdict":"request-changes","summary":null,
            "findings":[{{"severity":"{severity}","file":"{file}","line":7,
                          "title":"{title}","body":"b","fix":"f","confidence":0.9}}],
            "benchmark_demands":[],"disputes":[]}}"#
    ))
}

fn snapshot(ledger: &Ledger) -> Vec<(String, Status, u32, u32, usize)> {
    ledger
        .findings()
        .into_iter()
        .map(|f| {
            (
                f.key.clone(),
                f.status,
                f.news_round,
                f.last_seen_round,
                f.reports.len(),
            )
        })
        .collect()
}

/// Ingest one live reviewer result under a freshly opened Round of `run_id`.
fn ingest_one(store: &mut EventStore, cas: &Cas, run_id: &str, output: &LegacyStageOutput) {
    let authority = opened_legacy_round(store, cas, run_id);
    Ingest::new(store, cas, run_id)
        .unwrap()
        .under_round(&authority.round_event_id)
        .add_live_stage_outputs(&[("deep", output)])
        .unwrap();
}

/// Build a run of a few rounds against a store on disk, then hand back its directory.
fn build_run(dir: &Path) -> Vec<(String, Status, u32, u32, usize)> {
    let mut store = EventStore::open(dir.join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.join("cas")).unwrap();
    let authority = opened_legacy_round(&mut store, &cas, "run");
    let mut ingest = Ingest::new(&mut store, &cas, "run")
        .unwrap()
        .under_round(&authority.round_event_id);
    ingest
        .add_live_stage_outputs(&[(
            "deep-r1",
            &one_finding("major", "src/a.rs", "Retry loop can spin forever"),
        )])
        .unwrap();
    let key = ingest.ledger().findings()[0].key.clone();
    drop(ingest);
    support::resolve(
        &mut store,
        &cas,
        "run",
        &authority,
        &key,
        Status::Fixed,
        "capped",
    );
    let mut ingest = Ingest::new(&mut store, &cas, "run")
        .unwrap()
        .under_round(&authority.round_event_id);
    ingest.advance().unwrap();
    ingest
        .add_live_stage_outputs(&[(
            "deep-r2",
            &one_finding("blocker", "src/a.rs", "Retry loop can spin forever"),
        )])
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
    let rebuilt = snapshot(
        &LedgerProjection::rebuild(&store, &cas, "run")
            .unwrap()
            .into_ledger(),
    );
    assert_eq!(live, rebuilt, "rebuild must reproduce the committed state");

    // And the reopen-after-fix path really was exercised, so this is not a trivial equality.
    let (_, status, news_round, last_seen, reports) = &rebuilt[0];
    assert_eq!(*status, Status::Open, "the fix did not hold");
    assert_eq!((*news_round, *last_seen), (2, 2));
    assert_eq!(*reports, 2, "both reports survived");
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
    let authority = opened_legacy_round(&mut store, &cas, "run");
    let report_id = cas
        .put_json(
            &serde_json::to_value(review_core::FindingReport {
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
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                review_store::ledger::EVENT_FINDING_REPORTED,
                serde_json::json!({
                    "key": "claim",
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
            .caused_by(authority.round_event_id)
            .correlating("claim")
            .referencing(vec![report_id]),
        )
        .unwrap();

    let ledger = LedgerProjection::rebuild(&store, &cas, "run")
        .unwrap()
        .into_ledger();
    let finding = ledger.get("claim").unwrap();
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
    assert_eq!(ledger.len(), 1);

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
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_eq!(a.findings()[0].title, "only in a");
    assert_eq!(b.findings()[0].title, "only in b");
}
