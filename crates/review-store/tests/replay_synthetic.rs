//! Acceptance for the migration: the kernel must reach the shell harness's conclusions.
//!
//! Every case under `fixtures/synthetic/` is replayed by parsing its own `transcript.txt` — the
//! recorded command sequence — and driving the event store with the same inputs. Three things
//! are compared against what the harness actually printed:
//!
//! 1. the `new= dup= reopened= escalated= open=` tally after every ingest,
//! 2. the `converged` verdict, its exit code, and both of its counters,
//! 3. the final ledger, row by row.
//!
//! The test discovers cases from the directory, so a case added to the corpus is covered here
//! without touching this file. Nothing is hardcoded about which cases exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use review_core::{LegacyStageOutput, Severity};
use review_store::{
    Cas, ConvergencePolicy, EventStore, Ingest, LegacyRow, Status, legacy::legacy_fingerprint,
};

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic")
}

/// One recorded harness invocation.
#[derive(Debug)]
struct Step {
    argv: Vec<String>,
    exit: i32,
    stdout: String,
}

fn parse_transcript(text: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    for block in text.split("### ").skip(1) {
        let mut lines = block.lines();
        let argv: Vec<String> = lines
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let mut exit = 0;
        let mut stdout = String::new();
        let mut in_stdout = false;
        for line in lines {
            if let Some(code) = line.strip_prefix("exit=") {
                exit = code.trim().parse().unwrap_or(0);
            } else if line == "--- stdout" {
                in_stdout = true;
            } else if line == "--- stderr" {
                in_stdout = false;
            } else if in_stdout {
                stdout.push_str(line);
                stdout.push('\n');
            }
        }
        steps.push(Step { argv, exit, stdout });
    }
    steps
}

fn summary_line(stdout: &str) -> Option<&str> {
    stdout.lines().find(|l| l.starts_with("new="))
}

/// `round=2 open_blocking(>=major)=2 new(>=major)_in_last_1_rounds=2`
fn parse_convergence_line(stdout: &str) -> Option<(u32, usize, usize)> {
    let line = stdout.lines().find(|l| l.starts_with("round="))?;
    let mut round = None;
    let mut open_blocking = None;
    let mut new_recent = None;
    for token in line.split_whitespace() {
        let (name, value) = token.rsplit_once('=')?;
        let value: usize = value.parse().ok()?;
        if name == "round" {
            round = Some(value as u32);
        } else if name.starts_with("open_blocking") {
            open_blocking = Some(value);
        } else if name.starts_with("new(") {
            new_recent = Some(value);
        }
    }
    Some((round?, open_blocking?, new_recent?))
}

fn policy_from(argv: &[String]) -> ConvergencePolicy {
    let mut policy = ConvergencePolicy::default();
    let mut i = 0;
    while i < argv.len() {
        let value = argv.get(i + 1).cloned().unwrap_or_default();
        match argv[i].as_str() {
            "--clean-rounds" => policy.clean_rounds = value.parse().unwrap_or(1),
            "--max-rounds" => policy.max_rounds = value.parse().unwrap_or(3),
            "--gate" => {
                policy.gate = match value.as_str() {
                    "blocker" => Severity::Blocker,
                    "minor" => Severity::Minor,
                    _ => Severity::Major,
                }
            }
            _ => {}
        }
        i += 1;
    }
    policy
}

fn ledger_cases() -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(corpus())
        .expect("synthetic corpus missing")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("ledger.jsonl").exists())
        .collect();
    cases.sort();
    cases
}

fn committed_rows(case: &Path) -> Vec<LegacyRow> {
    std::fs::read_to_string(case.join("ledger.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("committed ledger row parses"))
        .collect()
}

struct Replay {
    ledger_rows: BTreeMap<String, LegacyRow>,
}

fn replay(case: &Path) -> Replay {
    let name = case.file_name().unwrap().to_string_lossy().into_owned();
    let dir = tempfile::tempdir().unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut ingest = Ingest::new(&mut store, &cas, "replay").unwrap();

    let transcript = std::fs::read_to_string(case.join("transcript.txt")).unwrap();
    for step in parse_transcript(&transcript) {
        // argv: ledger.sh <cmd> . <rest...>
        let Some(cmd) = step.argv.get(1) else {
            continue;
        };
        let rest: Vec<String> = step.argv.iter().skip(3).cloned().collect();
        match cmd.as_str() {
            "init" => {}
            "bump" => {
                ingest.advance().unwrap();
            }
            "add" => {
                let source = rest
                    .iter()
                    .position(|a| a == "--source")
                    .and_then(|i| rest.get(i + 1))
                    .cloned()
                    .expect("add carries --source");
                let file = rest.last().cloned().expect("add carries a findings file");
                let text = std::fs::read_to_string(case.join(&file)).unwrap();

                match serde_json::from_str::<LegacyStageOutput>(&text) {
                    Err(_) => {
                        // The harness refuses the batch and exits 2 without touching state.
                        assert_eq!(step.exit, 2, "{name}: expected a refused batch");
                        assert!(
                            ingest.ledger().is_empty(),
                            "{name}: a refused batch must not be ingested"
                        );
                        continue;
                    }
                    Ok(stage) => {
                        let got = ingest.add_stage_output(&source, &stage).unwrap();
                        let expected = summary_line(&step.stdout)
                            .unwrap_or_else(|| panic!("{name}: add printed no summary"));
                        assert_eq!(
                            got.to_string(),
                            expected.trim(),
                            "{name}: ingest tally differs from the harness"
                        );
                    }
                }
            }
            "resolve" => {
                let key = rest[0].clone();
                let status = Status::parse(&rest[1]).expect("known status");
                let note = rest
                    .iter()
                    .position(|a| a == "--note")
                    .map(|i| rest[i + 1..].join(" "));
                ingest.resolve(&key, status, note.as_deref()).unwrap();
            }
            "converged" => {
                let policy = policy_from(&rest);
                let got = ingest.ledger().convergence(policy);
                assert_eq!(
                    got.verdict.exit_code(),
                    step.exit,
                    "{name}: convergence exit code differs from the harness"
                );
                if let Some((round, open_blocking, new_recent)) =
                    parse_convergence_line(&step.stdout)
                {
                    assert_eq!(got.round, round, "{name}: round");
                    assert_eq!(got.open_blocking, open_blocking, "{name}: open_blocking");
                    assert_eq!(got.new_recent, new_recent, "{name}: new_recent");
                }
            }
            other => panic!("{name}: transcript uses an unhandled command: {other}"),
        }
    }

    let ledger_rows = ingest
        .ledger()
        .findings()
        .into_iter()
        .map(|f| {
            (
                f.key.clone(),
                LegacyRow {
                    fp: f.key.clone(),
                    round: f.news_round,
                    last_seen_round: f.last_seen_round,
                    source: f.source.clone(),
                    status: f.status.as_str().to_string(),
                    severity: f.severity,
                    file: f.file.clone(),
                    line: f.line,
                    title: f.title.clone(),
                    body: f.body.clone(),
                    confidence: f.confidence,
                    note: f.current_note().map(str::to_string),
                },
            )
        })
        .collect();
    Replay { ledger_rows }
}

/// The corpus is not empty and every case is exercised.
#[test]
fn every_ledger_case_is_replayed() {
    let cases = ledger_cases();
    assert!(cases.len() >= 13, "corpus shrank: {} cases", cases.len());
    for case in cases {
        let _ = replay(&case);
    }
}

/// The decisions must match row for row: same findings, same status, same effective severity,
/// same news round, same last-seen round, same adopted source.
#[test]
fn final_ledgers_match_the_harness() {
    for case in ledger_cases() {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let replayed = replay(&case);
        let expected = committed_rows(&case);

        assert_eq!(
            replayed.ledger_rows.len(),
            expected.len(),
            "{name}: finding count differs"
        );
        for row in expected {
            let got = replayed
                .ledger_rows
                .get(&row.fp)
                .unwrap_or_else(|| panic!("{name}: {} missing from the projection", row.fp));
            assert_eq!(got.status, row.status, "{name}/{}: status", row.fp);
            assert_eq!(got.severity, row.severity, "{name}/{}: severity", row.fp);
            assert_eq!(got.round, row.round, "{name}/{}: news round", row.fp);
            assert_eq!(
                got.last_seen_round, row.last_seen_round,
                "{name}/{}: last_seen_round",
                row.fp
            );
            assert_eq!(got.source, row.source, "{name}/{}: source", row.fp);
            assert_eq!(got.file, row.file, "{name}/{}: file", row.fp);
            assert_eq!(got.title, row.title, "{name}/{}: title", row.fp);
            assert_eq!(got.body, row.body, "{name}/{}: adopted body", row.fp);
            assert_eq!(got.line, row.line, "{name}/{}: line", row.fp);
        }
    }
}

/// Fingerprints are computed, not copied: every committed row's `fp` must fall out of the
/// projection's own hashing of that row's file and title.
#[test]
fn fingerprints_are_reproduced_from_content() {
    for case in ledger_cases() {
        for row in committed_rows(&case) {
            assert_eq!(
                legacy_fingerprint(&row.file, &row.title),
                row.fp,
                "{}: fingerprint mismatch",
                case.display()
            );
        }
    }
}

/// What the harness lost, the kernel keeps. `reopen-after-fix` is the case where the resolution
/// note is overwritten by the reopen note: the committed row holds one string, and the
/// projection still holds both, in order.
#[test]
fn history_the_shell_ledger_overwrote_survives() {
    let case = corpus().join("reopen-after-fix");
    let dir = tempfile::tempdir().unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut ingest = Ingest::new(&mut store, &cas, "replay").unwrap();

    let r1: LegacyStageOutput =
        serde_json::from_str(&std::fs::read_to_string(case.join("input/r1.json")).unwrap())
            .unwrap();
    let r2: LegacyStageOutput =
        serde_json::from_str(&std::fs::read_to_string(case.join("input/r2.json")).unwrap())
            .unwrap();

    ingest.add_stage_output("deep-r1", &r1).unwrap();
    let key = ingest.ledger().findings()[0].key.clone();
    ingest
        .resolve(
            &key,
            Status::Fixed,
            Some("fixed by clamping the range in commit abc1234"),
        )
        .unwrap();
    ingest.advance().unwrap();
    ingest.add_stage_output("deep-r2", &r2).unwrap();

    let finding = ingest.ledger().get(&key).unwrap();
    let notes: Vec<&str> = finding
        .history
        .iter()
        .filter_map(|t| t.note.as_deref())
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("abc1234")),
        "the resolution note must survive the reopen: {notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.starts_with("reopened:")),
        "the reopen must be recorded too: {notes:?}"
    );

    // And the committed row proves the harness kept only the later one.
    let committed = committed_rows(&case);
    let note = committed[0].note.clone().unwrap_or_default();
    assert!(note.starts_with("reopened:"), "fixture changed: {note}");
    assert!(!note.contains("abc1234"), "fixture changed: {note}");
}

/// Both reports of a same-round duplicate are retained, where the harness kept a counter.
#[test]
fn a_duplicate_keeps_both_reports() {
    let case = corpus().join("duplicate-same-round");
    let dir = tempfile::tempdir().unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut ingest = Ingest::new(&mut store, &cas, "replay").unwrap();

    for (source, file) in [
        ("deep-r1", "input/r1-deep.json"),
        ("cross-r1", "input/r1-cross.json"),
    ] {
        let stage: LegacyStageOutput =
            serde_json::from_str(&std::fs::read_to_string(case.join(file)).unwrap()).unwrap();
        ingest.add_stage_output(source, &stage).unwrap();
    }

    let findings = ingest.ledger().findings();
    assert_eq!(findings.len(), 1, "still one finding");
    let finding = findings[0];
    assert_eq!(finding.reports.len(), 2, "both reports are attached");
    assert_eq!(finding.corroborating_sources(), vec!["deep-r1", "cross-r1"]);
    assert_ne!(
        finding.reports[0].report_id, finding.reports[1].report_id,
        "different evidence must be distinct artifacts"
    );
}
