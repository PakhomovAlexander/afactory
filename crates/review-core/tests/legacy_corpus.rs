//! Acceptance: the contract must ingest real reviewer output unchanged.
//!
//! Two corpora, because they prove different halves of the same claim.
//!
//! `fixtures/legacy/` holds frozen bundles of real reviewer output. If `FindingReport@1`
//! cannot express them unchanged, the schema is wrong, not the payloads. That corpus is
//! private review data and ships only in the hub that captured it, so those tests are
//! `#[ignore]`d: cargo prints `ignored` in its default output, where a runtime skip would
//! print `ok` and hide the reason. `make review-kernel-test-corpus` runs them, and there a
//! missing corpus fails.
//!
//! `fixtures/synthetic/` ships with every checkout, and its `input/*.json` are the stage
//! outputs the real harness actually consumed — including the ones it was built to refuse.
//! Those tests always run, so a checkout without a private corpus still proves the contract
//! on real harness input rather than reporting `ok` for tests that asserted nothing.

use std::path::PathBuf;

use review_core::{FindingReport, LegacyStageOutput, json};

fn corpus_dirs() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/legacy");
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The corpus is private data, so these tests are `#[ignore]`d rather than skipped at runtime:
/// a runtime skip reports `ok`, and `cargo test` hides the notice explaining why, so a checkout
/// with no corpus would show a green acceptance suite that asserted nothing. `ignored` is
/// visible in cargo's own output. A hub that has a corpus runs them with
/// `make review-kernel-test-corpus`, and there their absence is a failure, not a skip.
fn require_corpus(outputs: &[(String, String)]) {
    assert!(
        !outputs.is_empty(),
        "no frozen legacy corpus under fixtures/legacy/ — this test was run explicitly, so its \
         corpus must be present (see fixtures/legacy/README.md)"
    );
}

fn schema_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(name)
}

/// The synthetic cases' stage inputs: real harness input, public, always present. Every case
/// directory holds `input/r<N>.json`, one stage output per round.
fn synthetic_stage_outputs() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(root)
        .expect("the synthetic corpus ships with every checkout")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("input").is_dir())
        .collect();
    cases.sort();

    let mut out = Vec::new();
    for case in cases {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let mut files: Vec<PathBuf> = std::fs::read_dir(case.join("input"))
            .expect("a case with an input/ directory")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        for path in files {
            let label = format!("{name}/{}", path.file_name().unwrap().to_string_lossy());
            out.push((label, std::fs::read_to_string(&path).unwrap()));
        }
    }
    assert!(
        !out.is_empty(),
        "the synthetic corpus carries no stage inputs"
    );
    out
}

fn stage_outputs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for dir in corpus_dirs() {
        let bundle = dir.file_name().unwrap().to_string_lossy().into_owned();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("findings-") && n.ends_with(".json"))
            })
            .collect();
        files.sort();
        for path in files {
            let name = format!("{bundle}/{}", path.file_name().unwrap().to_string_lossy());
            let text = std::fs::read_to_string(&path).unwrap();
            out.push((name, text));
        }
    }
    out
}

/// Every stage output parses under `deny_unknown_fields`, so an unmodelled field fails here
/// rather than being silently dropped on the way into the kernel.
#[test]
#[ignore = "requires a private legacy corpus; see fixtures/legacy/README.md"]
fn every_stage_output_parses() {
    let outputs = stage_outputs();
    require_corpus(&outputs);
    for (name, text) in outputs {
        let stage: LegacyStageOutput =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        // A stage that carries no findings still parses; the count assertions belong to the
        // hub that owns the corpus, not to the harness.
        let _ = stage.findings.len();
    }
}

#[test]
#[ignore = "requires a private legacy corpus; see fixtures/legacy/README.md"]
fn every_finding_converts_and_keeps_its_fix() {
    let outputs = stage_outputs();
    require_corpus(&outputs);
    let mut reports: Vec<FindingReport> = Vec::new();
    for (name, text) in outputs {
        let stage: LegacyStageOutput = serde_json::from_str(&text).unwrap();
        let legacy_fixes: Vec<String> = stage
            .findings
            .iter()
            .map(|f| f.fix.clone().unwrap_or_default())
            .collect();

        let converted = stage
            .into_reports()
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(converted.len(), legacy_fixes.len());
        for (report, fix) in converted.iter().zip(&legacy_fixes) {
            assert_eq!(&report.fix, fix, "{name}: fix must survive the import");
            assert!(!report.title.trim().is_empty());
            assert!(!report.body.trim().is_empty());
        }
        reports.extend(converted);
    }
    // No count pinning here: exact corpus sizes belong to the hub that owns the corpus. A
    // change-wide finding is likewise legal in general, so it is not refused here either.
    assert!(!reports.is_empty());
}

#[test]
#[ignore = "requires a private legacy corpus; see fixtures/legacy/README.md"]
fn every_converted_report_validates_against_the_schema() {
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(schema_path("finding-report-v1.json")).unwrap(),
    )
    .unwrap();
    let validator =
        jsonschema::validator_for(&schema).expect("finding-report-v1.json is not a valid schema");

    let outputs = stage_outputs();
    require_corpus(&outputs);
    for (name, text) in outputs {
        let stage: LegacyStageOutput = serde_json::from_str(&text).unwrap();
        for (index, report) in stage.into_reports().unwrap().into_iter().enumerate() {
            let value = serde_json::to_value(&report).unwrap();
            if !validator.is_valid(&value) {
                let errors: Vec<String> = validator
                    .iter_errors(&value)
                    .map(|e| format!("{} at {}", e, e.instance_path))
                    .collect();
                panic!(
                    "{name} finding {index} fails FindingReport@1: {}",
                    errors.join("; ")
                );
            }
            json::admit(&value).unwrap_or_else(|e| panic!("{name} finding {index}: {e}"));
        }
    }
}

/// The contract, exercised on real harness input in every checkout — including the checkouts
/// that will never hold a private corpus.
///
/// Each synthetic stage output either converts whole, every finding keeping its `fix` and
/// validating against `FindingReport@1`, or is refused. Refusal is a typed error, never a
/// panic and never a half-converted batch: the importer is all-or-nothing per stage.
///
/// Both counters are asserted, because a one-sided result is vacuous in the other direction.
/// A corpus that only converts proves nothing about strictness; one that only refuses proves
/// nothing about the happy path.
#[test]
fn the_contract_holds_on_real_harness_input() {
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(schema_path("finding-report-v1.json")).unwrap(),
    )
    .unwrap();
    let validator =
        jsonschema::validator_for(&schema).expect("finding-report-v1.json is not a valid schema");

    // Two independent refusals, counted apart: the parser rejects a value outside the
    // contract's enums, and the importer rejects a finding the contract cannot express. One
    // counter would let either regress to zero while the other kept the total positive.
    let mut converted = 0;
    let mut refused_by_parser = 0;
    let mut refused_by_importer = 0;
    for (name, text) in synthetic_stage_outputs() {
        let Ok(stage) = serde_json::from_str::<LegacyStageOutput>(&text) else {
            refused_by_parser += 1;
            continue;
        };
        let legacy_fixes: Vec<String> = stage
            .findings
            .iter()
            .map(|f| f.fix.clone().unwrap_or_default())
            .collect();
        let Ok(reports) = stage.into_reports() else {
            refused_by_importer += 1;
            continue;
        };

        assert_eq!(reports.len(), legacy_fixes.len(), "{name}: all-or-nothing");
        for (report, fix) in reports.iter().zip(&legacy_fixes) {
            assert_eq!(&report.fix, fix, "{name}: fix must survive the import");
            assert!(
                !report.title.trim().is_empty(),
                "{name}: empty title admitted"
            );
            assert!(
                !report.body.trim().is_empty(),
                "{name}: empty body admitted"
            );
            let value = serde_json::to_value(report).unwrap();
            assert!(validator.is_valid(&value), "{name}: fails FindingReport@1");
            json::admit(&value).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        converted += 1;
    }

    assert!(
        converted > 0,
        "nothing converted — the happy path is unproven"
    );
    assert!(
        refused_by_parser > 0,
        "no stage output was refused by the parser — enum strictness is unproven"
    );
    assert!(
        refused_by_importer > 0,
        "no stage output was refused by the importer — contract strictness is unproven"
    );
}
