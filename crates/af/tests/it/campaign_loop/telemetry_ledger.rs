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
/// second Round disputes the Finding. Returns the telemetry directory's ledger and the Store's
/// logs.
fn two_recorded_rounds(dir: &Path, extra: &[(&str, &str)]) -> (Value, Logs) {
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
    (review_ledger(&telemetry), store_logs(&state))
}

/// §16, P4: on the Campaign fixture `af telemetry ledger` prints every committed event of every
/// Store log in sequence and every admitted Finding and Demand with its later disposition, and
/// says `complete`; with the mirror failing by injection it says `incomplete`, and the Store is
/// what it would have been.
#[test]
fn telemetry_ledger_covers_the_campaign_and_fails_open() {
    let dir = tempfile::tempdir().unwrap();
    let (ledger, logs) = two_recorded_rounds(dir.path(), &[]);
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
    let (broken, failing_logs) = two_recorded_rounds(
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
    let shape = |logs: &Logs| {
        logs.values()
            .map(|events| {
                events
                    .iter()
                    .map(|(sequence, kind, _)| (*sequence, kind.clone()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shape(&failing_logs),
        shape(&logs),
        "the Store is unaffected"
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
