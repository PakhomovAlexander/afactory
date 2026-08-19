//! The live ledger ingest enforces the `FindingReport@1` contract.
//!
//! Before, `Ingest::add_stage_output` hand-built a ledger event from the legacy fields with no
//! validation, so the v1 contract and its acceptance corpus governed a conversion no run
//! performed. Now every finding is validated first; the corpus equivalence (proved in
//! `replay_synthetic`) still holds, and these pin what the gate now refuses on the live path.

use review_core::LegacyStageOutput;
use review_store::{Cas, EventStore, Ingest};

fn ingest_one(finding_json: &str) -> usize {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let stage: LegacyStageOutput = serde_json::from_str(&format!(
        r#"{{"verdict":"request-changes","summary":null,"findings":[{finding_json}],
            "benchmark_demands":[],"disputes":[]}}"#
    ))
    .unwrap();
    let mut ingest = Ingest::new(&mut store, &cas, "run").unwrap();
    ingest.add_stage_output("deep", &stage).unwrap();
    ingest.ledger().len()
}

#[test]
fn a_contract_complete_finding_is_ingested() {
    let n = ingest_one(
        r#"{"severity":"major","file":"src/a.rs","line":7,"title":"T","body":"b","fix":"bound it","confidence":0.9}"#,
    );
    assert_eq!(n, 1);
}

#[test]
fn a_finding_without_a_fix_is_refused_at_ingest() {
    // The exact leak the finding named: a null fix used to reach the ledger and
    // `reviewctl ledger`. FindingReport@1 requires a remedy, so it is now skipped.
    let n = ingest_one(
        r#"{"severity":"major","file":"src/a.rs","line":7,"title":"T","body":"b","fix":null,"confidence":0.9}"#,
    );
    assert_eq!(n, 0, "a fix-less finding must not reach the ledger");
}

#[test]
fn an_out_of_range_confidence_and_a_zero_line_are_refused() {
    assert_eq!(
        ingest_one(
            r#"{"severity":"major","file":"src/a.rs","line":7,"title":"T","body":"b","fix":"f","confidence":1.5}"#,
        ),
        0,
        "confidence outside 0..=1 is refused"
    );
    assert_eq!(
        ingest_one(
            r#"{"severity":"major","file":"src/a.rs","line":0,"title":"T","body":"b","fix":"f","confidence":0.9}"#,
        ),
        0,
        "a zero line is refused"
    );
}

/// A change-wide finding (empty file) is still admitted — empty locations are valid v1.
#[test]
fn a_change_wide_finding_is_still_admitted() {
    let n = ingest_one(
        r#"{"severity":"major","file":"","line":null,"title":"Whole-change concern","body":"b","fix":"f","confidence":0.9}"#,
    );
    assert_eq!(n, 1);
}

/// A reviewer's dispute is folded into the ledger: a `refute` on a prior claim contests it,
/// which the campaign loop and `reviewctl ledger`/`resolve` then see. Before, disputes sat in
/// raw CAS output and affected nothing.
#[test]
fn a_refute_dispute_contests_the_prior_claim() {
    use review_store::{Ledger, Status};
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();

    // Round 1: a finding is reported.
    let mut ingest = Ingest::new(&mut store, &cas, "run").unwrap();
    let stage: LegacyStageOutput = serde_json::from_str(
        r#"{"verdict":"request-changes","summary":null,"findings":[
            {"severity":"major","file":"src/a.rs","line":7,"title":"Claim","body":"b","fix":"f","confidence":0.9}
        ],"benchmark_demands":[],"disputes":[]}"#,
    )
    .unwrap();
    ingest.add_stage_output("architecture", &stage).unwrap();
    let key = ingest.ledger().findings()[0].key.clone();
    assert_eq!(ingest.ledger().get(&key).unwrap().status, Status::Open);

    // A later reviewer refutes it by its key.
    let dispute_stage: LegacyStageOutput = serde_json::from_str(&format!(
        r#"{{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],
            "disputes":[{{"claim_id":"{key}","position":"refute","reason":"not reproducible"}}]}}"#
    ))
    .unwrap();
    let summary = ingest
        .add_stage_output("performance", &dispute_stage)
        .unwrap();
    assert_eq!(summary.contested, 1);

    // Rebuilt from the log alone, the claim is contested — the dispute reached the ledger.
    let ledger = Ledger::rebuild(&store, "run").unwrap();
    assert_eq!(ledger.get(&key).unwrap().status, Status::Contested);
}
