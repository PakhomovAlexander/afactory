//! Importing the frozen legacy ledgers.
//!
//! The import must round-trip every row's decisions — anything less means an old run cannot be
//! read by new tooling, and the migration would start by losing the history it exists to
//! protect.
//!
//! Two corpora prove it. The frozen bundles under `fixtures/legacy/` are real review data, so
//! they ship only in the hub that captured them, and their test is `#[ignore]`d when absent.
//! The ledgers under `fixtures/synthetic/` were written by the same `ledger.sh` against
//! constructed cases, carry nothing private, and ship with every checkout — so the importer is
//! never merely reported as `ok` without having run.

use std::path::PathBuf;

use review_store::{
    Cas, EventStore, Ledger, LegacyRow, Status, import_ledger_jsonl, legacy_fingerprint,
};

fn legacy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/legacy")
}

/// Every frozen `ledger.jsonl`, including the two empty ones — a run that found nothing is a
/// case, not an absence of one.
fn ledgers() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(legacy_dir()) else {
        return out;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        for candidate in [dir.join("ledger.jsonl"), dir.join("ledger/ledger.jsonl")] {
            if candidate.exists() {
                out.push((
                    dir.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read_to_string(&candidate).unwrap(),
                ));
            }
        }
    }
    out
}

/// `#[ignore]` rather than a runtime skip, for the reason spelled out in
/// `review-core/tests/legacy_corpus.rs`: a runtime skip reports `ok` and cargo hides the
/// notice, so the absence of a corpus would look exactly like coverage.
fn require_corpus(ledgers: &[(String, String)]) {
    assert!(
        !ledgers.is_empty(),
        "no frozen legacy corpus under fixtures/legacy/ — this test was run explicitly, so its \
         corpus must be present (see fixtures/legacy/README.md)"
    );
}

/// Every `ledger.jsonl` under `fixtures/synthetic/`, produced by running the real harness.
fn synthetic_ledgers() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(root)
        .expect("the synthetic corpus ships with every checkout")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("ledger.jsonl").is_file())
        .collect();
    cases.sort();
    cases
        .into_iter()
        .map(|case| {
            let name = case.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(case.join("ledger.jsonl")).unwrap();
            (name, text)
        })
        .collect()
}

fn rows(jsonl: &str) -> Vec<LegacyRow> {
    jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("frozen ledger row parses"))
        .collect()
}

/// Import one ledger and compare every row's decisions against the projection. Returns the
/// row count, so a caller can prove it asserted on something.
fn round_trips(name: &str, jsonl: &str) -> usize {
    let expected = rows(jsonl);
    let dir = tempfile::tempdir().unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();

    let imported = import_ledger_jsonl(&mut store, &cas, name, jsonl).unwrap();
    assert_eq!(imported, expected.len(), "{name}: import count");

    let ledger = Ledger::rebuild(&store, &cas, name).unwrap();
    assert_eq!(ledger.len(), expected.len(), "{name}: projected count");

    for row in &expected {
        let finding = ledger
            .get(&row.fp)
            .unwrap_or_else(|| panic!("{name}: {} missing after import", row.fp));
        assert_eq!(finding.status.as_str(), row.status, "{name}/{}", row.fp);
        assert_eq!(finding.severity, row.severity, "{name}/{}", row.fp);
        assert_eq!(finding.news_round, row.round, "{name}/{}", row.fp);
        assert_eq!(
            finding.last_seen_round, row.last_seen_round,
            "{name}/{}",
            row.fp
        );
        assert_eq!(finding.title, row.title, "{name}/{}", row.fp);
        assert_eq!(finding.file, row.file, "{name}/{}", row.fp);
        assert_eq!(
            finding.fix, None,
            "{name}/{}: an artifact-less import must not invent a fix",
            row.fp
        );
        // The importer emits a resolution only for a row whose status is not `open`, and
        // the note rides on that resolution — so an open row's note is dropped. That is
        // the documented loss of importing final state, pinned here in both directions
        // rather than assumed. The frozen bundles are uniformly terminal, so they only
        // ever exercise the first branch; the synthetic ledgers exercise both.
        let terminal = Status::parse(&row.status).is_some_and(|s| s != Status::Open);
        if terminal {
            assert_eq!(
                finding.current_note().map(str::to_string),
                row.note,
                "{name}/{}",
                row.fp
            );
        } else {
            assert_eq!(
                finding.current_note(),
                None,
                "{name}/{}: an open row carries no imported note",
                row.fp
            );
        }
    }
    expected.len()
}

/// The importer, exercised in every checkout. These 13 ledgers were written by the real
/// `ledger.sh` while generating the synthetic cases, so they are harness output rather than
/// our reading of it — and unlike the frozen bundles they carry no private review data, so
/// they ship. Without them a checkout with no private corpus would report `ok` for an
/// importer nothing had run.
#[test]
fn the_synthetic_ledgers_import() {
    let all = synthetic_ledgers();
    assert!(!all.is_empty(), "the synthetic corpus carries no ledgers");

    let mut rows_checked = 0;
    for (name, jsonl) in all {
        rows_checked += round_trips(&name, &jsonl);
    }
    assert!(
        rows_checked > 0,
        "every synthetic ledger was empty — the round-trip asserted nothing"
    );
}

#[test]
#[ignore = "requires a private legacy corpus; see fixtures/legacy/README.md"]
fn the_whole_frozen_corpus_imports() {
    let all = ledgers();
    require_corpus(&all);
    for (name, jsonl) in all {
        round_trips(&name, &jsonl);
    }
}

/// An import is idempotent in the only sense that matters: importing into two separate stores
/// yields the same projection, so a re-import cannot drift.
#[test]
fn importing_twice_yields_the_same_projection() {
    // The synthetic ledgers ship with every checkout, so this never degrades into a skip.
    let (name, jsonl) = synthetic_ledgers()
        .into_iter()
        .find(|(_, j)| !j.trim().is_empty())
        .expect("a non-empty synthetic ledger exists");

    let mut projections = Vec::new();
    for _ in 0..2 {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        import_ledger_jsonl(&mut store, &cas, &name, &jsonl).unwrap();
        let ledger = Ledger::rebuild(&store, &cas, &name).unwrap();
        projections.push(
            ledger
                .findings()
                .into_iter()
                .map(|f| {
                    (
                        f.key.clone(),
                        f.status,
                        f.severity,
                        f.news_round,
                        f.last_seen_round,
                    )
                })
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(projections[0], projections[1]);
}

/// The fingerprint must agree with the shell implementation, checked against digests the shell
/// actually wrote: every row of every synthetic ledger, `fp` recomputed from its own `file` and
/// `title`. These ship, so the property is proved in every checkout — and no private review
/// data has to be pasted into a test to prove it.
#[test]
fn the_synthetic_fingerprints_match_the_shell() {
    let mut checked = 0;
    for (name, jsonl) in synthetic_ledgers() {
        for row in rows(&jsonl) {
            assert_eq!(
                legacy_fingerprint(&row.file, &row.title),
                row.fp,
                "{name}: {} ({})",
                row.title,
                row.file
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no rows checked — the corpus proved nothing");
}
