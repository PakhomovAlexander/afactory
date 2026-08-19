//! Importing the frozen legacy ledgers.
//!
//! Whatever frozen ledgers `fixtures/legacy/` holds, the import must round-trip every row's
//! decisions — anything less means an old run cannot be read by new tooling, and the migration
//! would start by losing the history it exists to protect. The corpus is private review data
//! and ships only in the hub that captured it; with none present these tests skip with a
//! notice rather than passing vacuously.

use std::path::PathBuf;

use review_store::{Cas, EventStore, Ledger, LegacyRow, import_ledger_jsonl};

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

/// True when no frozen corpus is present. Private data; a checkout without it skips these
/// tests loudly instead of passing on nothing.
fn corpus_absent(ledgers: &[(String, String)]) -> bool {
    if ledgers.is_empty() {
        eprintln!(
            "skipped: no frozen legacy corpus under fixtures/legacy/ — private review data, \
             present only in the hub that captured it (see fixtures/legacy/README.md)"
        );
        return true;
    }
    false
}

fn rows(jsonl: &str) -> Vec<LegacyRow> {
    jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("frozen ledger row parses"))
        .collect()
}

#[test]
fn the_whole_frozen_corpus_imports() {
    let all = ledgers();
    if corpus_absent(&all) {
        return;
    }
    for (name, jsonl) in all {
        let expected = rows(&jsonl);
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();

        let imported = import_ledger_jsonl(&mut store, &cas, &name, &jsonl).unwrap();
        assert_eq!(imported, expected.len(), "{name}: import count");

        let ledger = Ledger::rebuild(&store, &name).unwrap();
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
                finding.current_note().map(str::to_string),
                row.note,
                "{name}/{}",
                row.fp
            );
        }
    }
}

/// An import is idempotent in the only sense that matters: importing into two separate stores
/// yields the same projection, so a re-import cannot drift.
#[test]
fn importing_twice_yields_the_same_projection() {
    let all = ledgers();
    if corpus_absent(&all) {
        return;
    }
    let Some((name, jsonl)) = all.into_iter().find(|(_, j)| !j.trim().is_empty()) else {
        eprintln!("skipped: the corpus holds no non-empty ledger");
        return;
    };

    let mut projections = Vec::new();
    for _ in 0..2 {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        import_ledger_jsonl(&mut store, &cas, &name, &jsonl).unwrap();
        let ledger = Ledger::rebuild(&store, &name).unwrap();
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
