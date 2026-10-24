//! A Task's gate (ADR-0196): the check report af records beside the receipt, the host's load and
//! CPUs, the steps by stage, the flaky tests a gate reran alone, and `af task run ID
//! --retry-gate`, which runs a failed gate again on the same sealed candidate.

use std::path::{Path, PathBuf};
use std::process::Command;

use review_store::Cas;
use serde_json::{Value, json};

use crate::task_cli;

/// A Task-runtime fixture whose check is `gate.py`, a gate that writes its own account to
/// `$AF_CHECK_REPORT`. While `<root>/fail-preflight` exists it stops at a preflight step with
/// its fix and exits 1; otherwise it runs the real pagination check and reports one test that
/// failed in its parallel run and passed alone. With `evaluator_passes` false the evaluator,
/// which runs only after a passing gate, rejects the candidate.
struct Gate {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    ticket: PathBuf,
}

const GATE: &str = r#"import json, os, sys
report = os.environ.get('AF_CHECK_REPORT')
def line(record):
    record['schema'] = 'af.check-report/1'
    if report:
        with open(report, 'a') as stream:
            stream.write(json.dumps(record) + '\n')
def step(label, stage, code, fix=None):
    record = {'record': 'step', 'label': label, 'stage': stage, 'exit_code': code,
              'command': ['python3', 'scripts/%s.py' % label]}
    if fix:
        record['fix'] = fix
    line(record)
if os.path.exists(FLAG):
    step('performance-kernel-harness', 'preflight', 1,
         'python3 -I scripts/performance/cli.py harness-digest --write')
    sys.exit(1)
step('performance-kernel-harness', 'preflight', 0)
import pagination
assert pagination.paginate(list(range(7)), 2, 3) == [2, 3, 4]
line({'record': 'tests', 'test_threads': 7, 'failed_count': 0, 'failed': [],
      'flaky': [{'name': 'af::it task_heavy::loaded', 'parallel': 'failed',
                 'isolated': 'passed'}], 'rerun': 'isolated'})
step('test', 'tests', 0)
"#;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Re-pin every package of the fixture's catalog after an edit.
fn repin_all(repo: &Path) {
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        pin["digest"] = toml::Value::String(
            review_config::lock::package_digest(name, &repo.join(pin["path"].as_str().unwrap()))
                .unwrap(),
        );
    }
    std::fs::write(&path, toml::to_string(&catalog).unwrap()).unwrap();
}

/// The checked-in Task-runtime `fixture` whose `pagination` check is [`GATE`]; see [`Gate`].
fn gate(root: &Path, fixture: &str, task_id: &str, evaluator_passes: bool) -> Gate {
    let (repo, state) = task_cli::fixture_named(root, fixture);
    let flag = root.join("fail-preflight");
    std::fs::write(
        repo.join("gate.py"),
        format!("FLAG = {:?}\n{GATE}", flag.display().to_string()),
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/code-policy.toml"),
        "schema = \"af.code-task-policy/1\"\ncheck_wall_ms = 5000\nrequire_container = false\n\n\
         [checks.pagination]\nname = \"pagination\"\nrequired = true\n\n\
         [checks.pagination.command]\nprogram = \"/usr/bin/python3\"\n\n\
         [[checks.pagination.command.args]]\nvalue = \"-B\"\nprovenance = \"literal\"\n\n\
         [[checks.pagination.command.args]]\nvalue = \"gate.py\"\nprovenance = \"literal\"\n",
    )
    .unwrap();
    // A loaded host starts Python slowly: the walls bound a hung fixture, never the test's
    // subject (ADR-0114).
    task_cli::load_safe_walls(&repo);
    // Every Pipeline holds one Attempt more than a first run needs: the retried check's.
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    for pin in catalog["packages"].as_table().unwrap().values() {
        let path = repo
            .join(pin["path"].as_str().unwrap())
            .join("pipeline.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut pipeline: toml::Value = toml::from_str(&text).unwrap();
        let attempts = pipeline["max_attempts"].as_integer().unwrap();
        pipeline["max_attempts"] = toml::Value::Integer(attempts + 1);
        std::fs::write(&path, toml::to_string(&pipeline).unwrap()).unwrap();
    }
    if !evaluator_passes {
        std::fs::write(
            repo.join(".af/task-packages/fixture/evaluator/worker.py"),
            "import json,sys\njson.load(sys.stdin)\n\
             print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':'failed','reason':'The evaluator rejects this candidate'}]}}))\n",
        )
        .unwrap();
    }
    repin_all(&repo);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "gate fixture"]);
    // The fixture's own Task file, with one Attempt more for the retried check (beyond its
    // verification reserve, which the first check already used) and room for every Attempt at
    // the load-safe wall.
    let mut ticket: Value =
        serde_json::from_slice(&std::fs::read(repo.join("ticket.json")).unwrap()).unwrap();
    ticket["task_id"] = json!(task_id);
    let attempts = ticket["limits"]["max_attempts"].as_u64().unwrap();
    ticket["limits"]["max_attempts"] = json!(attempts + 1);
    task_cli::load_safe_limits(&mut ticket["limits"]);
    let path = root.join("ticket.json");
    std::fs::write(&path, serde_json::to_vec(&ticket).unwrap()).unwrap();
    Gate {
        root: root.to_path_buf(),
        repo,
        state,
        ticket: path,
    }
}

impl Gate {
    fn af(&self, args: &[&str]) -> (i32, String, String) {
        let output = crate::common::af()
            .current_dir(&self.repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .args(["--state", self.state.to_str().unwrap()])
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn start(&self) -> (i32, String, String) {
        self.af(&[
            "task",
            "start",
            "--execute",
            "--file",
            self.ticket.to_str().unwrap(),
        ])
    }

    fn show_json(&self, task_id: &str) -> Value {
        let (code, stdout, stderr) = self.af(&["task", "show", task_id, "--json"]);
        assert_eq!(code, 0, "{stderr}\n{stdout}");
        serde_json::from_str(stdout.trim()).unwrap()
    }

    fn fail_preflight(&self, fail: bool) {
        let flag = self.root.join("fail-preflight");
        if fail {
            std::fs::write(flag, b"").unwrap();
        } else {
            std::fs::remove_file(flag).unwrap();
        }
    }

    fn cas(&self) -> Cas {
        Cas::open(self.state.join("cas")).unwrap()
    }
}

/// Every artifact of `kind` in the Store, in no particular order.
fn artifacts(cas: &Cas, root: &Path, kind: &str) -> Vec<Value> {
    let mut found = Vec::new();
    let mut stack = vec![root.join("cas/objects")];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(digest) = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|prefix| prefix.to_str())
                .zip(path.file_name().and_then(|name| name.to_str()))
                .map(|(prefix, rest)| format!("sha256:{prefix}{rest}"))
            else {
                continue;
            };
            if let Ok(Some(artifact)) = cas.get_optional_artifact(&digest)
                && artifact.artifact_type == kind
            {
                found.push(artifact.payload);
            }
        }
    }
    found
}

/// The pagination gate stops at a preflight: its run log names the step and the fix, and the
/// next command. The check report records that stop by stage, the host's load and CPUs; then
/// `--retry-gate` runs the same check again on the same sealed candidate, under the same plan,
/// in one more check Attempt, and the Task continues into its evaluator and is verified. The
/// flaky test the passing gate reran alone is printed on its own line.
#[test]
fn a_failed_gate_is_retried_on_the_same_candidate_and_continues_into_review() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = gate(directory.path(), "pagination", "gate-retry", true);
    fixture.fail_preflight(true);
    let (code, stdout, stderr) = fixture.start();
    assert_eq!(
        code, 5,
        "a failed gate is gate_failed (`af help exit-codes`)\n{stderr}\n{stdout}"
    );
    let (code, shown, stderr) = fixture.af(&["task", "show", "gate-retry"]);
    assert_eq!(code, 0, "{stderr}\n{shown}");
    assert_eq!(
        shown.lines().next(),
        Some("Task gate-retry: finished/gate_failed/check_failed"),
        "{shown}"
    );
    assert_eq!(
        shown
            .lines()
            .filter(|line| line.starts_with("next:"))
            .count(),
        1,
        "one next command: {shown}"
    );
    assert!(
        shown.lines().any(|line| line
            == "check pagination: stopped at preflight step performance-kernel-harness \
                (exit 1); fix: python3 -I scripts/performance/cli.py harness-digest --write"),
        "{shown}"
    );
    assert!(
        shown
            .lines()
            .any(|line| line.starts_with("check pagination host: load ") && line.contains(" CPUs")),
        "{shown}"
    );
    // The next command carries the `--state` the command was given.
    let retry = format!(
        "af task run gate-retry --retry-gate --state {}",
        fixture.state.display()
    );
    assert_eq!(
        shown.lines().last(),
        Some(format!("next: {retry}").as_str()),
        "{shown}"
    );
    let before = fixture.show_json("gate-retry");
    crate::schemas::valid(
        &crate::schemas::validator("task-inspection-v13.json"),
        &before,
    );
    assert_eq!(before["result"]["domain_conclusion"], "changes_requested");
    assert_eq!(before["state"]["next_action"], retry.as_str());
    // The JSON carries the facts the text prints under `gate`: the check report, by stage and
    // with its fix, and the host it ran on.
    let [shown_report] = before["gate"]["check_reports"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("one check report: {before:#}");
    };
    assert_eq!(shown_report["node"], "root.nodes.check");
    let step = &shown_report["record"]["checks"]["pagination"]["steps"][0];
    assert_eq!(step["stage"], "preflight");
    assert_eq!(step["label"], "performance-kernel-harness");
    assert_eq!(
        step["fix"],
        "python3 -I scripts/performance/cli.py harness-digest --write"
    );
    assert!(before["gate"].get("retries").is_none(), "{before:#}");
    let attempts_before = before["attempts"].as_u64().unwrap();
    assert_eq!(attempts_before, 2, "the implementer and the failed check");
    // Present only when an Attempt's usage is unknown, a lease lost under load among them.
    assert!(before.get("unknown_usage_attempts").is_none(), "{before}");

    let cas = fixture.cas();
    let reports = artifacts(&cas, &fixture.state, "af/TaskCheckReport@1");
    let [failed] = reports.as_slice() else {
        panic!("one check report: {reports:?}");
    };
    assert_eq!(
        &shown_report["record"], failed,
        "the recorded report, as recorded"
    );
    let schema = crate::schemas::validator("task-check-report-v1.json");
    crate::schemas::valid(&schema, failed);
    let run = &failed["checks"]["pagination"];
    assert_eq!(run["report"], "admitted");
    assert_eq!(run["steps"][0]["stage"], "preflight");
    assert_eq!(run["steps"][0]["exit_code"], 1);
    for cpus in ["cpus_start", "cpus_end"] {
        assert!(run["host"][cpus].as_u64().unwrap() >= 1, "{cpus}");
    }
    for load in ["load_1m_start", "load_1m_end"] {
        let value = run["host"][load].as_str().unwrap();
        let (whole, fraction) = value.split_once('.').unwrap();
        assert!(
            whole.parse::<u64>().is_ok() && fraction.len() == 2,
            "{load} {value}"
        );
    }

    fixture.fail_preflight(false);
    let (code, stdout, stderr) = fixture.af(&["task", "run", "gate-retry", "--retry-gate"]);
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    assert!(
        stderr.contains("retrying gate root.nodes.check of Task gate-retry on the same candidate"),
        "{stderr}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "gate retried: root.nodes.check ran again on the same candidate"),
        "{stdout}"
    );
    assert!(
        stdout.lines().any(|line| line
            == "check pagination flaky: af::it task_heavy::loaded (failed in the parallel run, \
                passed alone)"),
        "{stdout}"
    );
    assert!(!stdout.contains("--retry-gate"), "{stdout}");
    let after = fixture.show_json("gate-retry");
    crate::schemas::valid(
        &crate::schemas::validator("task-inspection-v13.json"),
        &after,
    );
    assert_eq!(after["result"]["domain_conclusion"], "verified");
    // The retry history and the passing gate's own report, under `gate`.
    let [retry] = after["gate"]["retries"].as_array().unwrap().as_slice() else {
        panic!("one gate retry: {after:#}");
    };
    assert_eq!(retry["schema"], "af/TaskGateRetried@1");
    assert_eq!(retry["gates"][0]["node"], "root.nodes.check");
    assert_eq!(
        after["gate"]["check_reports"][0]["record"]["checks"]["pagination"]["tests"]["flaky"][0]["name"],
        "af::it task_heavy::loaded"
    );
    // `af task report` counts the retry as a run of the Task: the start and the retry.
    let (code, reported, stderr) = fixture.af(&["task", "report", "gate-retry", "--json"]);
    assert_eq!(code, 0, "{stderr}\n{reported}");
    let reported: Value = serde_json::from_str(reported.trim()).unwrap();
    assert_eq!(reported["tasks"][0]["runs"], 2, "{reported:#}");
    assert_eq!(after["result"]["acceptance"], "satisfied");
    assert_eq!(after["plan_id"], before["plan_id"], "no new plan");
    assert_eq!(after["task_id"], "gate-retry", "no new Task");
    assert_eq!(
        after["attempts"].as_u64().unwrap(),
        attempts_before + 2,
        "one more check Attempt, then the evaluator's"
    );
    assert!(after.get("unknown_usage_attempts").is_none(), "{after}");

    let receipts = artifacts(&cas, &fixture.state, "af/TaskCheckReceipt@1");
    assert_eq!(receipts.len(), 2, "{receipts:?}");
    let outcomes: std::collections::BTreeSet<_> = receipts
        .iter()
        .map(|receipt| receipt["outcome"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        outcomes,
        ["failed", "passed"]
            .map(str::to_owned)
            .into_iter()
            .collect()
    );
    assert_eq!(
        receipts[0]["snapshot_id"], receipts[1]["snapshot_id"],
        "the same sealed candidate"
    );
    assert_eq!(receipts[0]["plan_id"], receipts[1]["plan_id"]);
    let reports = artifacts(&cas, &fixture.state, "af/TaskCheckReport@1");
    assert_eq!(reports.len(), 2);
    let passed = reports
        .iter()
        .find(|report| report["checks"]["pagination"]["tests"].is_object())
        .unwrap();
    crate::schemas::valid(&schema, passed);
    let tests = &passed["checks"]["pagination"]["tests"];
    assert_eq!(tests["test_threads"], 7);
    assert_eq!(
        tests["flaky"],
        json!([{"name": "af::it task_heavy::loaded", "parallel": "failed", "isolated": "passed"}])
    );

    // A verified Task has no failed gate left to retry.
    let (code, stdout, stderr) = fixture.af(&["task", "run", "gate-retry", "--retry-gate"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stderr.trim_end().ends_with(
            ": Task gate-retry cannot retry its gate: it was verified, so no gate failed."
        ),
        "{stderr}"
    );
}

/// A Task whose checks passed and whose evaluator rejected the candidate did not end at a gate:
/// `--retry-gate` refuses it in one sentence and records nothing.
#[test]
fn retry_gate_refuses_a_task_its_evaluator_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = gate(directory.path(), "pagination", "evaluator-rejected", false);
    let (code, stdout, stderr) = fixture.start();
    assert_eq!(code, 3, "{stderr}\n{stdout}");
    let (_, shown, _) = fixture.af(&["task", "show", "evaluator-rejected"]);
    assert!(!shown.contains("--retry-gate"), "{shown}");
    let before = fixture.show_json("evaluator-rejected");
    let (code, stdout, stderr) = fixture.af(&["task", "run", "evaluator-rejected", "--retry-gate"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stderr.trim_end().ends_with(
            ": Task evaluator-rejected cannot retry its gate: its checks passed, so its last \
             failure was not a gate; --retry-gate runs only a failed check again."
        ),
        "{stderr}"
    );
    assert_eq!(
        fixture.show_json("evaluator-rejected"),
        before,
        "nothing was recorded"
    );
}

/// The gate of a reviewed implementation runs inside its review child Pipeline, before both
/// reviewers: retried after a failed preflight, the same candidate's check passes and the Task
/// continues into the review it skipped, both reviewers and the evaluator run, and the review
/// Round passes. One Round: the retry runs the gate of the same round again.
#[test]
fn a_retried_gate_continues_into_the_review_it_skipped() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = gate(directory.path(), "embedded-review", "review-retry", true);
    fixture.fail_preflight(true);
    let (code, stdout, stderr) = fixture.start();
    assert_eq!(code, 5, "a failed gate is gate_failed\n{stderr}\n{stdout}");
    let before = fixture.show_json("review-retry");
    assert!(before.get("unknown_usage_attempts").is_none(), "{before}");
    assert_eq!(
        before["attempts"], 2,
        "the implementer and the failed check; no reviewer ran: {before:#}"
    );
    let (_, shown, _) = fixture.af(&["task", "show", "review-retry"]);
    assert_eq!(
        shown.lines().last(),
        Some(
            format!(
                "next: af task run review-retry --retry-gate --state {}",
                fixture.state.display()
            )
            .as_str()
        ),
        "{shown}"
    );

    fixture.fail_preflight(false);
    let (code, stdout, stderr) = fixture.af(&["task", "run", "review-retry", "--retry-gate"]);
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    let after = fixture.show_json("review-retry");
    assert!(after.get("unknown_usage_attempts").is_none(), "{after}");
    assert_eq!(after["result"]["acceptance"], "satisfied", "{after:#}");
    assert_eq!(after["plan_id"], before["plan_id"], "no new plan");
    assert_eq!(
        after["attempts"].as_u64().unwrap(),
        2 + 4,
        "the retried check, both reviewers and the evaluator"
    );
    let rounds = after["review_rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 1, "{after:#}");
    assert_eq!(rounds[0]["outcome"], "passed", "{after:#}");
    let receipts = artifacts(&fixture.cas(), &fixture.state, "af/TaskCheckReceipt@1");
    assert_eq!(receipts.len(), 2, "{receipts:?}");
    assert_eq!(receipts[0]["snapshot_id"], receipts[1]["snapshot_id"]);
}
