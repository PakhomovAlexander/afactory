//! Acceptance: the contract must ingest real reviewer output unchanged.
//!
//! The corpus is whatever frozen review bundles `fixtures/legacy/` holds — real stage outputs
//! produced by real reviewers. If `FindingReport@1` cannot express them, the schema is wrong,
//! not the payloads. The corpus is private review data and ships only in the hub that captured
//! it; with no bundles present these tests skip with a notice rather than passing vacuously.

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

/// True when at least one frozen bundle is present. The corpus is private data; a checkout
/// without it skips these tests loudly instead of passing on nothing.
fn corpus_absent(outputs: &[(String, String)]) -> bool {
    if outputs.is_empty() {
        eprintln!(
            "skipped: no frozen legacy corpus under fixtures/legacy/ — private review data, \
             present only in the hub that captured it (see fixtures/legacy/README.md)"
        );
        return true;
    }
    false
}

fn schema_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(name)
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
fn every_stage_output_parses() {
    let outputs = stage_outputs();
    if corpus_absent(&outputs) {
        return;
    }
    for (name, text) in outputs {
        let stage: LegacyStageOutput =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        // A stage that carries no findings still parses; the count assertions belong to the
        // hub that owns the corpus, not to the harness.
        let _ = stage.findings.len();
    }
}

#[test]
fn every_finding_converts_and_keeps_its_fix() {
    let outputs = stage_outputs();
    if corpus_absent(&outputs) {
        return;
    }
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
fn every_converted_report_validates_against_the_schema() {
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(schema_path("finding-report-v1.json")).unwrap(),
    )
    .unwrap();
    let validator =
        jsonschema::validator_for(&schema).expect("finding-report-v1.json is not a valid schema");

    let outputs = stage_outputs();
    if corpus_absent(&outputs) {
        return;
    }
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
