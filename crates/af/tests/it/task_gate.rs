//! A Task's gate (ADR-0205): the check report af records beside the receipt, the host's load and
//! CPUs, the steps by stage, the flaky tests a gate reran alone, and `af task run ID
//! --retry-gate`, which runs a failed gate again on the same sealed candidate.

use std::path::{Path, PathBuf};
use std::process::Command;

use review_store::Cas;
use serde_json::{Value, json};

use crate::task_cli;

/// A Task-runtime fixture whose check is `gate.py`, a gate that writes its own account to
/// `$AF_CHECK_REPORT`. While `<root>/hang-check` exists it waits (a minute at most) before it
/// does anything. While `<root>/fail-preflight` exists it stops at a preflight step with
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
import time
waited = 0
while os.path.exists(HANG) and waited < 600:
    time.sleep(0.1)
    waited += 1
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
    gate_with(root, fixture, task_id, evaluator_passes, true)
}

/// [`gate`], with the Task file's spare Attempt for a retried check only when `spare_attempt`.
fn gate_with(
    root: &Path,
    fixture: &str,
    task_id: &str,
    evaluator_passes: bool,
    spare_attempt: bool,
) -> Gate {
    gate_edited(
        root,
        fixture,
        task_id,
        evaluator_passes,
        spare_attempt,
        &|_| {},
    )
}

/// [`gate_with`], with `edit` applied to the fixture's repository before its packages are
/// pinned and committed.
fn gate_edited(
    root: &Path,
    fixture: &str,
    task_id: &str,
    evaluator_passes: bool,
    spare_attempt: bool,
    edit: &dyn Fn(&Path),
) -> Gate {
    let (repo, state) = task_cli::fixture_named(root, fixture);
    let flag = root.join("fail-preflight");
    std::fs::write(
        repo.join("gate.py"),
        format!(
            "FLAG = {:?}\nHANG = {:?}\n{GATE}",
            flag.display().to_string(),
            root.join("hang-check").display().to_string()
        ),
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
    edit(&repo);
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
    ticket["limits"]["max_attempts"] = json!(attempts + u64::from(spare_attempt));
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
        self.af_env(&[], args)
    }

    fn af_env(&self, env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
        let output = crate::common::af()
            .current_dir(&self.repo)
            .envs(env.iter().copied())
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

    /// `command` as af prints it for this fixture: with the `--state` every command here is
    /// given, as typed.
    fn next(&self, command: &str) -> String {
        format!(
            "{command} --state {}",
            shell_words::quote(self.state.to_str().unwrap())
        )
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
        &crate::schemas::validator("task-inspection-v14.json"),
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
        &crate::schemas::validator("task-inspection-v14.json"),
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

    let receipts = artifacts(&cas, &fixture.state, "af/TaskCheckReceipt@2");
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
    let receipts = artifacts(&fixture.cas(), &fixture.state, "af/TaskCheckReceipt@2");
    assert_eq!(receipts.len(), 2, "{receipts:?}");
    assert_eq!(receipts[0]["snapshot_id"], receipts[1]["snapshot_id"]);
}

/// Every rendering's next command for `task_id`: `show` and `list`, as text and JSON. They
/// must agree; the one command is returned.
fn agreed_next(fixture: &Gate, env: &[(&str, &str)], task_id: &str) -> String {
    let (code, shown, stderr) = fixture.af_env(env, &["task", "show", task_id, "--json"]);
    assert_eq!(code, 0, "{stderr}\n{shown}");
    let shown: Value = serde_json::from_str(shown.trim()).unwrap();
    let next = shown["state"]["next_action"].as_str().unwrap().to_owned();
    let (code, listed, stderr) = fixture.af_env(env, &["task", "list", "--json"]);
    assert_eq!(code, 0, "{stderr}\n{listed}");
    let listed: Value = serde_json::from_str(listed.trim()).unwrap();
    let entry = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == task_id)
        .unwrap();
    assert_eq!(
        entry["state"], shown["state"],
        "list --json and show --json"
    );
    let (code, text, stderr) = fixture.af_env(env, &["task", "show", task_id]);
    assert_eq!(code, 0, "{stderr}\n{text}");
    assert_eq!(
        text.lines().last(),
        Some(format!("next: {next}").as_str()),
        "{text}"
    );
    let (code, list, stderr) = fixture.af_env(env, &["task", "list"]);
    assert_eq!(code, 0, "{stderr}\n{list}");
    let row = list.lines().find(|line| line.starts_with(task_id)).unwrap();
    assert_eq!(
        row.split_whitespace().last(),
        next.split_whitespace().nth(2),
        "{list}"
    );
    next
}

/// A failed gate whose Task has no Attempt left for a new check is not retryable: every
/// rendering names the gate's checks instead of `--retry-gate`, and `--retry-gate` refuses with
/// the limit that stops it and records nothing.
#[test]
fn a_failed_gate_without_an_attempt_left_never_advertises_a_retry() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = gate_with(directory.path(), "pagination", "no-room", true, false);
    fixture.fail_preflight(true);
    let (code, stdout, stderr) = fixture.start();
    assert_eq!(code, 5, "{stderr}\n{stdout}");
    assert_eq!(
        agreed_next(&fixture, &[], "no-room"),
        fixture.next("af task show no-room --checks")
    );
    let before = fixture.show_json("no-room");
    fixture.fail_preflight(false);
    let (code, stdout, stderr) = fixture.af(&["task", "run", "no-room", "--retry-gate"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stderr.contains(
            "Task no-room cannot retry its gate: the Task's limits leave no Attempt for \
             root.nodes.check (Task Attempt limit protects still-required verification)"
        ),
        "{stderr}"
    );
    assert_eq!(fixture.show_json("no-room"), before, "nothing was recorded");
}

/// A failed gate whose Task deadline passed is not retryable either: on the Store's clock past
/// the deadline every rendering names the gate's checks and `--retry-gate` refuses, naming the
/// deadline; on the current clock the same Task advertises the retry the kernel runs.
#[test]
fn a_failed_gate_past_its_deadline_never_advertises_a_retry() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = gate(directory.path(), "pagination", "late-gate", true);
    fixture.fail_preflight(true);
    let (code, stdout, stderr) = fixture.start();
    assert_eq!(code, 5, "{stderr}\n{stdout}");
    assert_eq!(
        agreed_next(&fixture, &[], "late-gate"),
        fixture.next("af task run late-gate --retry-gate")
    );
    // 2100-01-01, long after any deadline the fixture's Task file sets.
    let late = [("AF_TEST_CLOCK_FIXED_MS", "4102444800000")];
    assert_eq!(
        agreed_next(&fixture, &late, "late-gate"),
        fixture.next("af task show late-gate --checks")
    );
    let (code, stdout, stderr) =
        fixture.af_env(&late, &["task", "run", "late-gate", "--retry-gate"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stderr.contains("Task late-gate cannot retry its gate: its deadline passed"),
        "{stderr}"
    );
}

/// A retrying writer that dies after `TaskGateRetried@1` and before the new check publishes
/// leaves the gate's evidence in place: `--json` still has `gate`, with the superseded
/// receipt's checks and the pending retry, and the text prints the same check and retry.
#[test]
fn a_pending_gate_retry_keeps_the_gate_evidence() {
    use std::os::unix::process::CommandExt;
    let directory = tempfile::tempdir().unwrap();
    let fixture = gate(directory.path(), "pagination", "pending-retry", true);
    fixture.fail_preflight(true);
    let (code, stdout, stderr) = fixture.start();
    assert_eq!(code, 5, "{stderr}\n{stdout}");
    let before = fixture.show_json("pending-retry");
    assert_eq!(before["gate"]["status"], "failed", "{before:#}");

    let hang = fixture.root.join("hang-check");
    std::fs::write(&hang, b"").unwrap();
    let log = fixture.root.join("retry.stderr");
    let mut child = crate::common::af()
        .current_dir(&fixture.repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["task", "run", "pending-retry", "--retry-gate", "--state"])
        .arg(&fixture.state)
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .process_group(0)
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !std::fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("retrying gate root.nodes.check")
    {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(120),
            "{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        assert!(child.try_wait().unwrap().is_none(), "the retry ended early");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // The writer dies with its whole process group, before the new check publishes.
    let group = i32::try_from(child.id()).unwrap();
    // `--` first: procps kill (Linux) reads a bare `-PGID` after `-KILL` as another option and
    // signals nothing, exiting 0.
    let killed = Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .status()
        .unwrap();
    assert!(killed.success(), "kill -KILL -- -{group}: {killed}");
    child.wait().unwrap();
    std::fs::remove_file(&hang).unwrap();

    let pending = fixture.show_json("pending-retry");
    let gate = &pending["gate"];
    assert_eq!(gate["status"], before["gate"]["status"], "{pending:#}");
    assert_eq!(
        gate["check_reports"], before["gate"]["check_reports"],
        "the superseded receipt's report: {pending:#}"
    );
    let [retry] = gate["retries"].as_array().unwrap().as_slice() else {
        panic!("one pending retry: {pending:#}");
    };
    assert_eq!(retry["gates"][0]["node"], "root.nodes.check");
    let (code, text, stderr) = fixture.af(&["task", "show", "pending-retry"]);
    assert_eq!(code, 0, "{stderr}\n{text}");
    assert!(
        text.lines()
            .any(|line| line == "gate retried: root.nodes.check ran again on the same candidate"),
        "{text}"
    );
    assert!(
        text.lines().any(|line| line
            == "check pagination: stopped at preflight step performance-kernel-harness \
                (exit 1); fix: python3 -I scripts/performance/cli.py harness-digest --write"),
        "{text}"
    );
}

/// A paid `side` Worker that reads the sealed candidate beside the check, with no edge to it:
/// its package is the evaluator's without the check receipt, run as a plain `worker` node, and
/// it fails while `<root>/fail-side` exists. Its node feeds nothing, so a gate retry would keep
/// its Attempt.
fn add_side_worker(root: &Path, repo: &Path) {
    let packages = repo.join(".af/task-packages/fixture");
    let side = packages.join("side");
    std::fs::create_dir_all(side.join("outputs")).unwrap();
    let schema = "outputs/result.schema.json";
    std::fs::copy(packages.join("evaluator").join(schema), side.join(schema)).unwrap();
    let mut input: Value = serde_json::from_slice(
        &std::fs::read(packages.join("evaluator/input.schema.json")).unwrap(),
    )
    .unwrap();
    input["properties"]
        .as_object_mut()
        .unwrap()
        .remove("checks");
    input["required"] = json!(["requirements", "source"]);
    std::fs::write(side.join("input.schema.json"), input.to_string()).unwrap();
    let mut manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(packages.join("evaluator/worker.toml")).unwrap())
            .unwrap();
    manifest["name"] = toml::Value::String("fixture/side".into());
    let signature = manifest["signature"].as_table_mut().unwrap();
    signature["contract"]["inputs"]
        .as_table_mut()
        .unwrap()
        .remove("checks");
    signature["retains"]["result"] = toml::Value::Array(vec!["requirements".into()]);
    std::fs::write(
        side.join("worker.toml"),
        toml::to_string(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        side.join("worker.py"),
        format!(
            "import json,os,sys\njson.load(sys.stdin)\nif os.path.exists({:?}):\n    \
             sys.exit('the side Worker fails')\nprint(json.dumps({{'schema':'af.worker-reply/1',\
             'outputs':{{'result':[{{'outcome':'passed','reason':'side'}}]}}}}))\n",
            root.join("fail-side").display().to_string()
        ),
    )
    .unwrap();
    let path = packages.join("implementation/pipeline.toml");
    let mut pipeline: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let slots = pipeline["slots"].as_table_mut().unwrap();
    let mut slot = slots["evaluator"].clone();
    slot["worker"] = toml::Value::String("fixture/side".into());
    slots.insert("side".into(), slot);
    let node: toml::Value = toml::from_str(
        "id = \"side\"\n[operator]\nop = \"worker\"\nslot = \"side\"\n\
         [inputs.requirements]\nkind = \"input\"\nport = \"requirements\"\n\
         [inputs.source]\nkind = \"node\"\nnode = \"seal\"\nport = \"snapshot\"\n",
    )
    .unwrap();
    pipeline["nodes"].as_array_mut().unwrap().push(node);
    let attempts = pipeline["max_attempts"].as_integer().unwrap();
    pipeline["max_attempts"] = toml::Value::Integer(attempts + 1);
    std::fs::write(&path, toml::to_string(&pipeline).unwrap()).unwrap();
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut pin = catalog["packages"]["fixture/evaluator"].clone();
    pin["path"] = toml::Value::String(".af/task-packages/fixture/side".into());
    catalog["packages"]
        .as_table_mut()
        .unwrap()
        .insert("fixture/side".into(), pin);
    std::fs::write(&path, toml::to_string(&catalog).unwrap()).unwrap();
}

/// A paid Worker that ran beside a failed gate and failed too keeps its failure across a gate
/// retry, which reopens only the gate's branch: the retry is refused and never advertised. The
/// same pipeline whose side Worker passed and whose check alone failed is retried.
#[test]
fn a_failed_gate_beside_a_failed_paid_node_is_not_retried() {
    for side_fails in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let task_id = if side_fails {
            "side-failed"
        } else {
            "side-passed"
        };
        let fixture = gate_edited(&root, "pagination", task_id, true, true, &|repo| {
            add_side_worker(&root, repo)
        });
        // One more verifier, so one more Attempt and one more in the verification reserve.
        let mut ticket: Value =
            serde_json::from_slice(&std::fs::read(&fixture.ticket).unwrap()).unwrap();
        for limit in ["/limits/max_attempts", "/limits/verification/attempts"] {
            let value = ticket.pointer_mut(limit).unwrap();
            *value = json!(value.as_u64().unwrap() + 1);
        }
        std::fs::write(&fixture.ticket, serde_json::to_vec(&ticket).unwrap()).unwrap();
        if side_fails {
            std::fs::write(root.join("fail-side"), b"").unwrap();
        }
        fixture.fail_preflight(true);
        let (code, stdout, stderr) = fixture.start();
        assert_eq!(code, 5, "{task_id}: {stderr}\n{stdout}");
        let next = agreed_next(&fixture, &[], task_id);
        let (code, stdout, stderr) = fixture.af(&["task", "run", task_id, "--retry-gate"]);
        if side_fails {
            assert_eq!(
                next,
                fixture.next(&format!("af task show {task_id} --checks")),
                "{:#}",
                fixture.show_json(task_id)
            );
            assert_eq!(code, 1, "{stdout}");
            assert!(
                stderr.contains(&format!(
                    "Task {task_id} cannot retry its gate: a paid node ran beside its failed \
                     gate, so its last failure was not the gate's alone"
                )),
                "{stderr}"
            );
        } else {
            assert_eq!(
                next,
                fixture.next(&format!("af task run {task_id} --retry-gate"))
            );
            assert_ne!(code, 1, "the retry ran: {stderr}\n{stdout}");
            assert!(
                stderr.contains(&format!("retrying gate root.nodes.check of Task {task_id}")),
                "{stderr}"
            );
        }
    }
}

/// A check that runs past its wall is killed there, and it says so everywhere (ADR-0255): the
/// receipt (`af/TaskCheckReceipt@2`) records `stopped` with the cause `wall`, the two-second
/// per-check wall the gate ran under and how long it ran; the Task ends `check_failed`, never
/// `preflight_failed`; and `af task show` (`why:`), `af task show --checks` (text and
/// `af/task-checks@3`), `af task report` (the row's detail) and the run's stderr each say `check
/// pagination stopped at its 2 s wall after …` with what to do next, while none says `never
/// started`.
#[test]
fn a_check_killed_at_its_wall_says_so_and_never_reads_as_never_started() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = gate_edited(
        directory.path(),
        "pagination",
        "gate-wall",
        true,
        false,
        &|repo| {
            // A per-check wall of two seconds under the load-safe check Attempt wall: the
            // check starts whatever preparation costs, and only its own wall can end it.
            let path = repo.join(".af/code-policy.toml");
            let mut policy: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            policy
                .as_table_mut()
                .unwrap()
                .insert("check_process_wall_ms".into(), toml::Value::Integer(2_000));
            std::fs::write(&path, toml::to_string(&policy).unwrap()).unwrap();
        },
    );
    // The gate waits while this file exists, a minute at most: far past its wall.
    std::fs::write(fixture.root.join("hang-check"), b"").unwrap();
    let wall = "check pagination stopped at its 2 s wall after ";
    let next = "make the check faster, or raise its wall in .af/code-policy.toml \
                (check_wall_ms, check_process_wall_ms or [warm] build_wall_ms) and plan the \
                Task again";
    let (code, stdout, stderr) = fixture.af(&[
        "task",
        "start",
        "--execute",
        "--file",
        fixture.ticket.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        code, 5,
        "a stopped check fails the gate\n{stderr}\n{stdout}"
    );
    let named = |text: &str| {
        text.lines()
            .any(|line| line.contains(wall) && line.contains(next))
    };
    assert!(named(&stderr), "the run's stderr names the wall: {stderr}");
    assert!(!stderr.contains("never started"), "{stderr}");
    assert!(!stderr.contains("preflight"), "{stderr}");

    // The receipt records the stop, typed.
    let receipts = artifacts(&fixture.cas(), &fixture.state, "af/TaskCheckReceipt@2");
    let [receipt] = receipts.as_slice() else {
        panic!("one receipt: {receipts:?}");
    };
    crate::schemas::valid(
        &crate::schemas::validator("task-check-receipt-v2.json"),
        receipt,
    );
    let stop = &receipt["stopped"]["pagination"];
    assert_eq!(stop["cause"], "wall", "{receipt:#}");
    assert_eq!(stop["wall_ms"], 2_000, "{receipt:#}");
    let elapsed = stop["elapsed_ms"].as_u64().unwrap();
    assert!(elapsed >= 1_900, "it ran until its wall: {receipt:#}");
    assert!(receipt.get("acceptance").is_none(), "{receipt:#}");
    assert!(
        artifacts(&fixture.cas(), &fixture.state, "af/TaskCheckReceipt@1").is_empty(),
        "the retired receipt version is never written"
    );

    // `af task show`: the state is check_failed, and `why:` names the wall.
    let (code, shown, stderr) = fixture.af(&["task", "show", "gate-wall"]);
    assert_eq!(code, 0, "{stderr}\n{shown}");
    assert_eq!(
        shown.lines().next(),
        Some("Task gate-wall: finished/gate_failed/check_failed"),
        "{shown}"
    );
    assert!(
        shown
            .lines()
            .any(|line| line.starts_with(&format!("why: {wall}")) && line.contains(next)),
        "{shown}"
    );
    assert!(named(&shown), "{shown}");
    assert!(!shown.contains("never started"), "{shown}");
    assert!(!shown.contains("preflight"), "{shown}");

    // `af task show --checks`, in text and as `af/task-checks@3`.
    let (code, checks, stderr) = fixture.af(&["task", "show", "gate-wall", "--checks"]);
    assert_eq!(code, 0, "{stderr}\n{checks}");
    assert!(
        checks
            .lines()
            .any(|line| line.starts_with("check pagination: not_run, stopped at its 2 s wall")),
        "{checks}"
    );
    assert!(
        checks.lines().any(|line| line == format!("  next: {next}")),
        "{checks}"
    );
    assert!(!checks.contains("never started"), "{checks}");
    let (code, document, stderr) = fixture.af(&["task", "show", "gate-wall", "--checks", "--json"]);
    assert_eq!(code, 0, "{stderr}\n{document}");
    let document: Value = serde_json::from_str(document.trim()).unwrap();
    crate::schemas::valid(&crate::schemas::validator("task-checks-v3.json"), &document);
    assert_eq!(document["schema"], "af/task-checks@3");
    assert_eq!(document["state"]["cause"], "check_failed");
    assert_eq!(document["receipt"]["checks"][0]["stop"], *stop);

    // `af task report`: the row's detail, in text and in `--json`.
    let (code, report, stderr) = fixture.af(&["task", "report", "gate-wall"]);
    assert_eq!(code, 0, "{stderr}\n{report}");
    assert!(report.contains(wall) && report.contains(next), "{report}");
    assert!(!report.contains("never started"), "{report}");
    let (code, report, stderr) = fixture.af(&["task", "report", "gate-wall", "--json"]);
    assert_eq!(code, 0, "{stderr}\n{report}");
    let report: Value = serde_json::from_str(report.trim()).unwrap();
    let detail = report["tasks"][0]["detail"].as_str().unwrap();
    assert!(
        detail.starts_with(wall) && detail.ends_with(next),
        "{detail}"
    );
}

/// A package that still declares the receipt before ADR-0255 is refused by that name when the
/// plan compiles, with the rename and re-pin that fix it, and nothing runs; `af catalog digest`
/// still prints its pin, which is its bytes.
#[test]
fn a_package_that_declares_the_retired_receipt_is_refused_by_name() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = gate_edited(
        directory.path(),
        "pagination",
        "gate-retired",
        true,
        false,
        &|repo| {
            let path = repo.join(".af/task-packages/fixture/evaluator/worker.toml");
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("af/TaskCheckReceipt@2"));
            std::fs::write(
                &path,
                text.replace("af/TaskCheckReceipt@2", "af/TaskCheckReceipt@1"),
            )
            .unwrap();
        },
    );
    let (code, stdout, stderr) = fixture.start();
    assert_ne!(code, 0, "{stderr}\n{stdout}");
    assert!(
        stderr.contains(
            "af/TaskCheckReceipt@1 is the check receipt before ADR-0255, which this release \
             neither writes nor reads; declare af/TaskCheckReceipt@2 instead and re-pin the \
             package with `af catalog digest`"
        ),
        "{stderr}"
    );
    assert!(
        artifacts(&fixture.cas(), &fixture.state, "af/TaskCheckReceipt@2").is_empty()
            && artifacts(&fixture.cas(), &fixture.state, "af/TaskCheckReceipt@1").is_empty(),
        "no check ran"
    );
    // `af catalog digest` takes no `--state`, so it is asked without the helper's.
    let output = crate::common::af()
        .current_dir(&fixture.repo)
        .args(["catalog", "digest", ".af/task-packages/fixture/evaluator"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("sha256:"),
        "{output:?}"
    );
}
