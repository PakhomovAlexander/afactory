//! The agent loop's verbs end to end (ADR-0211), over Stores the CLI fixture pipelines build:
//! `af task findings` over a reviewed Task whose reviewer reports two findings; `af task
//! materialize` and `af task diff` over a candidate that adds, deletes and changes the mode of
//! files; delivering a verified verification Task of another Task's retained candidate; `af task
//! report --pr-body` through `scripts/check-pr-report.py`; and one background run that `af task
//! run --detach` starts, `af task status`, `af task wait` and `af task events` follow, and `af
//! task cancel` stops, after which a foreground run resumes it and prints its progress lines and
//! last line.

use review_store::{Cas, EventStore};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use crate::{schemas, task_cli, task_gc, task_interrupt};

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn af(repo: &Path, state: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .current_dir(repo)
        .args(args)
        .arg("--state")
        .arg(state)
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// stdout of a command that must exit `code`.
fn expect(output: &Output, code: i32) -> String {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}\n{}",
        out(output),
        err(output)
    );
    out(output)
}

/// The one JSON document of a command that must exit `code`, valid against `schema` when named.
fn document(output: &Output, code: i32, schema: Option<&str>) -> Value {
    let text = expect(output, code);
    let value: Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not one document: {text}"));
    if let Some(schema) = schema {
        schemas::valid(&schemas::validator(schema), &value);
    }
    value
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Pin every package of the catalog at `repo` from its tree, and commit.
fn pin_and_commit(repo: &Path, message: &str) {
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        let package = repo.join(pin["path"].as_str().unwrap());
        pin["digest"] =
            toml::Value::String(review_config::lock::package_digest(name, &package).unwrap());
    }
    std::fs::write(&path, toml::to_string(&catalog).unwrap()).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", message]);
}

/// A read-only verification Pipeline of kind `verification`: the gate and the evaluator over
/// the bound source, whose Snapshot is the source itself.
const VERIFY_PIPELINE: &str = r#"schema = "af.pipeline/1"
name = "fixture/verify"
version = "1.0.0"
max_attempts = 2
max_parallel = 1

[contract.inputs.requirements]
artifact_type = "af/Requirements@1"
cardinality = "one"
optional = false
covers = []

[contract.inputs.requirements.affinity]
kind = "unbound"

[contract.inputs.source]
artifact_type = "af/SourceTree@1"
cardinality = "one"
optional = false
covers = []

[contract.inputs.source.affinity]
kind = "unbound"

[contract.outputs.snapshot]
artifact_type = "af/SourceTree@1"
cardinality = "one"
optional = false
covers = []

[contract.outputs.snapshot.affinity]
kind = "same_as"
input = "source"

[contract.outputs.verification]
artifact_type = "af/VerificationResult@1"
cardinality = "one"
optional = false
covers = ["verified"]

[contract.outputs.verification.affinity]
kind = "same_as"
input = "source"

[accepts]
kinds = ["verification"]

[accepts.required_facts]

[slots.evaluator]
worker = "fixture/evaluator"
role = "evaluate"
input_type = "af/EvaluationInput@1"
output_type = "af/TaskEvaluation@1"
min_attempts = 1
max_attempts = 1
allow_local_replacement = true
independent_from = []

[[nodes]]
id = "check"

[nodes.operator]
op = "check"
checks = ["pagination"]

[nodes.inputs.source]
kind = "input"
port = "source"

[[nodes]]
id = "evaluate"

[nodes.operator]
op = "verify"
slot = "evaluator"

[nodes.inputs.checks]
kind = "node"
node = "check"
port = "result"

[nodes.inputs.requirements]
kind = "input"
port = "requirements"

[nodes.inputs.source]
kind = "input"
port = "source"

[nodes.when]
node = "check"
outcome = "passed"

[[nodes]]
id = "accept"

[nodes.operator]
op = "accept"

[nodes.inputs.checks]
kind = "node"
node = "check"
port = "result"

[nodes.inputs.evaluation]
kind = "node"
node = "evaluate"
port = "result"

[nodes.inputs.source]
kind = "input"
port = "source"

[outputs.snapshot]
kind = "node"
node = "accept"
port = "snapshot"

[outputs.verification]
kind = "node"
node = "accept"
port = "result"

[coverage.verified]
kind = "node"
node = "accept"
port = "result"
"#;

/// `bound-inputs` with a `bugs` reviewer that reports a major and a minor finding under a
/// `blocker` gate, so the round completes and `chain-base` verifies; an implementer that also
/// adds `added.txt`, deletes `README.md` and makes `successor-history.json` executable; and the
/// `fixture/verify` Pipeline with the `verification` kind, all in one commit.
fn reviewed(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, state) = task_gc::fixture(root);
    let bugs = repo.join(".af/task-packages/fixture/bugs/worker.py");
    let script = std::fs::read_to_string(&bugs).unwrap();
    let reports = "'reports':[{'severity':'major','file':'pagination.py','line':1,\
        'title':'Reject negative offset','body':'A negative offset slices from the end.',\
        'fix':'Raise ValueError for a negative offset.','confidence':0.99},\
        {'severity':'minor','file':'pagination.py','line':2,'title':'Name the default limit',\
        'body':'The default limit 2 is unexplained.','fix':'Name it.','confidence':0.9}]";
    assert!(script.contains("'reports':[]"), "{script}");
    std::fs::write(&bugs, script.replace("'reports':[]", reports)).unwrap();
    let implementer = repo.join(".af/task-packages/fixture/implementer/worker.py");
    let script = std::fs::read_to_string(&implementer).unwrap();
    let marker = "print(json.dumps(";
    assert_eq!(script.matches(marker).count(), 1, "{script}");
    let changes = "import os\nopen('added.txt','w').write('added\\n')\nos.remove('README.md')\n\
                   os.chmod('successor-history.json', 0o755)\n";
    std::fs::write(
        &implementer,
        script.replacen(marker, &format!("{changes}{marker}"), 1),
    )
    .unwrap();
    let verify = repo.join(".af/task-packages/fixture/verify");
    std::fs::create_dir_all(&verify).unwrap();
    std::fs::write(verify.join("pipeline.toml"), VERIFY_PIPELINE).unwrap();
    let kind = repo.join(".af/task-packages/fixture/verification-kind");
    std::fs::create_dir_all(&kind).unwrap();
    std::fs::write(
        kind.join("kind.toml"),
        "schema = \"af.task-kind/1\"\nname = \"fixture/verification-kind\"\nversion = \
         \"1.0.0\"\nkind = \"verification\"\nprofile = \"implementation\"\n",
    )
    .unwrap();
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    catalog["review"]["gate"] = toml::Value::String("blocker".into());
    for (name, place) in [
        ("fixture/verify", ".af/task-packages/fixture/verify"),
        (
            "fixture/verification-kind",
            ".af/task-packages/fixture/verification-kind",
        ),
    ] {
        let mut pin = toml::map::Map::new();
        pin.insert("version".into(), toml::Value::String("1.0.0".into()));
        pin.insert("digest".into(), toml::Value::String(String::new()));
        pin.insert("path".into(), toml::Value::String(place.into()));
        catalog["packages"]
            .as_table_mut()
            .unwrap()
            .insert(name.into(), toml::Value::Table(pin));
    }
    let mut kinds = toml::map::Map::new();
    kinds.insert(
        "verification".into(),
        toml::Value::String("fixture/verification-kind".into()),
    );
    catalog
        .as_table_mut()
        .unwrap()
        .insert("kinds".into(), toml::Value::Table(kinds));
    std::fs::write(&path, toml::to_string(&catalog).unwrap()).unwrap();
    let file = serde_json::json!({"schema":"af.task-file/1","task_id":"chain-verify",
        "kind":"verification","goal":"Verify the reviewed candidate read-only",
        "inputs":{"source":{"task":"chain-base","port":"snapshot"}},
        "pipeline":{"name":"fixture/verify","fallback":"refuse"},"strategy":"small","facts":{},
        "limits":{"tokens":1000,"max_attempts":3,"wall_ms":60000,
            "verification":{"tokens":200,"attempts":2,"wall_ms":10000}}});
    std::fs::write(
        repo.join("verify.json"),
        serde_json::to_vec_pretty(&file).unwrap(),
    )
    .unwrap();
    pin_and_commit(&repo, "agent loop fixture");
    (repo, state)
}

/// A linked worktree of `repo` at its HEAD, for `materialize` to write into.
fn worktree(repo: &Path, path: &Path) -> PathBuf {
    git(
        repo,
        &["worktree", "add", "-q", "--detach", path.to_str().unwrap()],
    );
    std::fs::canonicalize(path).unwrap()
}

/// Replace every Snapshot or artifact ID in `text` with `sha256:…`, for a stable snapshot.
fn ids_elided(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("sha256:") {
        out.push_str(&rest[..at]);
        out.push_str("sha256:…");
        let tail = &rest[at + 7..];
        let hex = tail.bytes().take_while(u8::is_ascii_hexdigit).count();
        rest = &tail[hex..];
    }
    out.push_str(rest);
    out
}

/// Item 1: per reviewer each finding with its severity, confidence, title, location, evidence
/// and fix, grouped by severity, then the evaluator's verdict; one `af/task-findings@2` under
/// `--json`; a Task without a review round says so with its state; a round it did not record is
/// refused by its number.
#[test]
fn findings_print_each_reviewers_findings_by_severity_and_the_evaluators_verdict() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = reviewed(root.path());
    task_gc::start(&repo, &state, "base.json");
    let initial = expect(&af(&repo, &state, &["task", "status", "chain-base"]), 0);
    assert!(!initial.contains("last Gate here:"), "{initial}");
    task_gc::start(&repo, &state, "gc-older.json");
    let history = expect(&af(&repo, &state, &["task", "status", "gc-older"]), 0);
    assert!(history.contains("last Gate here:"), "{history}");
    let previous = document(
        &af(&repo, &state, &["task", "report", "chain-base", "--json"]),
        0,
        Some("task-report-v3.json"),
    );
    let recorded_check = previous["tasks"][0]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|node| node["checks"].as_array().unwrap())
        .any(|check| {
            let Some(ms) = check["elapsed_ms"].as_u64() else {
                return false;
            };
            let seconds = ms.saturating_add(500) / 1_000;
            let duration = match seconds {
                0..60 => format!("{seconds} s"),
                60..3_600 => format!("{} min", seconds / 60),
                _ if seconds % 3_600 / 60 == 0 => format!("{} h", seconds / 3_600),
                _ => format!("{} h {} min", seconds / 3_600, seconds % 3_600 / 60),
            };
            history.contains(&format!("{}: {duration}", check["name"].as_str().unwrap()))
        });
    assert!(
        recorded_check,
        "the hint must use a recorded check span: {history}"
    );
    let plan = expect(
        &af(&repo, &state, &["task", "plan", "--file", "gc-newer.json"]),
        0,
    );
    assert!(plan.contains("last Gate here:"), "{plan}");

    let text = expect(&af(&repo, &state, &["task", "findings", "chain-base"]), 0);
    let expected = "\
Task chain-base: finished/verified; review round 1 of 1 (pass) on sha256:…
  bugs: 1 major, 1 minor
  correctness: no findings
  coverage: every reviewer ran (2 reviewers); 4 paths reviewed

major
  [bugs] Reject negative offset, confidence 0.99, open
    at pagination.py:1
    > A negative offset slices from the end.
    fix: Raise ValueError for a negative offset.

minor
  [bugs] Name the default limit, confidence 0.90, open
    at pagination.py:2
    > The default limit 2 is unexplained.
    fix: Name it.

evaluator (evaluate_goal): passed
  Verified offset and limit on the sealed source
next: af task deliver chain-base --branch af/chain-base --worktree ../chain-base --confirm chain-base
";
    // The next command carries the `--state` the command was given.
    let expected = expected.replace(
        "--confirm chain-base\n",
        &format!("--confirm chain-base --state {}\n", state.display()),
    );
    assert_eq!(ids_elided(&text), expected);

    let value = document(
        &af(&repo, &state, &["task", "findings", "chain-base", "--json"]),
        0,
        Some("task-findings-v2.json"),
    );
    assert_eq!(value["schema"], "af/task-findings@2");
    assert_eq!(value["rounds"], 1);
    let round = &value["round"];
    assert_eq!(
        round["counts"],
        serde_json::json!({"blocker":0,"major":1,"minor":1})
    );
    let bugs = &round["reviewers"][0];
    assert_eq!(bugs["reviewer"], "bugs");
    assert_eq!(bugs["findings"].as_array().unwrap().len(), 2);
    assert_eq!(
        bugs["findings"][0]["evidence"],
        "A negative offset slices from the end."
    );
    assert_eq!(
        bugs["findings"][0]["fix"],
        "Raise ValueError for a negative offset."
    );
    assert_eq!(round["reviewers"][1]["findings"], serde_json::json!([]));
    // Item 1 of ADR-0248: each finding is handed over as a prompt, on the reviewed Snapshot.
    let prompt = &bugs["findings"][0]["agent_prompt"];
    assert_eq!(prompt["location"]["file"], "pagination.py");
    assert_eq!(prompt["location"]["line"], 1);
    assert_eq!(prompt["location"]["snapshot_id"], round["snapshot_id"]);
    assert_eq!(prompt["evidence"], "A negative offset slices from the end.");
    assert_eq!(
        prompt["requested_change"],
        "Raise ValueError for a negative offset."
    );
    assert!(
        prompt["verification"]
            .as_str()
            .unwrap()
            .contains("af/TaskFixReceipt@1"),
        "{prompt}"
    );
    assert!(
        prompt["text"]
            .as_str()
            .unwrap()
            .contains("Location: pagination.py:1 on Snapshot sha256:"),
        "{prompt}"
    );
    // Item 2: both reviewers ran, and every path the round changed is reviewed.
    let coverage = &round["coverage"];
    assert_eq!(
        coverage["reviewers"],
        serde_json::json!([
            {"reviewer": "bugs", "state": "reviewed", "reason": null},
            {"reviewer": "correctness", "state": "reviewed", "reason": null},
        ])
    );
    let paths = coverage["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 4, "{coverage}");
    assert!(
        paths.iter().all(|path| path["state"] == "reviewed"),
        "{coverage}"
    );
    assert!(
        paths.iter().any(|path| path["path"] == "pagination.py"),
        "{coverage}"
    );
    // Item 4: nothing is verified fixed without a verification record.
    assert_eq!(round["verified_fixed"], serde_json::json!([]));
    assert_eq!(value["evaluation"]["outcome"], "passed");
    let typed: review_core::task::agent_loop::TaskFindingsV2 =
        serde_json::from_value(value.clone()).unwrap();
    typed.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        value,
        "the document is the typed contract"
    );
    let first = document(
        &af(
            &repo,
            &state,
            &["task", "findings", "chain-base", "--round", "1", "--json"],
        ),
        0,
        None,
    );
    assert_eq!(first, value);

    // Item 3: the same findings as one SARIF 2.1.0 log, with its required structure.
    let sarif = document(
        &af(
            &repo,
            &state,
            &["task", "findings", "chain-base", "--format", "sarif"],
        ),
        0,
        Some("findings-sarif-v1.json"),
    );
    assert_eq!(sarif["version"], "2.1.0");
    assert!(
        sarif["$schema"]
            .as_str()
            .unwrap()
            .ends_with("sarif-schema-2.1.0.json")
    );
    let run = &sarif["runs"][0];
    assert_eq!(run["tool"]["driver"]["name"], "af");
    assert_eq!(run["properties"]["schema"], "af/findings-sarif@1");
    assert_eq!(run["properties"]["task_id"], "chain-base");
    assert_eq!(run["invocations"][0]["executionSuccessful"], true);
    let results = run["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for (result, level, line) in [(&results[0], "warning", 1), (&results[1], "note", 2)] {
        assert_eq!(result["ruleId"], "af-review/bugs");
        assert_eq!(result["level"], level);
        assert!(
            result["message"]["text"]
                .as_str()
                .unwrap()
                .contains("Requested change:")
        );
        let location = &result["locations"][0]["physicalLocation"];
        assert_eq!(location["artifactLocation"]["uri"], "pagination.py");
        assert_eq!(location["region"]["startLine"], line);
    }
    let both = af(
        &repo,
        &state,
        &[
            "task",
            "findings",
            "chain-base",
            "--json",
            "--format",
            "sarif",
        ],
    );
    assert_eq!(both.status.code(), Some(2), "{}", err(&both));

    // A Task whose Pipeline has no review says so, with the state it reached.
    let plain = expect(&af(&repo, &state, &["task", "findings", "gc-older"]), 0);
    assert!(
        plain.starts_with(
            "Task gc-older: finished/verified; it recorded no review round, so there are no \
             review findings\n"
        ),
        "{plain}"
    );
    let none = document(
        &af(&repo, &state, &["task", "findings", "gc-older", "--json"]),
        0,
        Some("task-findings-v2.json"),
    );
    assert_eq!(
        (none["rounds"].clone(), none["round"].clone()),
        (0.into(), Value::Null)
    );

    // A round the Task did not record is refused by its number, with what to name instead.
    let missing = af(
        &repo,
        &state,
        &["task", "findings", "chain-base", "--round", "2"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        err(&missing).contains("recorded 1 review round, so --round 2"),
        "{}",
        err(&missing)
    );
    let missing = af(
        &repo,
        &state,
        &["task", "findings", "chain-base", "--round", "2", "--json"],
    );
    let error = document(&missing, 1, None);
    assert_eq!(error["schema"], "af/error@2");
    let unknown = af(&repo, &state, &["task", "findings", "nope"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        err(&unknown).contains("Task nope not found"),
        "{}",
        err(&unknown)
    );
}

/// Items 2, 3 and 7: the candidate is materialized into a worktree, where `git status` shows
/// exactly its additions, deletions and mode change; `af task diff` equals `git diff` of that
/// tree; a dirty worktree is refused unless `--force`; and the verification Task of the
/// retained candidate delivers it, the delivery recording both Tasks.
#[test]
fn a_candidate_is_materialized_diffed_and_delivered_through_its_verification() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = reviewed(root.path());
    task_gc::start(&repo, &state, "base.json");

    // Materialize into a clean worktree at the source commit.
    let into = worktree(&repo, &root.path().join("review-wt"));
    let materialized = document(
        &af(
            &repo,
            &state,
            &[
                "task",
                "materialize",
                "chain-base",
                "--into",
                into.to_str().unwrap(),
                "--json",
            ],
        ),
        0,
        Some("task-materialized-v1.json"),
    );
    assert_eq!(materialized["verified"], true);
    assert_eq!(
        (
            materialized["written"].clone(),
            materialized["deleted"].clone(),
            materialized["mode"].clone()
        ),
        (
            serde_json::json!(["added.txt", "pagination.py"]),
            serde_json::json!(["README.md"]),
            serde_json::json!(["successor-history.json"])
        ),
        "{materialized:#}"
    );
    let status = git(&into, &["status", "--porcelain=v1"]);
    let mut lines: Vec<&str> = status.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            " D README.md",
            " M pagination.py",
            " M successor-history.json",
            "?? added.txt"
        ],
        "{status}"
    );
    assert_eq!(
        git(&into, &["diff", "--summary", "successor-history.json"]).trim(),
        "mode change 100644 => 100755 successor-history.json"
    );

    // The diff of the candidate equals `git diff` of the materialized tree.
    git(&into, &["add", "-A"]);
    let expected = git(&into, &["diff", "--cached", "-M"]);
    let diff = expect(&af(&repo, &state, &["task", "diff", "chain-base"]), 0);
    assert_eq!(diff, expected);
    assert_eq!(
        expect(
            &af(&repo, &state, &["task", "diff", "chain-base", "--stat"]),
            0
        ),
        git(&into, &["diff", "--cached", "-M", "--stat"])
    );
    assert_eq!(
        expect(
            &af(
                &repo,
                &state,
                &["task", "diff", "chain-base", "--name-only"]
            ),
            0
        ),
        git(&into, &["diff", "--cached", "-M", "--name-only"])
    );
    let json = document(
        &af(&repo, &state, &["task", "diff", "chain-base", "--json"]),
        0,
        Some("task-diff-v1.json"),
    );
    assert_eq!(json["patch"], expected);
    let statuses: Vec<(String, String)> = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().into(),
                f["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    for (path, status) in [
        ("README.md", "deleted"),
        ("added.txt", "added"),
        ("pagination.py", "modified"),
        ("successor-history.json", "modified"),
    ] {
        assert!(
            statuses.contains(&(path.into(), status.into())),
            "{path} {status}: {statuses:?}"
        );
    }
    // Nothing the diff ran touched a worktree.
    assert!(git(&repo, &["status", "--porcelain"]).is_empty());

    // A worktree with uncommitted changes is refused, and --force overwrites them.
    git(&into, &["reset", "-q", "--hard"]);
    git(&into, &["clean", "-qfd"]);
    std::fs::write(into.join("pagination.py"), "local edit\n").unwrap();
    let dirty = af(
        &repo,
        &state,
        &[
            "task",
            "materialize",
            "chain-base",
            "--into",
            into.to_str().unwrap(),
        ],
    );
    assert_eq!(dirty.status.code(), Some(1));
    assert!(
        err(&dirty).contains("has uncommitted changes"),
        "{}",
        err(&dirty)
    );
    assert!(err(&dirty).contains("--force"), "{}", err(&dirty));
    assert_eq!(
        std::fs::read_to_string(into.join("pagination.py")).unwrap(),
        "local edit\n"
    );
    let forced = expect(
        &af(
            &repo,
            &state,
            &[
                "task",
                "materialize",
                "chain-base",
                "--into",
                into.to_str().unwrap(),
                "--force",
            ],
        ),
        0,
    );
    assert!(forced.contains("written 2, deleted 1, mode 1"), "{forced}");
    assert!(
        forced.contains(&format!("next: git -C {} status", into.display())),
        "{forced}"
    );
    assert!(forced.contains("af task report chain-base"), "{forced}");

    // A Snapshot the Task never recorded is refused, naming the ones it did.
    let other = format!("sha256:{}", "0".repeat(64));
    let named = af(
        &repo,
        &state,
        &["task", "diff", "chain-base", "--snapshot", &other],
    );
    assert_eq!(named.status.code(), Some(1));
    assert!(
        err(&named).contains("recorded no Snapshot"),
        "{}",
        err(&named)
    );

    // The verification Task binds the retained candidate, verifies it read-only and delivers
    // it on chain-base's committed source; the record names both Tasks.
    let verify = document(
        &af(
            &repo,
            &state,
            &[
                "task",
                "start",
                "--execute",
                "--file",
                "verify.json",
                "--json",
            ],
        ),
        0,
        None,
    );
    assert_eq!(verify["result"]["acceptance"], "satisfied", "{verify:#}");
    let delivered = root.path().join("delivered");
    let receipt = document(
        &af(
            &repo,
            &state,
            &[
                "task",
                "deliver",
                "chain-verify",
                "--branch",
                "af/chain-verify",
                "--worktree",
                delivered.to_str().unwrap(),
                "--confirm",
                "chain-verify",
                "--json",
            ],
        ),
        0,
        None,
    );
    assert_eq!(receipt["outcome"]["kind"], "delivered", "{receipt:#}");
    assert_eq!(receipt["task_id"], "chain-verify");
    assert!(!delivered.join("README.md").exists());
    assert_eq!(
        std::fs::read_to_string(delivered.join("added.txt")).unwrap(),
        "added\n"
    );
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task = store
        .task_projection(&cas, "chain-verify")
        .unwrap()
        .unwrap();
    let (record_id, record) = task.deliveries.last().unwrap();
    assert_eq!(
        record.status,
        review_core::task::delivery::TaskDeliveryStatusV1::Delivered
    );
    let envelope = cas.get_artifact(record_id).unwrap();
    let proofs: Vec<review_core::task::delivery::TaskDeliveryProofV1> = envelope
        .input_artifacts
        .iter()
        .filter_map(|id| cas.get_optional_artifact(id).ok().flatten())
        .filter(|artifact| artifact.artifact_type == "af/TaskDeliveryProof@1")
        .map(|artifact| serde_json::from_value(artifact.payload).unwrap())
        .collect();
    assert_eq!(proofs.len(), 1, "{envelope:?}");
    assert_eq!(proofs[0].verification_task_id, "chain-verify");
    assert_eq!(proofs[0].candidate_task_id, "chain-base");
    let base = store.task_projection(&cas, "chain-base").unwrap().unwrap();
    assert_eq!(
        Some(&proofs[0].candidate_source_snapshot_id),
        base.revision.inputs["source"].snapshot_id.as_ref()
    );
    let proof_value = serde_json::to_value(&proofs[0]).unwrap();
    schemas::valid(
        &schemas::validator("task-delivery-proof-v1.json"),
        &proof_value,
    );
    drop(store);
    // The delivered tree is the materialized one: the candidate, on chain-base's source commit.
    assert_eq!(
        git(&delivered, &["rev-parse", "HEAD"]),
        git(&repo, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        std::fs::read(delivered.join("pagination.py")).unwrap(),
        std::fs::read(into.join("pagination.py")).unwrap()
    );
    let text = expect(&af(&repo, &state, &["task", "show", "chain-verify"]), 0);
    assert!(
        text.starts_with("Task chain-verify: finished/verified"),
        "{text}"
    );
}

/// Item 2's refusal of an unverified candidate, and `af task deliver` naming `materialize
/// --allow-unverified` in its own refusal: a candidate whose gate failed.
#[test]
fn an_unverified_candidate_is_refused_unless_allowed_and_deliver_names_the_way() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(root.path(), "pagination");
    let implementer = repo.join(".af/task-packages/fixture/implementer/worker.py");
    std::fs::write(
        &implementer,
        "import json,sys\njson.load(sys.stdin)\nopen('notes.txt','w').write('unfinished\\n')\n\
         print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Left \
         pagination unfinished'}]}}))\n",
    )
    .unwrap();
    pin_and_commit(&repo, "unfinished implementer");
    expect(
        &af(&repo, &state, &["task", "plan", "--file", "ticket.json"]),
        0,
    );
    // --quiet keeps the progress lines back, never the last line.
    let run = af(
        &repo,
        &state,
        &["task", "run", "pagination-cli", "--execute", "--quiet"],
    );
    assert_eq!(run.status.code(), Some(5), "{}\n{}", out(&run), err(&run));
    assert!(
        !err(&run).contains("af task run: implement"),
        "{}",
        err(&run)
    );
    assert!(
        err(&run)
            .lines()
            .last()
            .unwrap()
            .starts_with("Task pagination-cli: finished/gate_failed/"),
        "{}",
        err(&run)
    );
    let into = worktree(&repo, &root.path().join("wt"));
    let refused = af(
        &repo,
        &state,
        &[
            "task",
            "materialize",
            "pagination-cli",
            "--into",
            into.to_str().unwrap(),
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    let said = err(&refused);
    assert!(said.contains("is not its verified result"), "{said}");
    assert!(said.contains("--allow-unverified"), "{said}");
    assert!(!into.join("notes.txt").exists(), "a refusal wrote nothing");
    let allowed = af(
        &repo,
        &state,
        &[
            "task",
            "materialize",
            "pagination-cli",
            "--into",
            into.to_str().unwrap(),
            "--allow-unverified",
        ],
    );
    let text = expect(&allowed, 0);
    assert!(err(&allowed).contains("unverified candidate: review it before merging"));
    assert!(
        text.contains("an unverified candidate of Task pagination-cli"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(into.join("notes.txt")).unwrap(),
        "unfinished\n"
    );
    let deliver = af(
        &repo,
        &state,
        &[
            "task",
            "deliver",
            "pagination-cli",
            "--branch",
            "af/refused",
            "--worktree",
            root.path().join("refused").to_str().unwrap(),
            "--confirm",
            "pagination-cli",
        ],
    );
    assert_eq!(deliver.status.code(), Some(1));
    assert!(
        err(&deliver).contains("only a verified Task can be delivered"),
        "{}",
        err(&deliver)
    );
    assert!(
        err(&deliver).contains("af task materialize pagination-cli --into DIR --allow-unverified"),
        "{}",
        err(&deliver)
    );
}

/// Item 6: `--pr-body` prints the template with the report and the lines added block spliced
/// in, and `scripts/check-pr-report.py` accepts it as it is.
#[test]
fn the_pr_body_is_the_template_with_both_blocks_and_the_checker_accepts_it() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = reviewed(root.path());
    task_gc::start(&repo, &state, "base.json");
    task_gc::start(&repo, &state, "gc-older.json");
    std::fs::create_dir_all(repo.join(".github")).unwrap();
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    std::fs::copy(
        workspace().join(".github/PULL_REQUEST_TEMPLATE.md"),
        repo.join(".github/PULL_REQUEST_TEMPLATE.md"),
    )
    .unwrap();
    std::fs::copy(
        workspace().join("scripts/pr-lines.py"),
        repo.join("scripts/pr-lines.py"),
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "template and lines script"]);
    // Without origin/main the script cannot count, and no body is printed: a body without its
    // lines block is one the pull request check refuses.
    let refused = af(
        &repo,
        &state,
        &["task", "report", "chain-base", "gc-older", "--pr-body"],
    );
    assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));
    assert!(out(&refused).is_empty(), "{}", out(&refused));
    let said = err(&refused);
    assert!(said.contains("--base REF"), "{said}");
    assert!(
        said.contains("af task report chain-base gc-older --pr-body"),
        "the command to run again, with a base: {said}"
    );
    for line in said.lines() {
        assert!(crate::common::fits_or_unbreakable(line, 100), "{line}");
    }
    // GA final fix item 3: a script that prints only the begin marker gets no body either; the
    // refusal names the end marker it lacks.
    // The copy keeps the workspace file's mode, read-only in a sealed gate tree: replace it.
    std::fs::remove_file(repo.join("scripts/pr-lines.py")).unwrap();
    std::fs::write(
        repo.join("scripts/pr-lines.py"),
        "print('<!-- af-pr-lines:v1 -->')\n",
    )
    .unwrap();
    let begun = af(
        &repo,
        &state,
        &["task", "report", "chain-base", "gc-older", "--pr-body"],
    );
    assert_eq!(begun.status.code(), Some(1), "{}", err(&begun));
    assert!(out(&begun).is_empty(), "{}", out(&begun));
    let said = err(&begun).replace('\n', " ");
    assert!(
        said.contains("printed an incomplete lines added block")
            && said.contains("no end marker `<!-- /af-pr-lines -->` after it"),
        "{said}"
    );
    assert!(
        said.contains("python3 -I scripts/pr-lines.py --repo ."),
        "{said}"
    );
    std::fs::copy(
        workspace().join("scripts/pr-lines.py"),
        repo.join("scripts/pr-lines.py"),
    )
    .unwrap();
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    let body = expect(
        &af(
            &repo,
            &state,
            &["task", "report", "chain-base", "gc-older", "--pr-body"],
        ),
        0,
    );
    assert!(body.starts_with("## What\n"), "{body}");
    let receipt = body.find("#### Receipt").unwrap();
    let pipeline = body.find("**fixture/").unwrap();
    let rounds = body.find("| Round |").unwrap();
    assert!(receipt < pipeline && pipeline < rounds, "{body}");
    for field in [
        "Outcome:",
        "Checks:",
        "Unresolved findings:",
        "Tokens:",
        "Active time:",
    ] {
        assert!(body.contains(field), "{field}: {body}");
    }
    assert!(
        body.contains("Unresolved findings: 0 blocker, 1 major, 1 minor"),
        "{body}"
    );
    assert!(
        body.contains("<!-- af-pr-lines:v1 -->\n**Lines added: "),
        "{body}"
    );
    assert!(body.contains("## Checklist"), "{body}");
    // The base named on the command line is the script's.
    let based = expect(
        &af(
            &repo,
            &state,
            &[
                "task",
                "report",
                "chain-base",
                "gc-older",
                "--pr-body",
                "--base",
                "origin/main",
            ],
        ),
        0,
    );
    assert_eq!(based, body);
    let file = root.path().join("body.md");
    std::fs::write(&file, &body).unwrap();
    let check = Command::new("python3")
        .arg("-I")
        .arg(workspace().join("scripts/check-pr-report.py"))
        .arg("--body")
        .arg(&file)
        .arg("--repo")
        .arg(&repo)
        .arg("--diff")
        .arg("origin/main")
        .arg("HEAD")
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}\n{}\n{body}",
        out(&check),
        err(&check)
    );

    // GA second fix item 1: a script that prints the title without its table gets no body, and
    // the checker refuses the same block in a description.
    std::fs::remove_file(repo.join("scripts/pr-lines.py")).unwrap();
    std::fs::write(
        repo.join("scripts/pr-lines.py"),
        "print('<!-- af-pr-lines:v1 -->')\nprint('**Lines added: 1**')\n\
         print('<!-- /af-pr-lines -->')\n",
    )
    .unwrap();
    let titled = af(
        &repo,
        &state,
        &["task", "report", "chain-base", "gc-older", "--pr-body"],
    );
    assert_eq!(titled.status.code(), Some(1), "{}", err(&titled));
    assert!(out(&titled).is_empty(), "{}", out(&titled));
    let said = err(&titled).replace('\n', " ");
    assert!(
        said.contains("its block's title is not followed by the table"),
        "{said}"
    );
    let begin = body.find("<!-- af-pr-lines:v1 -->").unwrap();
    let end = body.find("<!-- /af-pr-lines -->").unwrap();
    let title_only = format!(
        "{}<!-- af-pr-lines:v1 -->\n**Lines added: 1**\n{}",
        &body[..begin],
        &body[end..]
    );
    std::fs::write(&file, &title_only).unwrap();
    let checked = checker(&file, &repo, None);
    assert!(!checked.status.success(), "{}", out(&checked));
    assert!(
        err(&checked).contains("the lines added block is not what `scripts/pr-lines.py` prints"),
        "{}",
        err(&checked)
    );

    // A branch that adds no line: the script's `**Lines added: none**` alone, which the checker
    // accepts as the block of the pull request's commits.
    std::fs::remove_file(repo.join("scripts/pr-lines.py")).unwrap();
    std::fs::copy(
        workspace().join("scripts/pr-lines.py"),
        repo.join("scripts/pr-lines.py"),
    )
    .unwrap();
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let none = expect(
        &af(
            &repo,
            &state,
            &["task", "report", "chain-base", "gc-older", "--pr-body"],
        ),
        0,
    );
    assert!(
        none.contains("<!-- af-pr-lines:v1 -->\n**Lines added: none**\n<!-- /af-pr-lines -->\n"),
        "{none}"
    );
    std::fs::write(&file, &none).unwrap();
    let checked = checker(&file, &repo, Some(("origin/main", "HEAD")));
    assert!(
        checked.status.success(),
        "{}\n{}\n{none}",
        out(&checked),
        err(&checked)
    );
    // A template of one's own takes the block where its markers are.
    let template = root.path().join("template.md");
    std::fs::write(
        &template,
        "Intro\n\n<!-- af-task-report:v1 -->\n<!-- /af-task-report -->\n\n<!-- af-pr-lines:v1 \
         -->\n<!-- /af-pr-lines -->\n",
    )
    .unwrap();
    let own = expect(
        &af(
            &repo,
            &state,
            &[
                "task",
                "report",
                "chain-base",
                "--pr-body",
                "--template",
                template.to_str().unwrap(),
            ],
        ),
        0,
    );
    assert!(
        own.starts_with("Intro\n\n<!-- af-task-report:v1 -->\n### af task report"),
        "{own}"
    );
    let conflict = af(
        &repo,
        &state,
        &["task", "report", "chain-base", "--pr-body", "--json"],
    );
    assert_eq!(conflict.status.code(), Some(2));
}

/// `scripts/check-pr-report.py --body FILE` of this workspace, counting the lines added in
/// `repo` from `diff` (BASE, HEAD), or checking the block's shape alone without one.
fn checker(file: &Path, repo: &Path, diff: Option<(&str, &str)>) -> std::process::Output {
    let mut command = Command::new("python3");
    command
        .arg("-I")
        .arg(workspace().join("scripts/check-pr-report.py"))
        .arg("--body")
        .arg(file)
        .arg("--repo")
        .arg(repo);
    match diff {
        Some((base, head)) => command.arg("--diff").arg(base).arg(head),
        None => command.arg("--no-diff"),
    };
    command.output().unwrap()
}

fn alive(pid: u32) -> bool {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        None,
    )
    .is_ok()
}

/// Reports the background run and its Worker if the test fails before `cancel` stopped them.
/// Neither is this test's child, so neither is signalled here: the Worker ends by itself once
/// the run is gone, and the run ends at its Attempt walls (issue #331 item 7).
fn leftovers(pids: &[u32]) -> crate::common::recorded::Leftovers {
    crate::common::recorded::Leftovers(
        pids.iter()
            .filter_map(|pid| crate::common::recorded::Recorded::now(i32::try_from(*pid).ok()?))
            .collect(),
    )
}

/// The output of a follower `af task events` that must end by itself: its stdout (`stdout`, the
/// rest of the pipe the caller took) and its stderr are drained as it runs, so a full pipe never
/// holds it, while `try_wait` is polled. The deadline is a fail-safe, never what a test waits
/// for: past `limit` the follower is killed and reaped and the test fails with what it printed,
/// instead of waiting forever when terminal detection regresses.
fn bounded_follower(
    mut follower: std::process::Child,
    stdout: impl std::io::Read + Send + 'static,
    limit: Duration,
) -> Output {
    fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    }
    let stdout = drain(stdout);
    let stderr = drain(follower.stderr.take().unwrap());
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = follower.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = follower.kill();
            follower.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    let Some(status) = status else {
        panic!(
            "the follower printed no terminal record within {limit:?}; killed:\n{}\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    };
    Output {
        status,
        stdout,
        stderr,
    }
}

/// The lines of `af task events`, each one `af/task-event@1` document, its events numbered
/// without a gap from `first` and at most one terminal record, the last line.
fn event_lines(text: &str, first: u64) -> Vec<Value> {
    let schema = schemas::validator("task-event-v1.json");
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not one line: {line}")))
        .collect();
    for (at, line) in lines.iter().enumerate() {
        schemas::valid(&schema, line);
        assert_eq!(line["schema"], "af/task-event@1", "{line}");
        if line["record"] == "terminal" {
            assert_eq!(
                at + 1,
                lines.len(),
                "the terminal record is the last line:\n{text}"
            );
        } else {
            assert_eq!(
                line["sequence"],
                first + at as u64,
                "a gap or repeat:\n{text}"
            );
        }
    }
    lines
}

/// Items 4, 5 and 8: a detached run returns at once with its pid and log, outlives the shell
/// that started it, logs its progress and heartbeat; `status` names it running at its node,
/// by the run that holds the lease even when a newer run recorded itself;
/// `wait --timeout` gives up with 124; `af task events` follows the running Task as JSON lines
/// with the Store's gap-free sequences, stops at `--timeout` naming the `--after` that continues,
/// ends with exactly one terminal record once the Task settles, and `--after N` returns only the
/// rest (ADR-0250); `cancel` refuses a writer of another host, then stops the
/// recorded run and its Worker, leaving the Task cancelled by its requester and resumable; a
/// foreground run resumes it, printing its node lines and its state as stderr's last line; and
/// `wait` returns the Task's exit code.
#[test]
fn a_detached_run_is_followed_cancelled_and_resumed() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(root.path(), "pagination");
    let ready = root.path().join("worker-ready");
    let resume = root.path().join("worker-resume");
    task_interrupt::long_running_implementer(&repo, &ready, &resume);
    let task = task_interrupt::TASK;
    expect(
        &af(&repo, &state, &["task", "plan", "--file", "ticket.json"]),
        0,
    );

    // Detached through a shell that exits as soon as af returned: the run goes on without it.
    let started = Instant::now();
    let environment = crate::common::af();
    let mut shell = Command::new("/bin/sh");
    for (name, value) in environment.get_envs() {
        if let Some(value) = value {
            shell.env(name, value);
        }
    }
    let detached = shell
        .arg("-c")
        .arg("\"$0\" task run \"$1\" --execute --detach --json --state \"$2\"; exit $?")
        .arg(crate::common::af_binary())
        .arg(task)
        .arg(&state)
        .current_dir(&repo)
        .env("AF_TEST_HEARTBEAT_MS", "300")
        .output()
        .unwrap();
    let value = document(&detached, 0, Some("task-detached-v3.json"));
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "returned at once"
    );
    assert_eq!(value["admitted"], true, "{value:#}");
    assert_eq!(value["next_action"], format!("af task wait {task}"));
    let pid = u32::try_from(value["pid"].as_u64().unwrap()).unwrap();
    let mut leftovers = leftovers(&[pid]);
    let log = PathBuf::from(value["log"].as_str().unwrap());
    assert!(log.ends_with(format!("runs/{task}-1.log")), "{log:?}");
    assert!(log.is_file(), "{log:?}");
    assert_eq!(value["state"]["phase"], "running", "{value:#}");
    let until = Instant::now() + Duration::from_secs(60);
    let worker: Vec<u32> = loop {
        if let Ok(text) = std::fs::read_to_string(&ready) {
            let pids: Vec<u32> = text
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if pids.len() == 2 {
                break pids;
            }
        }
        assert!(
            Instant::now() < until,
            "the Worker did not start: {}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    leftovers.0.extend(crate::common::recorded::Recorded::now(
        i32::try_from(worker[0]).unwrap(),
    ));
    assert!(alive(pid), "the run outlived the shell that started it");
    // A follower of the running Task, ended by the cancel below.
    let mut follower = crate::common::af()
        .current_dir(&repo)
        .args(["task", "events", task, "--state"])
        .arg(&state)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let follower_pid = follower.id();
    leftovers.0.extend(crate::common::recorded::Recorded::now(
        i32::try_from(follower_pid).unwrap(),
    ));

    let status = expect(&af(&repo, &state, &["task", "status", task]), 0);
    assert!(
        status.starts_with(&format!("Task {task}: running, node implement, ")),
        "{status}"
    );
    assert!(status.contains(&format!("pid {pid} on ")), "{status}");
    // A newer run whose lease attempt was refused recorded itself too: the status still names
    // the run that holds the lease.
    let record_path = state.join(format!("runs/{task}-1.json"));
    let mut refused: Value =
        serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
    refused["run"] = 2.into();
    refused["pid"] = std::process::id().into();
    refused["writer"] = "cli-refused".into();
    refused["detached"] = false.into();
    refused["log"] = Value::Null;
    let refused_path = state.join(format!("runs/{task}-2.json"));
    std::fs::write(&refused_path, serde_json::to_vec(&refused).unwrap()).unwrap();
    let held = expect(&af(&repo, &state, &["task", "status", task]), 0);
    assert!(held.contains(&format!("run 1 pid {pid} on ")), "{held}");
    let held = document(
        &af(&repo, &state, &["task", "status", task, "--json"]),
        0,
        Some("task-status-v2.json"),
    );
    assert_eq!(held["run"]["pid"], pid, "{held:#}");
    std::fs::remove_file(&refused_path).unwrap();
    for line in status.lines() {
        assert!(line.chars().count() <= 100, "{line}");
    }
    document(
        &af(&repo, &state, &["task", "status", task, "--json"]),
        0,
        Some("task-status-v2.json"),
    );
    let waited = af(&repo, &state, &["task", "wait", task, "--timeout", "1"]);
    assert_eq!(waited.status.code(), Some(124), "{}", err(&waited));
    assert!(
        err(&waited).contains("still runs after"),
        "{}",
        err(&waited)
    );
    // The stream so far: every recorded event, no terminal record, and the --after that goes on.
    let partial = af(&repo, &state, &["task", "events", task, "--timeout", "1"]);
    assert_eq!(partial.status.code(), Some(124), "{}", err(&partial));
    let so_far = event_lines(&out(&partial), 0);
    assert!(
        so_far.iter().all(|line| line["record"] == "event"),
        "{}",
        out(&partial)
    );
    assert!(
        so_far
            .iter()
            .any(|line| line["change"] == "execution_recorded"
                && line["detail"] == "started"
                && line["node"] == "implement"),
        "{}",
        out(&partial)
    );
    let seen = so_far.last().unwrap()["sequence"].as_u64().unwrap();
    assert!(
        err(&partial).contains(&format!(
            "continue with `af task events {task} --after {seen} --state {}`",
            state.display()
        )),
        "{}",
        err(&partial)
    );

    // The heartbeat reaches the log while the Worker holds.
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("af task run: implement running") {
            assert!(text.contains("af task run: implement started ("), "{text}");
            assert!(text.contains("tokens"), "{text}");
            break;
        }
        assert!(Instant::now() < until, "no heartbeat in the log:\n{text}");
        std::thread::sleep(Duration::from_millis(50));
    }

    // A writer the records place on another host is refused with its pid and host.
    let record = std::fs::read_to_string(&record_path).unwrap();
    let mut elsewhere: Value = serde_json::from_str(&record).unwrap();
    elsewhere["host"] = "build-host-elsewhere".into();
    std::fs::write(&record_path, serde_json::to_vec(&elsewhere).unwrap()).unwrap();
    let refused = af(&repo, &state, &["task", "cancel", task]);
    assert_eq!(refused.status.code(), Some(1));
    let said = err(&refused);
    assert!(
        said.contains(&format!("pid {pid} on host build-host-elsewhere")),
        "{said}"
    );
    assert!(alive(pid), "a refusal signals nothing");
    std::fs::write(&record_path, record).unwrap();

    let cancelled = document(
        &af(&repo, &state, &["task", "cancel", task, "--json"]),
        0,
        Some("task-cancelled-v2.json"),
    );
    assert_eq!(cancelled["request"]["pid"], pid);
    assert_eq!(
        cancelled["state"],
        serde_json::json!({"phase":"abandoned","outcome":"cancelled","cause":"host_cancelled",
            "next_action":format!("af task run {task} --state {}", state.display())})
    );
    let gone = Instant::now() + Duration::from_secs(15);
    while alive(pid) || alive(worker[1]) {
        assert!(
            Instant::now() < gone,
            "the run or its Worker survived the cancel"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let shown = expect(&af(&repo, &state, &["task", "show", task]), 0);
    assert!(
        shown.starts_with(&format!(
            "Task {task}: abandoned/cancelled/host_cancelled\n"
        )),
        "{shown}"
    );
    assert!(shown.contains("cancelled: run 1 by "), "{shown}");
    assert!(
        shown.ends_with(&format!(
            "next: af task run {task} --state {}\n",
            state.display()
        )),
        "{shown}"
    );
    // The follower saw the cancel: every event from 0, then the one terminal record.
    let stdout = follower.stdout.take().unwrap();
    let followed = bounded_follower(follower, stdout, Duration::from_secs(60));
    leftovers
        .0
        .retain(|recorded| recorded.pid() != i32::try_from(follower_pid).unwrap());
    assert_eq!(followed.status.code(), Some(4), "{}", err(&followed));
    let lines = event_lines(&out(&followed), 0);
    let terminal = lines.last().unwrap();
    assert_eq!(terminal["record"], "terminal", "{}", out(&followed));
    assert_eq!(terminal["exit_code"], 4);
    assert_eq!(terminal["state"]["outcome"], "cancelled");
    assert_eq!(
        terminal["state"]["next_action"],
        format!("af task run {task} --state {}", state.display())
    );
    let followed_last = terminal["last_sequence"].as_u64().unwrap();
    assert_eq!(lines[lines.len() - 2]["sequence"], followed_last);
    // Read again from the start, the stream is the follower's, then what the stopped run
    // recorded after it, under the same state.
    let whole = af(&repo, &state, &["task", "events", task]);
    assert_eq!(whole.status.code(), Some(4), "{}", err(&whole));
    let whole = event_lines(&out(&whole), 0);
    let whole_terminal = whole.last().unwrap();
    assert_eq!(whole_terminal["state"], terminal["state"]);
    assert_eq!(&whole[..lines.len() - 1], &lines[..lines.len() - 1]);
    let last = whole_terminal["last_sequence"].as_u64().unwrap();
    // Reconnecting with the last sequence it read, a follower gets only the rest.
    let rest = af(
        &repo,
        &state,
        &["task", "events", task, "--after", &seen.to_string()],
    );
    assert_eq!(rest.status.code(), Some(4), "{}", err(&rest));
    let rest_lines = event_lines(&out(&rest), seen + 1);
    assert_eq!(
        &rest_lines[..],
        &whole[usize::try_from(seen).unwrap() + 1..]
    );
    let beyond = af(
        &repo,
        &state,
        &["task", "events", task, "--after", &(last + 1).to_string()],
    );
    assert_eq!(beyond.status.code(), Some(1), "{}", err(&beyond));
    assert!(
        err(&beyond).contains(&format!("so --after {} names none of them", last + 1)),
        "{}",
        err(&beyond)
    );
    let refusal: Value = serde_json::from_slice(&beyond.stdout).unwrap();
    schemas::valid(&schemas::validator("error-v2.json"), &refusal);
    let again = af(&repo, &state, &["task", "cancel", task]);
    assert_eq!(again.status.code(), Some(1));
    assert!(err(&again).contains("is not running"), "{}", err(&again));
    let waited = af(&repo, &state, &["task", "wait", task]);
    assert_eq!(
        waited.status.code(),
        Some(4),
        "a cancelled Task waits as cancelled"
    );
    let notified = crate::common::af()
        .current_dir(&repo)
        .args(["task", "wait", task, "--notify", "--bell", "--state"])
        .arg(&state)
        .env("PATH", "/a-notifier-that-does-not-exist")
        .output()
        .unwrap();
    assert_eq!(notified.status.code(), Some(4), "{}", err(&notified));
    assert!(
        err(&notified).contains("warning: notifier unavailable"),
        "{}",
        err(&notified)
    );
    assert!(
        !err(&notified).contains('\u{7}'),
        "a pipe is not a terminal"
    );

    // A foreground run resumes it, with its progress on stderr and its state last.
    std::fs::write(&resume, b"").unwrap();
    let resumed = af(&repo, &state, &["task", "run", task]);
    let stderr = err(&resumed);
    assert_eq!(
        resumed.status.code(),
        Some(0),
        "{}\n{stderr}",
        out(&resumed)
    );
    assert!(
        stderr.contains("af task run: implement started ("),
        "{stderr}"
    );
    assert!(
        stderr.contains("af task run: implement finished ("),
        "{stderr}"
    );
    assert!(stderr.contains("tokens so far)"), "{stderr}");
    assert!(stderr.contains("af task run: check finished ("), "{stderr}");
    // Every node that ran, the kernel's own included, has one start line before each of its
    // finished or failed lines.
    let progress: Vec<&str> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("af task run: "))
        .collect();
    let mut open: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut ended = 0;
    for line in &progress {
        let Some((node, rest)) = line.split_once(' ') else {
            continue;
        };
        if rest.starts_with("started (") {
            assert!(open.insert(node), "{node} started twice:\n{stderr}");
        } else if rest.starts_with("finished (") || rest.starts_with("failed (") {
            assert!(open.remove(node), "{node} ended without a start:\n{stderr}");
            ended += 1;
        }
    }
    assert!(ended >= 3, "{stderr}");
    assert!(stderr.contains("af task run: check started ("), "{stderr}");
    assert_eq!(
        stderr.lines().last().unwrap(),
        format!(
            "Task {task}: finished/verified; next: af task deliver {task} --branch af/{task} \
             --worktree ../{task} --confirm {task} --state {}",
            state.display()
        )
    );
    let settled = document(
        &af(&repo, &state, &["task", "wait", task, "--json"]),
        0,
        Some("task-status-v2.json"),
    );
    assert_eq!(settled["state"]["outcome"], "verified");
    // The resumed run's events follow the cancelled stream's last sequence, under one terminal.
    let resumed_events = af(
        &repo,
        &state,
        &["task", "events", task, "--after", &last.to_string()],
    );
    assert_eq!(
        resumed_events.status.code(),
        Some(0),
        "{}",
        err(&resumed_events)
    );
    let lines = event_lines(&out(&resumed_events), last + 1);
    assert!(
        lines.iter().any(|line| line["change"] == "finished"),
        "{}",
        out(&resumed_events)
    );
    assert!(
        lines.iter().any(|line| line["detail"] == "settled"
            && line["result"] == "succeeded"
            && line["charged_tokens"].is_string()),
        "{}",
        out(&resumed_events)
    );
    let terminal = lines.last().unwrap();
    assert_eq!(terminal["record"], "terminal");
    assert_eq!(terminal["exit_code"], 0);
    assert_eq!(terminal["state"], settled["state"]);
    let notified = crate::common::af()
        .current_dir(&repo)
        .args(["task", "wait", task, "--notify", "--bell", "--state"])
        .arg(&state)
        .env("PATH", "/a-notifier-that-does-not-exist")
        .output()
        .unwrap();
    assert_eq!(notified.status.code(), Some(0), "{}", err(&notified));
    assert!(
        err(&notified).contains("warning: notifier unavailable"),
        "{}",
        err(&notified)
    );
    assert!(
        !err(&notified).contains('\u{7}'),
        "a pipe is not a terminal"
    );
    assert_eq!(
        settled["run"]["run"], 2,
        "the resumed run is run 2: {settled:#}"
    );
    leftovers.0.clear();
}

/// Issue #433: a planned Task whose run was starting when `af task events` first polled, and
/// whose process ended before it took the Task, is refused at the poll that finds no run
/// starting, as at a first poll (ADR-0250): exit 1, one `af/error@2` line and no terminal record.
/// The base tree refused only at the first poll, so this follower printed a terminal record of
/// the planned state and exited 0. The run here is a record of this host, written as `af task
/// run` writes it before its lease, for a process the test holds: the follower's first line
/// proves its first poll saw the run starting, and the process is ended and reaped before the
/// next poll reads the record again.
#[test]
fn a_planned_task_whose_starting_run_died_is_refused_at_a_later_poll() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(root.path(), "pagination");
    let task = task_interrupt::TASK;
    expect(
        &af(&repo, &state, &["task", "plan", "--file", "ticket.json"]),
        0,
    );
    // A process that lives until the test ends it, or until its stdin closes when the test
    // fails first.
    let mut run = Command::new("/bin/cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    std::fs::create_dir_all(state.join("runs")).unwrap();
    std::fs::write(
        state.join(format!("runs/{task}-1.json")),
        serde_json::to_vec(&serde_json::json!({
            "schema": "af/task-run-process@1",
            "task_id": task,
            "run": 1,
            "pid": run.id(),
            "host": review_process::host::host_name().unwrap_or_else(|| "unknown".into()),
            "writer": "cli-starting",
            "started_unix_ms": u64::try_from(started_unix_ms).unwrap(),
            "detached": false,
            "log": null,
        }))
        .unwrap(),
    )
    .unwrap();
    let mut follower = crate::common::af()
        .current_dir(&repo)
        .args(["task", "events", task, "--state"])
        .arg(&state)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = std::io::BufReader::new(follower.stdout.take().unwrap());
    let mut first = String::new();
    std::io::BufRead::read_line(&mut stdout, &mut first).unwrap();
    let first: Value = serde_json::from_str(&first)
        .unwrap_or_else(|_| panic!("not one line at the first poll: {first:?}"));
    assert_eq!(first["schema"], "af/task-event@1", "{first}");
    assert_eq!(first["record"], "event", "{first}");
    run.kill().unwrap();
    run.wait().unwrap();
    let followed = bounded_follower(follower, stdout, Duration::from_secs(60));
    assert_eq!(
        followed.status.code(),
        Some(1),
        "{}\n{}",
        out(&followed),
        err(&followed)
    );
    assert!(
        err(&followed).contains("is planned and no run of it is in progress"),
        "{}",
        err(&followed)
    );
    let lines: Vec<Value> = out(&followed)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not one line: {line}")))
        .collect();
    let errors: Vec<&Value> = lines
        .iter()
        .filter(|line| line["schema"] == "af/error@2")
        .collect();
    assert_eq!(errors.len(), 1, "{}", out(&followed));
    schemas::valid(&schemas::validator("error-v2.json"), errors[0]);
    assert!(
        lines
            .iter()
            .all(|line| line["schema"] == "af/error@2" || line["record"] == "event"),
        "no terminal record:\n{}",
        out(&followed)
    );
}

/// A detached run that is neither holding the Task nor admitted when the window passes is not
/// claimed to run: `--detach` exits 4 with `admitted: false` and `af task status ID` next, and
/// the run, held back here as a loaded machine can, still starts and finishes afterwards.
#[test]
fn a_detached_run_not_admitted_in_time_is_not_claimed_to_run() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(root.path(), "pagination");
    let task = task_interrupt::TASK;
    expect(
        &af(&repo, &state, &["task", "plan", "--file", "ticket.json"]),
        0,
    );
    let detached = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "run",
            task,
            "--execute",
            "--detach",
            "--json",
            "--state",
        ])
        .arg(&state)
        .env("AF_TEST_DETACH_ADMISSION_MS", "200")
        .env("AF_TEST_DETACHED_START_DELAY_MS", "3000")
        .output()
        .unwrap();
    let value = document(&detached, 4, Some("task-detached-v3.json"));
    let pid = u32::try_from(value["pid"].as_u64().unwrap()).unwrap();
    let _leftovers = leftovers(&[pid]);
    assert_eq!(value["admitted"], false, "{value:#}");
    assert_eq!(value["next_action"], format!("af task status {task}"));
    assert_ne!(value["state"]["phase"], "running", "{value:#}");
    let said = err(&detached);
    assert!(said.contains(&format!("`af task status {task}`")), "{said}");
    for line in said.lines() {
        assert!(line.chars().count() <= 100, "{line}");
    }
    // The run goes on: once it starts it takes the Task and finishes it.
    let waited = af(&repo, &state, &["task", "wait", task, "--timeout", "600"]);
    assert_eq!(waited.status.code(), Some(0), "{}", err(&waited));
}
