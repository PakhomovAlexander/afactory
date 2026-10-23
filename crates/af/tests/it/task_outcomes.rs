//! One Task state model across every rendering (P5): `af task list`, `af task show`, `af task
//! report` and the run's own output print the same `phase/outcome/cause` and next command, in
//! text and under `--json`; a failed gate names its failing tests; a refused plan names the limits
//! that fit and `--fit` plans with them; a Task file takes its kind's defaults; planning is
//! idempotent and `--replace` re-plans in place; the wall clock starts at the first run; and
//! every not-found error names what was looked for and where. The fixture pipeline's Workers are
//! commands: no test spends a model token.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Output;

use crate::{schemas, task_cli, task_interrupt};

const TASK: &str = "pagination-cli";

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
}

fn fixture(name: &str) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let (repo, state) = task_cli::fixture_named(&root, name);
    Fixture {
        _directory: directory,
        root,
        repo,
        state,
    }
}

impl Fixture {
    /// `af ARGS --state STATE` from the fixture's checkout.
    fn af(&self, args: &[&str]) -> Output {
        crate::common::af()
            .current_dir(&self.repo)
            .args(args)
            .arg("--state")
            .arg(&self.state)
            .output()
            .unwrap()
    }

    /// The `--json` document of `af ARGS`, which must succeed.
    fn json(&self, args: &[&str]) -> Value {
        let output = self.af(&[args, &["--json"]].concat());
        assert!(
            output.status.success(),
            "{args:?}: {}\n{}",
            out(&output),
            err(&output)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// A copy of the fixture's Task file outside the checkout, edited by `edit`.
    fn task_file(&self, name: &str, edit: impl FnOnce(&mut Value)) -> PathBuf {
        let mut ticket: Value =
            serde_json::from_slice(&std::fs::read(self.repo.join("ticket.json")).unwrap()).unwrap();
        edit(&mut ticket);
        let path = self.root.join(name);
        std::fs::write(&path, serde_json::to_vec_pretty(&ticket).unwrap()).unwrap();
        path
    }

    fn write(&self, relative: &str, text: &str) {
        std::fs::write(self.repo.join(relative), text).unwrap();
    }

    /// Repin every package and commit, as an operator's reviewed change would.
    fn commit(&self) {
        task_interrupt::commit_fixture(&self.repo);
    }
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The check fails on two named tests, printed as nextest prints them.
fn failing_check(fixture: &Fixture) {
    let policy = fixture.repo.join(".af/code-policy.toml");
    let text = std::fs::read_to_string(&policy).unwrap();
    let check = "import pagination; assert pagination.paginate(list(range(7)),2,3) == [2,3,4]";
    assert_eq!(text.matches(check).count(), 1);
    let failing = "import sys; print('        FAIL [   0.004s] af::it alpha::one', file=sys.stderr); \
                   print('     TIMEOUT [  60.000s] af::it beta::two', file=sys.stderr); sys.exit(1)";
    std::fs::write(&policy, text.replace(check, failing)).unwrap();
    fixture.commit();
}

/// The evaluator rejects the sealed result.
fn rejecting_evaluator(fixture: &Fixture) {
    fixture.write(
        ".af/task-packages/fixture/evaluator/worker.py",
        "import json,sys\nrequest=json.load(sys.stdin)\nprint(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':'failed','reason':'Offsets are off by one'}]}}))\n",
    );
    fixture.commit();
}

/// The implementer replies with something that is not a Worker reply.
fn malformed_implementer(fixture: &Fixture) {
    fixture.write(
        ".af/task-packages/fixture/implementer/worker.py",
        "import json,sys\nrequest=json.load(sys.stdin)\nprint('not a reply')\n",
    );
    fixture.commit();
}

/// The state every rendering of the Task prints, read from each: the JSON documents of `show`,
/// `list` and `report`, and the text of `show`, `list` and `report`. They must all agree, and
/// the triple and the next command they agree on are returned.
fn agreed_state(fixture: &Fixture) -> (String, Option<String>) {
    let shown = fixture.json(&["task", "show", TASK]);
    schemas::valid(&schemas::validator("task-inspection-v12.json"), &shown);
    let listed = fixture.json(&["task", "list"]);
    schemas::valid(&schemas::validator("task-list-v3.json"), &listed);
    let reported = fixture.json(&["task", "report", TASK]);
    schemas::valid(&schemas::validator("task-report-v2.json"), &reported);
    let state = shown["state"].clone();
    let entry = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == TASK)
        .unwrap()
        .clone();
    assert_eq!(entry["state"], state, "list --json and show --json");
    assert_eq!(
        reported["tasks"][0]["state"], state,
        "report --json and show --json"
    );
    let label = [&state["phase"], &state["outcome"], &state["cause"]]
        .into_iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("/");
    let next = state["next_action"].as_str().map(str::to_owned);

    let show = fixture.af(&["task", "show", TASK]);
    let show = out(&show);
    let lines: Vec<&str> = show.lines().collect();
    assert_eq!(lines[0], format!("Task {TASK}: {label}"), "{show}");
    assert_eq!(
        *lines.last().unwrap(),
        format!("next: {}", next.as_deref().unwrap_or("none")),
        "{show}"
    );
    let list = out(&fixture.af(&["task", "list"]));
    let row = list.lines().find(|l| l.starts_with(TASK)).unwrap();
    let cells: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cells[1], label, "{list}");
    let verb = next
        .as_deref()
        .and_then(|n| n.split_whitespace().nth(2))
        .unwrap_or("-");
    assert_eq!(cells.last().copied(), Some(verb), "{list}");
    let report = out(&fixture.af(&["task", "report", TASK]));
    let row = report
        .lines()
        .find(|l| l.starts_with(&format!("| 1 | {TASK} |")))
        .unwrap();
    assert!(
        row.starts_with(&format!("| 1 | {TASK} | {label}")),
        "{row}\n{report}"
    );
    (label, next)
}

/// Each outcome the fixture pipeline reaches, driven through the CLI: the run's own output, its
/// exit code (`af help exit-codes`), and every other rendering print the same state, and every
/// state that did not verify names a next command.
#[test]
fn every_rendering_prints_the_same_state_for_each_outcome() {
    struct Case {
        name: &'static str,
        setup: fn(&Fixture),
        label: &'static str,
        next: &'static str,
        code: i32,
    }
    let cases = [
        Case {
            name: "verified",
            setup: |_| {},
            label: "finished/verified",
            next: "af task deliver pagination-cli --branch af/pagination-cli --worktree ../pagination-cli --confirm pagination-cli",
            code: 0,
        },
        Case {
            name: "gate_failed",
            setup: failing_check,
            label: "finished/gate_failed/check_failed",
            next: "af task run pagination-cli --retry-gate",
            code: 5,
        },
        Case {
            name: "changes_requested",
            setup: rejecting_evaluator,
            label: "finished/changes_requested/evaluator_rejected",
            next: "af task show pagination-cli",
            code: 3,
        },
        Case {
            name: "provider_failed",
            setup: malformed_implementer,
            label: "finished/provider_failed/malformed_reply",
            next: "af task show pagination-cli",
            code: 6,
        },
    ];
    for case in cases {
        let fixture = fixture("pagination");
        (case.setup)(&fixture);
        let file = fixture.task_file("ticket.json", |_| {});
        let file = file.to_str().unwrap();
        // The run's own output: its first line is the state and its last the next command.
        let run = fixture.af(&["task", "start", "--file", file, "--execute"]);
        assert_eq!(
            run.status.code(),
            Some(case.code),
            "{}: {}\n{}",
            case.name,
            out(&run),
            err(&run)
        );
        let printed = out(&run);
        let lines: Vec<&str> = printed.lines().collect();
        assert_eq!(
            lines[0],
            format!("Task {TASK}: {}", case.label),
            "{}: {printed}",
            case.name
        );
        assert_eq!(
            *lines.last().unwrap(),
            format!("next: {}", case.next),
            "{}: {printed}",
            case.name
        );
        let (label, next) = agreed_state(&fixture);
        assert_eq!(label, case.label, "{}", case.name);
        assert_eq!(next.as_deref(), Some(case.next), "{}", case.name);
        // The same run under --json prints the document on stdout and its last log line on
        // stderr.
        let again = fixture.af(&["task", "run", TASK, "--json"]);
        assert_eq!(again.status.code(), Some(case.code), "{}", case.name);
        let document: Value = serde_json::from_slice(&again.stdout).unwrap();
        assert_eq!(
            document["state"]["next_action"].as_str(),
            Some(case.next),
            "{}",
            case.name
        );
        assert!(
            err(&again)
                .trim_end()
                .ends_with(&format!("Task {TASK}: {}; next: {}", case.label, case.next)),
            "{}: {}",
            case.name,
            err(&again)
        );
    }
}

/// A planned Task and an interrupted one print one state too: planned with the command that
/// confirms its plan, and abandoned, cancelled by the host, with the command that resumes it.
#[test]
fn a_planned_and_an_interrupted_task_print_the_same_state_everywhere() {
    let planned = fixture("pagination");
    let file = planned.task_file("ticket.json", |_| {});
    let plan = planned.json(&["task", "plan", "--file", file.to_str().unwrap()]);
    let (label, next) = agreed_state(&planned);
    assert_eq!(label, "planned");
    assert_eq!(
        next.unwrap(),
        format!(
            "af task run {TASK} --confirm-plan {}",
            plan["plan_id"].as_str().unwrap()
        )
    );

    let interrupted = fixture("pagination");
    let ready = interrupted.root.join("worker-ready");
    let resume = interrupted.root.join("worker-resume");
    task_interrupt::long_running_implementer(&interrupted.repo, &ready, &resume);
    let stopped = task_interrupt::interrupt_running_worker(
        &interrupted.repo,
        &interrupted.state,
        &ready,
        &[nix::sys::signal::Signal::SIGINT],
    );
    assert!(!stopped.status.success(), "{}", err(&stopped));
    let (label, next) = agreed_state(&interrupted);
    assert_eq!(label, "abandoned/cancelled/host_cancelled");
    assert_eq!(next.unwrap(), format!("af task run {TASK}"));
}

/// Replace every artifact ID with `sha256:<id>` and every measured duration with `<ms>`, so a
/// snapshot holds only what does not vary between runs.
fn stable(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("sha256:") {
        out.push_str(&rest[..at]);
        out.push_str("sha256:<id>");
        rest = &rest[at + 7..];
        rest = rest.trim_start_matches(|c: char| c.is_ascii_hexdigit());
    }
    out.push_str(rest);
    let words: Vec<String> = out
        .split('\n')
        .map(|line| {
            let mut line = line.to_owned();
            if let Some(at) = line.find(" in ")
                && let Some(end) = line[at + 4..].find(" ms")
                && line[at + 4..at + 4 + end]
                    .bytes()
                    .all(|b| b.is_ascii_digit())
            {
                line.replace_range(at + 4..at + 4 + end, "<ms>");
            }
            line
        })
        .collect();
    words.join("\n")
}

/// A check that fails on two named tests: `--checks` names them with the end of its stderr, the
/// list's `gate` and the run's own output carry them, and `--checks --json` is the receipt
/// document.
#[test]
fn a_failed_gate_names_its_failing_tests_in_checks_the_list_and_the_run() {
    let fixture = fixture("pagination");
    failing_check(&fixture);
    let file = fixture.task_file("ticket.json", |_| {});
    let run = fixture.af(&[
        "task",
        "start",
        "--file",
        file.to_str().unwrap(),
        "--execute",
    ]);
    assert_eq!(run.status.code(), Some(5), "{}", err(&run));
    let printed = out(&run);
    let tail: Vec<&str> = printed.lines().collect();
    let at = tail
        .iter()
        .position(|line| line.starts_with("check pagination: failed: "))
        .unwrap();
    let tail = stable(&tail[at..].join("\n"));
    assert_eq!(
        tail,
        "\
check pagination: failed: 2 failing tests
  FAIL alpha::one
  FAIL beta::two
  stderr of check pagination: sha256:<id>
next: af task run pagination-cli --retry-gate"
    );
    let checks = fixture.af(&["task", "show", TASK, "--checks"]);
    assert!(checks.status.success(), "{}", err(&checks));
    assert_eq!(
        stable(&out(&checks)),
        "\
Task pagination-cli: finished/gate_failed/check_failed
receipt sha256:<id> from root.nodes.check: failed
check pagination: failed (exit 1) in <ms> ms, cold (no warm cache)
  failing tests (2):
    alpha::one
    beta::two
  stderr sha256:<id>, last 2 lines:
            FAIL [   0.004s] af::it alpha::one
         TIMEOUT [  60.000s] af::it beta::two
next: af task run pagination-cli --retry-gate
"
    );
    let document = fixture.json(&["task", "show", TASK, "--checks"]);
    schemas::valid(&schemas::validator("task-checks-v1.json"), &document);
    assert_eq!(document["schema"], "af/task-checks@1");
    let check = &document["receipt"]["checks"][0];
    assert_eq!(
        check["failed_tests"],
        serde_json::json!(["alpha::one", "beta::two"])
    );
    assert_eq!(check["exit_code"], 1);
    assert_eq!(check["stderr_tail"].as_array().unwrap().len(), 2);
    let listed = fixture.json(&["task", "list"]);
    assert_eq!(
        listed["tasks"][0]["gate"],
        serde_json::json!({"status": "failed", "failed_tests": ["alpha::one", "beta::two"], "flaky": []})
    );
    let shown = out(&fixture.af(&["task", "show", TASK]));
    assert!(
        shown.contains("why: check pagination, 2 tests: alpha::one, beta::two"),
        "{shown}"
    );
    let report = out(&fixture.af(&["task", "report", TASK]));
    assert!(
        report.contains(
            "| finished/gate_failed/check_failed: check pagination, 2 tests: alpha::one, beta::two |"
        ),
        "{report}"
    );
    // A command that exits with the outcome's code while its result goes to stdout says why on
    // stderr too: a non-zero exit never leaves stderr silent.
    let state_line = format!(
        "Task {TASK}: finished/gate_failed/check_failed; next: af task run {TASK} --retry-gate"
    );
    for args in [
        &["task", "explain", TASK][..],
        &["task", "explain", TASK, "--json"],
        &["task", "run", TASK],
    ] {
        let exited = fixture.af(args);
        assert_eq!(exited.status.code(), Some(5), "{args:?}: {}", err(&exited));
        assert!(
            err(&exited).trim_end().ends_with(&state_line),
            "{args:?}: {}",
            err(&exited)
        );
    }
}

/// Every refusal before any state exists exits non-zero with its reason on stderr, in text and
/// under `--json`, where stdout carries the one `af/error@1` document.
#[test]
fn a_refusal_names_its_reason_on_stderr_in_text_and_json() {
    let fixture = fixture("pagination");
    let zero = fixture.task_file("zero.json", |ticket| {
        ticket["limits"]["wall_ms"] = 0.into();
    });
    let zero = zero.to_str().unwrap();
    for args in [
        &["task", "plan", "--file", zero][..],
        &["task", "start", "--file", zero],
        &["task", "start", "--file", zero, "--execute"],
    ] {
        for json in [false, true] {
            let args = [args, if json { &["--json"][..] } else { &[] }].concat();
            let refused = fixture.af(&args);
            assert_eq!(
                refused.status.code(),
                Some(1),
                "{args:?}: {:?}\n{}",
                refused.status,
                err(&refused)
            );
            assert!(
                err(&refused).contains("Task wall_ms and --timeout-secs must be positive"),
                "{args:?}: {:?}\n{}",
                refused.status,
                err(&refused)
            );
            if json {
                let document: Value = serde_json::from_slice(&refused.stdout).unwrap();
                assert_eq!(document["schema"], "af/error@1", "{args:?}");
            } else {
                assert!(out(&refused).is_empty(), "{args:?}: {}", out(&refused));
            }
            assert!(!fixture.state.exists(), "{args:?} created state");
        }
    }
    let tight = fixture.task_file("tight.json", |ticket| {
        ticket["limits"]["max_attempts"] = 2.into();
    });
    let refused = fixture.af(&["task", "plan", "--file", tight.to_str().unwrap(), "--json"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        err(&refused).contains("cannot retain verification reserves"),
        "{}",
        err(&refused)
    );
}

/// `af task show` prints each failed node's cause, its first error line and its diagnostic, and
/// the node that only missed a branch's receipt after the node that failed, naming it.
#[test]
fn show_names_each_failed_node_s_cause_and_first_error_line() {
    let fixture = fixture("pagination");
    malformed_implementer(&fixture);
    let file = fixture.task_file("ticket.json", |_| {});
    let run = fixture.af(&[
        "task",
        "start",
        "--file",
        file.to_str().unwrap(),
        "--execute",
    ]);
    assert_eq!(run.status.code(), Some(6), "{}", err(&run));
    let shown = out(&fixture.af(&["task", "show", TASK]));
    let failures: Vec<String> = stable(&shown)
        .lines()
        .skip_while(|line| !line.starts_with("root.nodes."))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        failures,
        [
            "root.nodes.implement: malformed_reply: Invalid typed Worker reply: expected ident at line 1 column 2",
            "  diagnostic sha256:<id>",
            "root.nodes.evaluate: follows from the failure above: Required branch receipt was not produced: root.nodes.check did not publish one",
            "  diagnostic sha256:<id>",
            "next: af task show pagination-cli",
        ],
        "{shown}"
    );
    let document = fixture.json(&["task", "show", TASK]);
    assert_eq!(document["failures"][0]["cause"], "malformed_reply");
    assert!(document["failures"][1]["cause"].is_null());
    let report = out(&fixture.af(&["task", "report", TASK]));
    assert!(
        report
            .contains("| finished/provider_failed/malformed_reply: root.nodes.implement failed |"),
        "{report}"
    );
}

/// Limits too small for the plan's reserves are refused on stderr with every term: the reserve
/// each verification slot protects, what the Task supplied and the deficit; the document names
/// the smallest limits that fit, and `--fit` plans with exactly those.
#[test]
fn a_refused_plan_names_the_limits_that_fit_and_fit_plans_with_them() {
    let fixture = fixture("pagination");
    let file = fixture.task_file("tight.json", |ticket| {
        ticket["limits"]["max_attempts"] = 2.into();
    });
    let file = file.to_str().unwrap();
    let refused = fixture.af(&["task", "plan", "--file", file]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        out(&refused).is_empty(),
        "text mode prints the refusal on stderr only"
    );
    let said = err(&refused);
    let line = said
        .lines()
        .find(|line| line.contains("cannot retain verification reserves"))
        .unwrap();
    assert_eq!(
        line,
        "  fixture/implementation: Task cannot retain verification reserves and mandatory work \
         within its token and Attempt allowance: it needs tokens >= 200 (verification reserve \
         200 + 0 for one Attempt of each mandatory Worker) and max_attempts >= 3 (2 + 1); the \
         Task supplies tokens 1000, max_attempts 2, a deficit of 0 tokens and 1 Attempts; \
         reserves per verification slot: [root.nodes.check 1 x 0 tokens; root.nodes.evaluate 1 \
         x 0 tokens]"
    );
    assert!(
        said.contains(&format!("next: af task plan --file {file} --fit")),
        "{said}"
    );
    let document = fixture.af(&["task", "plan", "--file", file, "--json"]);
    let document: Value = serde_json::from_slice(&document.stdout).unwrap();
    schemas::valid(&schemas::validator("task-selection-v2.json"), &document);
    let diagnostic = &document["planning_diagnostic"];
    let required = &diagnostic["required_limits"];
    assert_eq!(
        *required,
        serde_json::json!({"tokens": 1000, "max_attempts": 3, "wall_ms": 60000,
            "verification": {"tokens": 200, "attempts": 2, "wall_ms": 10000}})
    );
    assert_eq!(
        diagnostic["raised"],
        serde_json::json!([{"limit": "max_attempts", "supplied": 2, "required": 3}])
    );
    let printed = format!(
        "  the smallest limits that fit: tokens {}, max_attempts {}, wall_ms {}, verification tokens {} attempts {} wall_ms {}",
        required["tokens"],
        required["max_attempts"],
        required["wall_ms"],
        required["verification"]["tokens"],
        required["verification"]["attempts"],
        required["verification"]["wall_ms"]
    );
    assert!(said.contains(&printed), "{said}");
    assert_eq!(diagnostic["reserves"][0]["node"], "root.nodes.check");

    let fitted = fixture.af(&["task", "plan", "--file", file, "--fit"]);
    assert!(fitted.status.success(), "{}", err(&fitted));
    assert!(
        err(&fitted).contains(
            "af task plan --fit: planned with max_attempts 2 -> 3; the Task file on disk is unchanged"
        ),
        "{}",
        err(&fitted)
    );
    let explained = fixture.json(&["task", "explain", TASK]);
    let limits = &explained["plan"]["limits"];
    assert_eq!(limits["tokens"], required["tokens"]);
    assert_eq!(limits["max_attempts"], required["max_attempts"]);
    assert_eq!(limits["verification"], required["verification"]);
    let ticket: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(
        ticket["limits"]["max_attempts"], 2,
        "the file on disk is never edited"
    );
}

/// A Task file without `strategy` takes its kind's, `verification` and `facts.standard` with it,
/// and a `verification` the kind's profile cannot verify by is refused with both values and the
/// table.
#[test]
fn a_task_file_takes_its_kind_s_defaults_and_a_contradiction_names_both_values() {
    let fixture = fixture("embedded-review");
    let file = fixture.task_file("only-kind.json", |ticket| {
        let ticket = ticket.as_object_mut().unwrap();
        for field in ["strategy", "verification", "facts", "pipeline"] {
            ticket.remove(field);
        }
    });
    let plan = fixture.json(&["task", "plan", "--file", file.to_str().unwrap()]);
    assert_eq!(plan["state"]["phase"], "planned");
    let explained = fixture.json(&["task", "explain", TASK]);
    let revision: Value = {
        let id = explained["revision_id"].as_str().unwrap();
        let cas = review_store::Cas::open_existing(fixture.state.join("cas")).unwrap();
        cas.get_json(id).unwrap()["payload"].clone()
    };
    assert_eq!(revision["strategy"], "reviewed");
    assert_eq!(revision["facts"]["standard"], true);

    let review = crate::task_cli::fixture_named(&fixture.root.join("review"), "review");
    let mut ticket: Value =
        serde_json::from_slice(&std::fs::read(review.0.join("review.json")).unwrap()).unwrap();
    ticket["verification"] = "evaluation".into();
    let contradicting = fixture.root.join("contradicting.json");
    std::fs::write(&contradicting, serde_json::to_vec(&ticket).unwrap()).unwrap();
    let refused = crate::common::af()
        .current_dir(&review.0)
        .args(["task", "plan", "--file"])
        .arg(&contradicting)
        .arg("--state")
        .arg(&review.1)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    let said = err(&refused);
    for needle in [
        "asks for verification `evaluation`",
        "kind `review` is the `review` profile",
        "implement -> strategy reviewed, verification review, facts standard = true",
        "verification -> strategy light, verification evaluation",
    ] {
        assert!(said.contains(needle), "{needle}:\n{said}");
    }
}

/// Planning the same Task file again shows the plan it recorded; a changed file under the same
/// ID is refused, naming `--replace`; `--replace` plans the unstarted Task again in place.
#[test]
fn planning_is_idempotent_and_replace_plans_an_unstarted_task_again() {
    let fixture = fixture("pagination");
    let file = fixture.task_file("ticket.json", |_| {});
    let file = file.to_str().unwrap();
    let first = fixture.json(&["task", "plan", "--file", file]);
    let again = fixture.af(&["task", "plan", "--file", file, "--json"]);
    assert!(again.status.success(), "{}", err(&again));
    assert!(
        err(&again).contains("already planned from this Task file"),
        "{}",
        err(&again)
    );
    let again: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again["plan_id"], first["plan_id"]);
    assert_eq!(
        again["history"], first["history"],
        "nothing was recorded again"
    );

    let changed = fixture.task_file("ticket.json", |ticket| {
        ticket["goal"] = "Implement offset/limit pagination, and say so".into();
    });
    let changed = changed.to_str().unwrap();
    let refused = fixture.af(&["task", "plan", "--file", changed]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        err(&refused).contains(&format!(
            "Task `{TASK}` is already planned from a different Task file"
        )) && err(&refused).contains(&format!("af task plan --file {changed} --replace")),
        "{}",
        err(&refused)
    );

    let replaced = fixture.af(&["task", "plan", "--file", changed, "--replace", "--json"]);
    assert!(replaced.status.success(), "{}", err(&replaced));
    let replaced: Value = serde_json::from_slice(&replaced.stdout).unwrap();
    assert_ne!(replaced["plan_id"], first["plan_id"]);
    assert_eq!(replaced["state"]["phase"], "planned");
    let listed = fixture.json(&["task", "list"]);
    assert_eq!(
        listed["tasks"].as_array().unwrap().len(),
        1,
        "one Task, re-planned in place"
    );
    let run = fixture.af(&[
        "task",
        "run",
        TASK,
        "--confirm-plan",
        replaced["plan_id"].as_str().unwrap(),
    ]);
    assert!(run.status.success(), "{}", err(&run));
    let other = fixture.task_file("other.json", |ticket| {
        ticket["goal"] = "Implement cursor pagination instead".into();
    });
    let late = fixture.af(&[
        "task",
        "plan",
        "--file",
        other.to_str().unwrap(),
        "--replace",
    ]);
    assert_eq!(late.status.code(), Some(1));
    assert!(
        err(&late).contains("has already run, so --replace cannot plan it again"),
        "{}",
        err(&late)
    );
}

/// The wall clock starts at the first run: two seconds after planning, the run's deadline is
/// still the whole declared wall away, the preview says so, and the run prints it.
#[test]
fn the_wall_clock_starts_at_the_first_run_not_at_plan() {
    let fixture = fixture("pagination");
    let file = fixture.task_file("ticket.json", |_| {});
    let preview = fixture.af(&["task", "plan", "--file", file.to_str().unwrap()]);
    assert!(preview.status.success(), "{}", err(&preview));
    let time = out(&preview)
        .lines()
        .find(|line| line.starts_with("TIME  "))
        .unwrap()
        .to_owned();
    assert_eq!(time, "TIME  1 min wall | deadline starts when you run");
    let plan = fixture.json(&["task", "explain", TASK])["plan_id"]
        .as_str()
        .unwrap()
        .to_owned();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let run = fixture.af(&["task", "run", TASK, "--confirm-plan", &plan]);
    assert!(run.status.success(), "{}", err(&run));
    let line = err(&run)
        .lines()
        .find(|line| line.starts_with(&format!("af task run: Task {TASK} deadline ")))
        .unwrap()
        .to_owned();
    assert!(
        line.ends_with("s from now)") || line.ends_with("min from now)"),
        "{line}"
    );
    let shown = fixture.json(&["task", "show", TASK]);
    let clock = &shown["clock"];
    let deadline = clock["deadline_unix_ms"].as_u64().unwrap();
    let started = clock["started_unix_ms"].as_u64().unwrap();
    // The clock started when the run admitted the plan, two seconds after planning, and gave
    // the Task its declared wall from then: only planning's own time before the Task was
    // recorded is gone, never the wait.
    assert!(started >= before, "{clock}");
    let remaining = deadline - started;
    assert_eq!(remaining, clock["wall_ms"].as_u64().unwrap(), "{clock}");
    assert!(
        remaining.abs_diff(60_000) < 1_000,
        "the run had {remaining} ms of a 60000 ms wall: {clock}"
    );
    // The plan the run confirmed is the plan that ran: only the clock moved its deadline.
    assert_eq!(shown["plan_id"], plan.as_str());
}

/// Not found and cannot open: each names what it looked for and where, and what to run next.
#[test]
fn not_found_errors_name_the_task_the_store_and_what_it_holds() {
    let fixture = fixture("pagination");
    let file = fixture.task_file("ticket.json", |_| {});
    fixture.json(&["task", "plan", "--file", file.to_str().unwrap()]);
    let state = std::fs::canonicalize(&fixture.state)
        .unwrap()
        .display()
        .to_string();
    for args in [
        &["task", "show", "nope"][..],
        &["task", "run", "nope"],
        &["task", "explain", "nope"],
        &["task", "report", "nope"],
    ] {
        let missing = fixture.af(args);
        assert_eq!(missing.status.code(), Some(1), "{args:?}");
        assert!(
            err(&missing).contains(&format!(
                "Task nope not found in the Task Store {state} (1 Task there: pagination-cli); af task list --state {state} lists its Tasks"
            )),
            "{args:?}: {}",
            err(&missing)
        );
    }
    let absent = fixture.root.join("absent.json");
    let unreadable = fixture.af(&["task", "plan", "--file", absent.to_str().unwrap()]);
    assert_eq!(unreadable.status.code(), Some(1));
    assert!(
        err(&unreadable).contains(&format!("cannot open Task file {}: ", absent.display())),
        "{}",
        err(&unreadable)
    );
    let run = fixture.af(&[
        "task",
        "run",
        TASK,
        "--confirm-plan",
        fixture.json(&["task", "explain", TASK])["plan_id"]
            .as_str()
            .unwrap(),
    ]);
    assert!(run.status.success(), "{}", err(&run));
    let port = fixture.af(&[
        "task",
        "output",
        TASK,
        "--port",
        "nope",
        "--output",
        fixture.root.join("out.json").to_str().unwrap(),
    ]);
    assert_eq!(port.status.code(), Some(1));
    assert!(
        err(&port).contains(
            "Task pagination-cli has no such output port nope; ports: snapshot, verification"
        ),
        "{}",
        err(&port)
    );
    let inside = fixture.repo.join("state-inside");
    let refused = crate::common::af()
        .current_dir(&fixture.repo)
        .args(["task", "plan", "--file", file.to_str().unwrap(), "--state"])
        .arg(&inside)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    let repo = std::fs::canonicalize(&fixture.repo).unwrap();
    assert!(
        err(&refused).contains(&format!(
            "must be outside the repository {}",
            repo.display()
        )) && err(&refused).contains("state-inside"),
        "{}",
        err(&refused)
    );
}

/// `af task list` prints a header row and one aligned row per Task, and a delivery only once
/// there is one: never `not-delivered`.
#[test]
fn task_list_prints_a_header_and_one_row_per_task() {
    let fixture = fixture("pagination");
    let file = fixture.task_file("ticket.json", |_| {});
    fixture.json(&["task", "plan", "--file", file.to_str().unwrap()]);
    let list = out(&fixture.af(&["task", "list"]));
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 2, "{list}");
    let header: Vec<&str> = lines[0].split_whitespace().collect();
    assert_eq!(
        header,
        ["TASK", "STATE", "ROUND", "TOKENS", "AGE", "NEXT"],
        "{list}"
    );
    let row: Vec<&str> = lines[1].split_whitespace().collect();
    assert_eq!(row[..4], ["pagination-cli", "planned", "-", "0"], "{list}");
    assert!(row[4].ends_with('s'), "{list}");
    assert_eq!(row[5], "run", "{list}");
    assert_eq!(
        lines[1].find("planned"),
        lines[0].find("STATE"),
        "aligned:\n{list}"
    );
    assert!(!list.contains("not-delivered"), "{list}");
}

/// The state section of `docs/tasks.md` says what `af help task-states` says, in the same words:
/// every phase, outcome and cause with its meaning.
#[test]
fn the_documentation_states_every_phase_outcome_and_cause_as_the_binary_does() {
    use review_core::task::state::*;
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let docs = std::fs::read_to_string(workspace.join("docs/tasks.md")).unwrap();
    for phase in TaskStatePhaseV1::ALL {
        let row = format!("| `{}` | {} |", phase.as_str(), phase.description());
        assert!(docs.contains(&row), "{row}");
    }
    for outcome in TaskOutcomeV1::ALL {
        let row = format!(
            "| `{}` | {} | {} |",
            outcome.as_str(),
            outcome.exit_code(),
            outcome.description()
        );
        assert!(docs.contains(&row), "{row}");
    }
    for cause in TaskCauseV1::ALL {
        let outcome = cause
            .outcome()
            .map_or("the one it retained".to_owned(), |o| {
                format!("`{}`", o.as_str())
            });
        let row = format!(
            "| `{}` | {outcome} | {} |",
            cause.as_str(),
            cause.description()
        );
        assert!(docs.contains(&row), "{row}");
    }
}
