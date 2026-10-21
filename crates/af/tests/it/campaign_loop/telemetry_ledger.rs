//! Design §7.5 and §16 for P4, through the real binary: `af review run` records its Campaign
//! with Telemetry at `spans`, and `af telemetry ledger` answers from the telemetry directory —
//! every committed event of every Store log in sequence, every admitted Finding and Demand with
//! its later disposition, `complete` — while a mirror failing by injection leaves the run
//! `incomplete` and the Store as it would have been.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

/// A Store's logs: each log's events as `(sequence, type, id)`, by run id.
type Logs = BTreeMap<String, Vec<(u64, String, String)>>;

/// `af review …` with Telemetry recording into `telemetry` at `spans`, `--heavy` for `run`, and
/// `extra` in the environment.
fn review(
    repo: &Path,
    home: &Path,
    telemetry: &Path,
    extra: &[(&str, &str)],
    args: &[&str],
) -> (i32, String, String) {
    let mut command = crate::common::af();
    command
        .arg("review")
        .current_dir(repo)
        .env("HOME", home)
        .env("USER", "loop-test")
        .env("AF_TELEMETRY__LEVEL", "spans")
        .env("AF_TELEMETRY__DIR", telemetry);
    for (name, value) in extra {
        command.env(name, value);
    }
    let mut actual = args.to_vec();
    if actual.first() == Some(&"run") {
        actual.splice(
            1..1,
            [
                "--pipeline",
                super::PIPELINE,
                "--policy-rev",
                "HEAD",
                "--heavy",
            ],
        );
    }
    let out = command.args(actual).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `af telemetry … --dir telemetry --json`.
fn telemetry_json(telemetry: &Path, args: &[&str]) -> Value {
    let out = crate::common::af()
        .arg("telemetry")
        .args(args)
        .arg("--dir")
        .arg(telemetry)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The ledger of the one review run in `telemetry`, valid against its schema.
fn review_ledger(telemetry: &Path) -> Value {
    let runs = telemetry_json(telemetry, &["runs"]);
    let runs: Vec<&Value> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| run["kind"] == "review")
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let run = runs[0]["run_id"].as_str().unwrap().to_string();
    let ledger = telemetry_json(telemetry, &["ledger", &run]);
    crate::schemas::valid(
        &crate::schemas::validator("telemetry-ledger-v1.json"),
        &ledger,
    );
    ledger
}

/// Every log of the Store at `state`: its events as `(sequence, type, id)`.
fn store_logs(state: &str) -> Logs {
    let store =
        review_store::EventStore::open_read_only(Path::new(state).join("events.sqlite")).unwrap();
    store
        .run_ids()
        .unwrap()
        .into_iter()
        .map(|run| {
            let events = store
                .replay(&run)
                .unwrap()
                .into_iter()
                .map(|event| {
                    (
                        event.sequence,
                        event.event_type.as_str().to_string(),
                        event.event_id,
                    )
                })
                .collect();
            (run, events)
        })
        .collect()
}

/// The Task log's transitions a clock decides how often: the lease renewals and the execution
/// records the running Attempt writes while it waits, whose count depends on load.
const TIME_DRIVEN: [&str; 2] = ["lease_renewed", "execution_recorded"];

/// One Store log reduced to what a run decides, not its clock: a Campaign log whole, as
/// `(sequence, type)`; a Task log's events as `(type, transition kind)` in log order without the
/// time-driven transitions, beside the set of every `(type, kind)` the log holds, those included.
#[derive(Debug, PartialEq, Eq)]
enum Stable {
    Campaign(Vec<(u64, String)>),
    Task {
        ordered: Vec<(String, String)>,
        kinds: std::collections::BTreeSet<(String, String)>,
    },
}

/// Every log of the Store at `state` in run-id order, as `Stable`.
fn stable_logs(state: &str) -> Vec<Stable> {
    let store =
        review_store::EventStore::open_read_only(Path::new(state).join("events.sqlite")).unwrap();
    store
        .run_ids()
        .unwrap()
        .into_iter()
        .map(|run| {
            let events = store.replay(&run).unwrap();
            let task = events
                .iter()
                .any(|event| event.event_type.as_str() == "TaskTransition@5");
            if !task {
                return Stable::Campaign(
                    events
                        .iter()
                        .map(|event| (event.sequence, event.event_type.as_str().to_string()))
                        .collect(),
                );
            }
            let typed: Vec<(String, String)> = events
                .iter()
                .map(|event| {
                    let kind = event.payload["change"]["kind"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    (event.event_type.as_str().to_string(), kind)
                })
                .collect();
            Stable::Task {
                ordered: typed
                    .iter()
                    .filter(|(_, kind)| !TIME_DRIVEN.contains(&kind.as_str()))
                    .cloned()
                    .collect(),
                kinds: typed.into_iter().collect(),
            }
        })
        .collect()
}

/// The ledger's mirrored events of every log, in sequence.
fn mirrored(ledger: &Value) -> Logs {
    let mut logs: Logs = BTreeMap::new();
    for entry in ledger["entries"].as_array().unwrap() {
        if entry["entry"] != "event" {
            continue;
        }
        logs.entry(entry["log"].as_str().unwrap().to_string())
            .or_default()
            .push((
                entry["sequence"].as_u64().unwrap(),
                entry["name"].as_str().unwrap().to_string(),
                entry["event_id"].as_str().unwrap().to_string(),
            ));
    }
    for events in logs.values_mut() {
        events.sort();
    }
    logs
}

/// The fixture's reviewer, changed to report a Finding and a Demand in the first Round and, once
/// a prior Finding is assigned to it, to dispute every one while keeping the Demand.
fn dispute_in_the_second_round(repo: &Path, home: &Path) {
    let pipeline = std::fs::read_to_string(super::pipeline_path(repo)).unwrap();
    let start = pipeline.find("'''").unwrap() + 3;
    let end = start + pipeline[start..].find("'''").unwrap();
    let demand = r#"{\"claim\":\"the loop terminates\",\"why\":\"termination is not demonstrated\",\"suggested_method\":\"run a bounded integration test\"}"#;
    let finding = r#"{\"severity\":\"major\",\"file\":\"src/main.rs\",\"line\":1,\"title\":\"Unbounded loop\",\"body\":\"spins\",\"fix\":\"bound it\",\"confidence\":0.9,\"rule_id\":\"test.rules/loop-safety@1\",\"occurrence_key\":\"main-loop\"}"#;
    let script = format!(
        "input=$(cat); \
         ids=$(printf '%s' \"$input\" | grep -o '\"finding_id\":\"sha256:[0-9a-f]*\"' | cut -d'\"' -f4 | sort -u); \
         d=; for id in $ids; do \
         d=\"$d${{d:+,}}{{\\\"finding_id\\\":\\\"$id\\\",\\\"position\\\":\\\"dispute\\\",\\\"reason\\\":\\\"the caller bounds the loop\\\"}}\"; \
         done; \
         if [ -z \"$ids\" ]; then \
         printf '%s' \"{{\\\"findings\\\":[{finding}],\\\"benchmark_demands\\\":[{demand}],\\\"dispositions\\\":[]}}\"; \
         else \
         printf '%s' \"{{\\\"findings\\\":[],\\\"benchmark_demands\\\":[{demand}],\\\"dispositions\\\":[$d]}}\"; \
         fi"
    );
    let changed = format!("{}{script}{}", &pipeline[..start], &pipeline[end..]);
    super::set_pipeline(repo, &changed);
    super::git(repo, home, &["add", "-A"]);
    super::git(
        repo,
        home,
        &["commit", "-qm", "dispute in the second Round"],
    );
}

/// Two Rounds of one Campaign, both recorded: the Round's Finding and Demand are admitted, the
/// second Round disputes the Finding. Returns the telemetry directory's ledger, the Store's logs
/// and their deterministic shape.
fn two_recorded_rounds(dir: &Path, extra: &[(&str, &str)]) -> (Value, Logs, Vec<Stable>) {
    let (repo, home, state) = super::fixture(dir);
    dispute_in_the_second_round(&repo, &home);
    let telemetry = dir.join("telemetry");
    for round in 1..=2 {
        let (code, stdout, stderr) = review(
            &repo,
            &home,
            &telemetry,
            extra,
            &["run", "--campaign", "telemetry-ledger", "--state", &state],
        );
        assert_eq!(code, 3, "Round {round}: {stdout}\n{stderr}");
        assert!(
            !stderr.contains("telemetry problem") || !extra.is_empty(),
            "{stderr}"
        );
    }
    (
        review_ledger(&telemetry),
        store_logs(&state),
        stable_logs(&state),
    )
}

/// §16, P4: on the Campaign fixture `af telemetry ledger` prints every committed event of every
/// Store log in sequence and every admitted Finding and Demand with its later disposition, and
/// says `complete`; with the mirror failing by injection it says `incomplete`, and the Store is
/// what it would have been.
#[test]
fn telemetry_ledger_covers_the_campaign_and_fails_open() {
    let dir = tempfile::tempdir().unwrap();
    let (ledger, logs, stable) = two_recorded_rounds(dir.path(), &[]);
    assert_eq!(
        ledger["coverage"], "complete",
        "{}",
        ledger["coverage_reasons"]
    );
    assert_eq!(ledger["run"]["history"], "prefix:2");
    assert_eq!(
        mirrored(&ledger),
        logs,
        "every committed event, in sequence"
    );
    let findings = ledger["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0]["title"], "Unbounded loop");
    assert_eq!(findings[0]["status"], "contested");
    assert_eq!(
        findings[0]["dispositions"],
        serde_json::json!([{"status":"contested","previous_status":"open",
            "sequence": findings[0]["dispositions"][0]["sequence"]}])
    );
    let demands = ledger["demands"].as_array().unwrap();
    assert_eq!(demands.len(), 1, "{demands:?}");
    assert_eq!(demands[0]["status"], "open");
    assert_eq!(demands[0]["lifecycle"][0]["event"], "opened");

    // The same Campaign with every mirror write failing: the run says so, the command and the
    // Store do not change.
    let failing = tempfile::tempdir().unwrap();
    let (broken, _, failing_stable) = two_recorded_rounds(
        failing.path(),
        &[(review_pipeline::telemetry::FAIL_SETTING, "1")],
    );
    assert_eq!(broken["coverage"], "incomplete");
    let reasons = broken["coverage_reasons"].to_string();
    assert!(
        reasons.contains("af.ledger.coverage = incomplete"),
        "{reasons}"
    );
    assert!(reasons.contains("Store mirror problem"), "{reasons}");
    assert_eq!(
        failing_stable, stable,
        "the Store is unaffected: every deterministic event in order, every kind present"
    );
}

/// ADR-0007 through the binary: a standalone Demand, satisfied by the operator between two
/// recorded Rounds (`af review evidence add` and `satisfy`, which record no Telemetry), is
/// summarized keyed by its Demand id — opened, then evidence satisfied with the Evidence id,
/// summarized by the next recorded Round at the satisfaction's own sequence — and never by a
/// Finding key.
#[test]
fn telemetry_demands_keyed_by_demand() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = super::fixture(dir.path());
    std::fs::write(repo.join("DEMAND"), b"required\n").unwrap();
    super::git(&repo, &home, &["add", "DEMAND"]);
    super::git(&repo, &home, &["commit", "-qm", "request evidence"]);
    let telemetry = dir.path().join("telemetry");
    let run = |args: &[&str]| review(&repo, &home, &telemetry, &[], args);
    let (code, stdout, stderr) = run(&[
        "run",
        "--campaign",
        "telemetry-demand",
        "--state",
        &state,
        "--json",
    ]);
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let outcome: Value = serde_json::from_str(stdout.trim()).unwrap();
    let demand_id = outcome["totals"]["open_or_stale_demand_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    let evidence = dir.path().join("measurement.txt");
    std::fs::write(&evidence, b"bounded integration test passed\n").unwrap();
    let (code, stdout, stderr) = run(&[
        "evidence",
        "add",
        "--campaign",
        "telemetry-demand",
        "--state",
        &state,
        &demand_id,
        evidence.to_str().unwrap(),
        "--actor",
        "operator",
    ]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let evidence_id = stdout.split_whitespace().nth(1).unwrap().to_string();
    let (code, stdout, stderr) = run(&[
        "evidence",
        "satisfy",
        "--campaign",
        "telemetry-demand",
        "--state",
        &state,
        &demand_id,
        &evidence_id,
        "--policy",
        "bench-policy@1",
        "--reason",
        "measured",
        "--actor",
        "operator",
    ]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let (code, stdout, stderr) = run(&["run", "--campaign", "telemetry-demand", "--state", &state]);
    assert!(code == 0 || code == 3, "{stdout}\n{stderr}");
    let ledger = review_ledger(&telemetry);
    assert!(
        ledger["findings"].as_array().unwrap().is_empty(),
        "a standalone Demand: {}",
        ledger["findings"]
    );
    let demands = ledger["demands"].as_array().unwrap();
    assert_eq!(demands.len(), 1, "{demands:?}");
    assert_eq!(demands[0]["demand_id"], demand_id.as_str());
    assert_eq!(
        (&demands[0]["kind"], &demands[0]["required"]),
        (&Value::from("required"), &Value::from(true))
    );
    let lifecycle: Vec<(&str, Option<&str>)> = demands[0]["lifecycle"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["event"].as_str().unwrap(),
                step["evidence_id"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        lifecycle,
        [
            ("opened", None),
            ("evidence_satisfied", Some(evidence_id.as_str()))
        ]
    );
    // The satisfaction's sequence is the committed `EvidenceSatisfied@1`'s own.
    let logs = store_logs(&state);
    let satisfied = logs
        .values()
        .flatten()
        .find(|(_, kind, _)| kind == "EvidenceSatisfied@1")
        .unwrap()
        .0;
    assert_eq!(demands[0]["lifecycle"][1]["sequence"], satisfied);
    for entry in ledger["entries"].as_array().unwrap() {
        let demand_entry = entry["entry"] == "demand"
            || entry["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("af.demand."));
        if demand_entry {
            assert_eq!(entry["demand_id"], demand_id.as_str(), "{entry}");
            assert!(entry.get("finding_key").is_none(), "{entry}");
        }
    }
}

/// Design §7.5 (P4): a Finding summary is written for every admitted Report, not only when the
/// Finding's status changes. Round 1 reports the loop (`major`); Round 2, with a committed
/// `BLOCKER`, reports the same occurrence escalated (`blocker`, still `open`); Round 3, on a
/// later commit, reports it again unchanged (a duplicate, a new Report artifact). Each
/// admission is a summary at its `FindingReported@1`'s sequence, and `af telemetry ledger`
/// shows the latest: the adopted severity and the last Report's artifact.
#[test]
fn telemetry_finding_summary_for_every_admitted_report() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = super::fixture(dir.path());
    let telemetry = dir.path().join("telemetry");
    let run = |round: u32| {
        let (code, stdout, stderr) = review(
            &repo,
            &home,
            &telemetry,
            &[],
            &[
                "run",
                "--campaign",
                "telemetry-escalation",
                "--state",
                &state,
            ],
        );
        assert!(
            code == 3 || (round == 3 && code != 0 && code != 1 && code != 2),
            "Round {round}: {code}\n{stdout}\n{stderr}"
        );
        assert!(!stderr.contains("telemetry problem"), "{stderr}");
    };
    run(1);
    std::fs::write(repo.join("BLOCKER"), "escalate the stable occurrence\n").unwrap();
    super::git(&repo, &home, &["add", "BLOCKER"]);
    super::git(&repo, &home, &["commit", "-qm", "escalate"]);
    run(2);
    std::fs::write(repo.join("NOTE"), "the same report again\n").unwrap();
    super::git(&repo, &home, &["add", "NOTE"]);
    super::git(&repo, &home, &["commit", "-qm", "again"]);
    run(3);
    let ledger = review_ledger(&telemetry);
    let findings = ledger["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    let finding = &findings[0];
    assert_eq!(finding["status"], "open", "{finding}");
    assert_eq!(
        finding["severity"], "blocker",
        "the adopted severity: {finding}"
    );
    assert_eq!(finding["effective_severity"], "blocker", "{finding}");
    assert_eq!(finding["round"], 3, "{finding}");
    let key = finding["key"].as_str().unwrap();
    // One summary per admitted Report, each at its admission's sequence.
    let reported: Vec<u64> = ledger["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["entry"] == "event" && entry["name"] == "FindingReported@1")
        .map(|entry| entry["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(reported.len(), 3, "three Reports admitted: {reported:?}");
    let summarized: Vec<u64> = ledger["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["entry"] == "finding" && entry["finding_key"] == key)
        .map(|entry| entry["sequence"].as_u64().unwrap())
        .collect();
    for sequence in &reported {
        assert!(summarized.contains(sequence), "{sequence}: {summarized:?}");
    }
    // The latest summary names the last Report's artifact.
    let logs = store_logs(&state);
    let last_report = logs
        .values()
        .flatten()
        .filter(|(_, kind, _)| kind == "FindingReported@1")
        .map(|(sequence, _, _)| *sequence)
        .max()
        .unwrap();
    assert_eq!(*reported.last().unwrap(), last_report);
    let store =
        review_store::EventStore::open_read_only(Path::new(&state).join("events.sqlite")).unwrap();
    let artifact = store
        .replay("campaign-telemetry-escalation")
        .unwrap()
        .into_iter()
        .find(|event| event.sequence == last_report)
        .unwrap()
        .artifact_refs[0]
        .clone();
    assert_eq!(
        finding["report_artifact_id"],
        artifact.as_str(),
        "{finding}"
    );
}

/// Design §7.5 and ADR-0007 (P4): a Demand's summary pairs the Demand's own Subject with the
/// Snapshot bound to that Subject, in every later status summary. Round 1 opens a Demand; the
/// operator adds Evidence and satisfies it; a new head makes Round 2 review another Subject,
/// so the Demand is `stale` — its stale summary still names Round 1's Subject and Snapshot,
/// never the new head's — after the lifecycle steps opened, evidence satisfied and stale.
#[test]
fn telemetry_stale_demand_names_its_own_subject() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = super::fixture(dir.path());
    std::fs::write(repo.join("DEMAND"), b"required\n").unwrap();
    super::git(&repo, &home, &["add", "DEMAND"]);
    super::git(&repo, &home, &["commit", "-qm", "request evidence"]);
    let telemetry = dir.path().join("telemetry");
    let campaign = "telemetry-stale";
    let run = |args: &[&str]| review(&repo, &home, &telemetry, &[], args);
    let (code, stdout, stderr) = run(&["run", "--campaign", campaign, "--state", &state, "--json"]);
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let outcome: Value = serde_json::from_str(stdout.trim()).unwrap();
    let demand_id = outcome["totals"]["open_or_stale_demand_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    let evidence = dir.path().join("measurement.txt");
    std::fs::write(&evidence, b"bounded integration test passed\n").unwrap();
    let (code, stdout, stderr) = run(&[
        "evidence",
        "add",
        "--campaign",
        campaign,
        "--state",
        &state,
        &demand_id,
        evidence.to_str().unwrap(),
        "--actor",
        "operator",
    ]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let evidence_id = stdout.split_whitespace().nth(1).unwrap().to_string();
    let (code, stdout, stderr) = run(&[
        "evidence",
        "satisfy",
        "--campaign",
        campaign,
        "--state",
        &state,
        &demand_id,
        &evidence_id,
        "--policy",
        "bench-policy@1",
        "--reason",
        "measured",
        "--actor",
        "operator",
    ]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    // The Demand's own Subject and the Snapshot bound to it.
    let log = format!("campaign-{campaign}");
    let cas = review_store::Cas::open(Path::new(&state).join("cas")).unwrap();
    let subject_of = |round: u64| {
        let store =
            review_store::EventStore::open_read_only(Path::new(&state).join("events.sqlite"))
                .unwrap();
        let subject = store
            .replay(&log)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == review_core::EventType::RoundStartedV1)
            .find(|event| event.payload["round"] == round)
            .unwrap()
            .payload["subject_id"]
            .as_str()
            .unwrap()
            .to_string();
        let snapshot =
            serde_json::from_value::<review_core::SubjectV1>(cas.get_json(&subject).unwrap())
                .unwrap()
                .head_snapshot_id;
        (subject, snapshot)
    };
    let (own_subject, own_snapshot) = subject_of(1);
    // A new head: Round 2 reviews another Subject and the satisfaction is stale.
    std::fs::write(repo.join("NOTE"), b"a new head\n").unwrap();
    super::git(&repo, &home, &["add", "NOTE"]);
    super::git(&repo, &home, &["commit", "-qm", "new head"]);
    let (code, stdout, stderr) = run(&["run", "--campaign", campaign, "--state", &state]);
    assert!(code == 0 || code == 3, "{stdout}\n{stderr}");
    let (new_subject, new_snapshot) = subject_of(2);
    assert_ne!(new_subject, own_subject);
    assert_ne!(new_snapshot, own_snapshot);
    let ledger = review_ledger(&telemetry);
    let demand = ledger["demands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|demand| demand["demand_id"] == demand_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(demand["status"], "stale", "{demand}");
    assert_eq!(demand["subject_id"], own_subject.as_str(), "{demand}");
    assert_eq!(demand["snapshot_id"], own_snapshot.as_str(), "{demand}");
    let lifecycle: Vec<(&str, Option<&str>)> = demand["lifecycle"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["event"].as_str().unwrap(),
                step["evidence_id"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        lifecycle,
        [
            ("opened", None),
            ("evidence_satisfied", Some(evidence_id.as_str())),
            ("stale", None),
        ],
        "{demand}"
    );
}

/// Design §7.5 and ADR-0007 (P4): an admitted Evidence reuse (`EvidenceReuseAdmitted@1`) that
/// takes a stale Demand back to `satisfied` is a satisfaction lifecycle step,
/// `af.demand.evidence_satisfied` with the reused satisfaction's Evidence id. Round 1 opens a
/// Demand that the operator satisfies through the binary; a mirror recording the Campaign's
/// log is then told of one committed transaction in which Round 2 starts on another Subject
/// (the Demand is stale) and the reuse of Round 1's satisfaction is admitted (satisfied
/// again). The Store admits a reuse only while its satisfaction's Subject is active, so that
/// transaction is handed to the mirror directly: the mirror's rule is the Ledger projection's
/// transition, wherever it occurs.
#[test]
fn telemetry_demand_reuse_takes_stale_back_to_satisfied() {
    use review_store::store::observer::{Commit, CommitObserver as _};
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, state) = super::fixture(dir.path());
    std::fs::write(repo.join("DEMAND"), b"required\n").unwrap();
    super::git(&repo, &home, &["add", "DEMAND"]);
    super::git(&repo, &home, &["commit", "-qm", "request evidence"]);
    let campaign = "telemetry-reuse";
    let run = |args: &[&str]| review(&repo, &home, &dir.path().join("review"), &[], args);
    let (code, stdout, stderr) = run(&["run", "--campaign", campaign, "--state", &state, "--json"]);
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let outcome: Value = serde_json::from_str(stdout.trim()).unwrap();
    let demand_id = outcome["totals"]["open_or_stale_demand_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    let evidence = dir.path().join("measurement.txt");
    std::fs::write(&evidence, b"bounded integration test passed\n").unwrap();
    let (code, stdout, stderr) = run(&[
        "evidence",
        "add",
        "--campaign",
        campaign,
        "--state",
        &state,
        &demand_id,
        evidence.to_str().unwrap(),
        "--actor",
        "operator",
    ]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let evidence_id = stdout.split_whitespace().nth(1).unwrap().to_string();
    let (code, stdout, stderr) = run(&[
        "evidence",
        "satisfy",
        "--campaign",
        campaign,
        "--state",
        &state,
        &demand_id,
        &evidence_id,
        "--policy",
        "bench-policy@1",
        "--reason",
        "measured",
        "--actor",
        "operator",
    ]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let log = format!("campaign-{campaign}");
    let store_dir = Path::new(&state);
    let cas = review_store::Cas::open(store_dir.join("cas")).unwrap();
    let store = review_store::EventStore::open(store_dir.join("events.sqlite")).unwrap();
    let events = store.replay(&log).unwrap();
    let ledger = review_store::LedgerProjection::rebuild(&store, &cas, &log)
        .unwrap()
        .into_ledger();
    let satisfaction = ledger.demand(&demand_id).unwrap().satisfactions[0].clone();
    assert_eq!(satisfaction.satisfaction.evidence_id, evidence_id);
    let started = events
        .iter()
        .find(|event| event.event_type == review_core::EventType::RoundStartedV1)
        .unwrap()
        .clone();
    let own_subject = started.payload["subject_id"].as_str().unwrap().to_string();
    let own_head = ledger.active_head_snapshot_id().unwrap().to_string();
    let next = events.len() as u64;
    // Round 2 on another Subject, then the reuse of Round 1's satisfaction, in one commit.
    let head = cas.put(b"another head snapshot").unwrap();
    let subject = cas
        .put_json(&serde_json::to_value(review_core::SubjectV1::whole_tree(&head)).unwrap())
        .unwrap();
    let mut payload = started.payload.clone();
    payload["round"] = 2.into();
    payload["subject_id"] = subject.clone().into();
    let second_round = review_core::RunEvent {
        event_id: "01jd8m4qz9k7v3n2p6r8t0w2r2".into(),
        sequence: next,
        payload,
        causation_id: Some(started.event_id.clone()),
        artifact_refs: started
            .artifact_refs
            .iter()
            .map(|id| {
                if *id == own_subject {
                    subject.clone()
                } else if *id == own_head {
                    head.clone()
                } else {
                    id.clone()
                }
            })
            .collect(),
        ..started.clone()
    };
    let admission = review_core::EvidenceReuseAdmissionV1 {
        demand_id: demand_id.clone(),
        satisfaction_id: satisfaction.artifact_id.clone(),
        subject_id: satisfaction.satisfaction.subject_id.clone(),
        actor: "operator".into(),
        policy_revision: "bench-policy@1".into(),
        reason: "the measurement covers the new head".into(),
    };
    let (record, _) = cas
        .put_artifact(
            review_core::contract::EVIDENCE_REUSE_ADMISSION_V1,
            review_core::Producer::KernelOperation {
                run_id: log.clone(),
                node_id: None,
                operation_id: "reuse".into(),
            },
            vec![satisfaction.record_id.clone()],
            Some(head.clone()),
            serde_json::to_value(&admission).unwrap(),
        )
        .unwrap();
    let reuse = review_core::RunEvent {
        event_id: "01jd8m4qz9k7v3n2p6r8t0w2r3".into(),
        sequence: next + 1,
        event_type: review_core::EventType::EvidenceReuseAdmittedV1,
        payload: serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
            artifact_id: record.clone(),
        })
        .unwrap(),
        causation_id: None,
        correlation_id: Some(demand_id.clone()),
        artifact_refs: vec![record],
        node_id: None,
        attempt_id: None,
        ..started.clone()
    };
    let telemetry = dir.path().join("telemetry");
    let recording = crate::telemetry_ledger::Recording::start(
        &telemetry,
        review_telemetry::level::Level::Spans,
        store_dir,
        &log,
        1,
    );
    let mirror = recording.mirror();
    recording.within(|| {
        mirror.committed(&Commit {
            store: &store,
            cas: &cas,
            events: &[second_round, reuse],
            task: None,
        })
    });
    drop(store);
    recording.finish();
    let ledger = crate::telemetry_ledger::read_ledger(&telemetry, &log);
    let demand = ledger
        .demands
        .iter()
        .find(|demand| demand.demand_id == demand_id)
        .unwrap();
    assert_eq!(demand.status, "satisfied", "{demand:?}");
    let at = |kind: review_core::EventType| {
        events
            .iter()
            .find(|event| event.event_type == kind)
            .unwrap()
            .sequence
    };
    let lifecycle: Vec<(&str, Option<&str>, Option<u64>)> = demand
        .lifecycle
        .iter()
        .map(|step| {
            (
                step.event.as_str(),
                step.evidence_id.as_deref(),
                step.sequence,
            )
        })
        .collect();
    assert_eq!(
        lifecycle,
        [
            (
                "opened",
                None,
                Some(at(review_core::EventType::DemandRecordedV1))
            ),
            (
                "evidence_satisfied",
                Some(evidence_id.as_str()),
                Some(at(review_core::EventType::EvidenceSatisfiedV1))
            ),
            ("stale", None, Some(next)),
            (
                "evidence_satisfied",
                Some(evidence_id.as_str()),
                Some(next + 1)
            ),
        ]
    );
    // Every summary names the Demand's own Subject (Round 1's), never Round 2's.
    assert_eq!(demand.subject_id, own_subject);
    assert_eq!(demand.snapshot_id, own_head);
}
