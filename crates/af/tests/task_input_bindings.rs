//! ADR-0117 end to end: three Tasks in one Store, chained by artifact identity.
//!
//! The first is an ordinary reviewed implementation with no `inputs` table at all. The second
//! binds `history` to its ledger and delivers, proving a Task boundary crossed without a file.
//! The third binds `source` to its derived candidate tree, runs, and is refused at delivery,
//! because a derived tree has no commit for a target repository's `HEAD` to equal.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/task_cli.rs"]
mod task_cli;

const AF: &str = env!("CARGO_BIN_EXE_af");

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap()
}

fn run(repo: &Path, state: &Path, args: &[&str], json: bool) -> Output {
    let mut command = Command::new(AF);
    command.current_dir(repo).args(args);
    if json {
        command.arg("--json");
    }
    command.arg("--state").arg(state).output().unwrap()
}

fn json(repo: &Path, state: &Path, args: &[&str], expected: i32) -> Value {
    let output = run(repo, state, args, true);
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    let shown = format!("{args:?}\n{out}\n{err}");
    assert_eq!(output.status.code(), Some(expected), "{shown}");
    serde_json::from_str(&out).unwrap()
}

fn text(repo: &Path, state: &Path, args: &[&str]) -> String {
    let output = run(repo, state, args, false);
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "{args:?}\n{err}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn validator(name: &str) -> jsonschema::Validator {
    let directory = workspace().join("schemas");
    let mut registry = jsonschema::Registry::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let Some(id) = value["$id"].as_str().map(str::to_owned) else {
            continue;
        };
        registry = registry
            .add(id, jsonschema::Resource::from_contents(value))
            .unwrap();
    }
    let value: Value =
        serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap();
    {
        let registry = registry.prepare().unwrap();
        jsonschema::options()
            .with_registry(&registry)
            .build(&value)
            .unwrap()
    }
}

/// The real receipt is the schema's conformance corpus: a document the CLI actually printed.
fn valid(schema: &jsonschema::Validator, value: &Value) {
    let errors: Vec<_> = schema
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// The fixture pins its package digests from the copied tree: its `fixture/implementation`
/// Pipeline adds the `history` output that `embedded-review` does not have, so the checked-in
/// digest is a placeholder. Same approach as `crates/af/tests/task_acceptance.rs`.
fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, state) = task_cli::fixture_named(root, "bound-inputs");
    let path = repo.join(".af/task-catalog.toml");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut catalog: toml::Value = toml::from_str(&source).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        let package = repo.join(pin["path"].as_str().unwrap());
        let digest = review_config::lock::package_digest(name, &package);
        pin["digest"] = toml::Value::String(digest.unwrap());
    }
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "pin packages"].as_slice(),
    ] {
        let output = git(&repo, args);
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "git {args:?}: {err}");
    }
    (repo, state)
}

/// A source refresh replaces the requirements artifact and nothing else. The binding record is
/// provenance no port carries, so it has to survive the new revision on its own — in the
/// revision the Store admitted, in the `af/task-inspection@11` document and in every view.
#[test]
fn an_issue_refresh_keeps_the_binding_record_of_the_revision_it_replaces() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());
    let issue = |description: &str| {
        json!({"schema":"af.issue-input/1","id":"10042","key":"AF-42","revision":"v1",
            "summary":"Carry the reviewed ledger forward",
            "description":description,"acceptance":{"ledger":"Read the bound ledger."}})
    };
    let write_issue = |value: &Value| {
        let bytes = serde_json::to_vec_pretty(value).unwrap();
        std::fs::write(repo.join("issue.json"), bytes).unwrap();
    };
    write_issue(&issue("Preserve the input values."));
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "captured issue"].as_slice(),
    ] {
        assert!(git(&repo, args).status.success(), "git {args:?}");
    }

    let start = ["task", "start", "--execute", "--file", "base.json"];
    let base = json(&repo, &state, &start, 0);
    assert_eq!(base["result"]["acceptance"], "satisfied");
    let ledger = base["result"]["outputs"]["history"]["artifact_ids"][0].clone();

    let plan = ["task", "plan", "--file", "successor-issue.json"];
    let planned = json(&repo, &state, &plan, 0);
    assert_eq!(planned["schema"], "af/task-inspection@11");
    let binding = &planned["input_bindings"]["bindings"]["history"];
    assert_eq!(binding["artifact_id"], ledger);

    write_issue(&issue(
        "Preserve every original input value, including an empty list.",
    ));
    let refreshed = json(&repo, &state, &["task", "refresh", "chain-issue"], 0);
    assert_ne!(refreshed["revision_id"], planned["revision_id"]);
    assert_eq!(refreshed["schema"], "af/task-inspection@11");
    let binding = &refreshed["input_bindings"]["bindings"]["history"];
    assert_eq!(binding["artifact_id"], ledger);
    assert_eq!(binding["task"]["task_id"], "chain-base");
    assert_eq!(binding["task"]["port"], "history");
    valid(&validator("task-inspection-v11.json"), &refreshed);

    // Read back from the revision the Store admitted, not from the one it replaced.
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let id = refreshed["revision_id"].as_str().unwrap();
    let revision = cas.get_json(id).unwrap()["payload"].clone();
    assert_eq!(revision["previous_revision_id"], planned["revision_id"]);
    let provenance = revision["provenance"]["input_artifact_ids"].clone();
    let records = provenance
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|id| cas.get_json(id.as_str().unwrap()).ok())
        .filter(|envelope| envelope["type"] == "af/TaskInputBindings@1")
        .count();
    assert_eq!(records, 1, "{provenance}");
    assert_eq!(revision["inputs"]["history"]["artifact_ids"][0], ledger);

    let explain = ["task", "explain", "chain-issue", "--tree"];
    let preview = text(&repo, &state, &explain);
    let row = "BOUND history <- task chain-base/history (satisfied/";
    assert!(preview.contains(row), "{preview}");
    let shown = text(&repo, &state, &["task", "show", "chain-issue"]);
    let line = "bound history <- task chain-base/history (satisfied)";
    assert!(shown.contains(line), "{shown}");
}

#[test]
fn a_chain_of_tasks_becomes_a_chain_of_artifact_ids_in_one_store() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());
    let start = |file: &str| {
        let args = ["task", "start", "--execute", "--file", file];
        json(&repo, &state, &args, 0)
    };
    let show = |task: &str| {
        let args = ["task", "show", task];
        json(&repo, &state, &args, 0)
    };
    let deliver = |task: &str, branch: &str, tree: &Path, code: i32| {
        let path = tree.to_str().unwrap();
        let mut args = vec!["task", "deliver", task, "--confirm", task];
        args.extend(["--branch", branch, "--worktree", path]);
        json(&repo, &state, &args, code)
    };

    // 1. An ordinary reviewed implementation. Its Task file carries no `inputs` table, so it
    //    emits neither the field nor the new inspection generation.
    let base = start("base.json");
    assert_eq!(base["result"]["acceptance"], "satisfied");
    assert!(base.get("input_bindings").is_none(), "{base}");
    // One inspection generation: a Task without bindings prints the same `@11` document it
    // always did, without the field.
    assert_eq!(
        base["schema"], "af/task-inspection@11",
        "{}",
        base["schema"]
    );
    let outputs = &base["result"]["outputs"];
    assert_eq!(outputs["history"]["artifact_type"], "af/ReviewHistory@1");
    let ledger = outputs["history"]["artifact_ids"][0].clone();
    let ledger_id = ledger.as_str().unwrap().to_owned();
    let tree = outputs["snapshot"]["snapshot_id"].clone();
    let base_shown = show("chain-base");

    // 2. A successor binds `history`. The referenced artifact is carried verbatim, and the
    //    `empty_review_history` root default is suppressed.
    let plan_args = ["task", "plan", "--file", "successor-history.json"];
    let planned = json(&repo, &state, &plan_args, 0);
    assert_eq!(planned["schema"], "af/task-inspection@11");
    let inspection = validator("task-inspection-v11.json");
    valid(&inspection, &planned);
    // This real receipt is the conformance corpus of the self-optimizer adapter's `@12` unit
    // test; regenerate it with AF_WRITE_INSPECTION_FIXTURE=1 when the document changes.
    if std::env::var_os("AF_WRITE_INSPECTION_FIXTURE").is_some() {
        let path = workspace().join("fixtures/task-runtime/bound-inputs/inspection-bound.json");
        let text = serde_json::to_string_pretty(&planned).unwrap();
        std::fs::write(path, format!("{text}\n")).unwrap();
    }
    let binding = &planned["input_bindings"]["bindings"]["history"];
    assert_eq!(binding["artifact_id"], ledger);
    assert_eq!(binding["task"]["task_id"], "chain-base");
    assert_eq!(binding["task"]["port"], "history");
    assert_eq!(binding["task"]["acceptance"], "satisfied");
    assert!(binding.get("rerooted_snapshot_id").is_none());
    let bound_port = &planned["plan"]["inputs"]["history"]["artifact_ids"][0];
    assert_eq!(bound_port, &ledger, "a bound history is not a fresh empty");

    let explain = ["task", "explain", "chain-history", "--tree"];
    let preview = text(&repo, &state, &explain);
    let annotated = "history <- task chain-base/history";
    assert!(preview.contains(annotated), "{preview}");
    let row = "BOUND history <- task chain-base/history (satisfied/";
    assert!(preview.contains(row), "{preview}");
    let exact = preview.lines().any(|row| row.trim() == ledger_id);
    assert!(exact, "the artifact ID is its own row\n{preview}");

    let run_args = ["task", "run", "--execute", "chain-history"];
    let ran = json(&repo, &state, &run_args, 0);
    assert_eq!(ran["result"]["acceptance"], "satisfied");
    assert_eq!(ran["schema"], "af/task-inspection@11");
    valid(&inspection, &ran);
    let shown = text(&repo, &state, &["task", "show", "chain-history"]);
    let line = "bound history <- task chain-base/history (satisfied)";
    assert!(shown.contains(line), "{shown}");

    // A bound `history` does not touch delivery.
    let delivered = root.path().join("delivered");
    let receipt = deliver("chain-history", "chain/history", &delivered, 0);
    assert_eq!(receipt["outcome"]["kind"], "delivered");
    assert!(delivered.join("pagination.py").is_file());

    // 3. A successor binds `source` to the derived candidate tree. It is re-rooted, so the
    //    Task plans and runs like any other.
    // `--uncommitted` captures the invoking checkout, which a bound `source` replaces, so the
    // two are refused together, before any capture.
    let dirty_args = [
        "task",
        "plan",
        "--file",
        "successor-source.json",
        "--uncommitted",
    ];
    let refused = json(&repo, &state, &dirty_args, 1);
    let message = refused["error"].to_string();
    assert!(message.contains("bound `source`"), "{refused}");

    let bound = start("successor-source.json");
    assert_eq!(bound["result"]["acceptance"], "satisfied");
    assert_eq!(bound["schema"], "af/task-inspection@11");
    let binding = bound["input_bindings"]["bindings"]["source"].clone();
    assert_eq!(binding["task"]["task_id"], "chain-base");
    assert_eq!(binding["task"]["port"], "snapshot");
    assert_eq!(binding["snapshot_id"], tree);
    let rerooted = binding["rerooted_snapshot_id"].clone();
    assert!(rerooted.is_string(), "the derived tree was re-rooted");
    assert_ne!(rerooted, tree);
    let explained = json(&repo, &state, &["task", "explain", "chain-source"], 0);
    let port = &explained["plan"]["inputs"]["source"];
    assert_eq!(port["snapshot_id"], rerooted);
    assert_eq!(port["artifact_ids"][0], binding["resolved_artifact_id"]);

    // Its delivery is refused, before the prepared record and any Git mutation.
    let refused_tree = root.path().join("refused");
    let refused = deliver("chain-source", "chain/source", &refused_tree, 1);
    assert_eq!(refused["schema"], "af/error@1");
    let reason = refused["error"].as_str().unwrap();
    assert!(reason.contains("task chain-base/snapshot"), "{reason}");
    assert!(reason.contains("no commit"), "{reason}");
    assert!(!refused_tree.exists(), "a refusal made a worktree");
    let branch = git(&repo, &["rev-parse", "--verify", "chain/source"]);
    assert!(!branch.status.success(), "a refusal made a branch");
    let source_shown = show("chain-source");
    assert!(source_shown.get("delivery").is_none(), "{source_shown}");

    // 4. The re-rooted source, bound again by exact artifact. It is parentless with a
    //    generation-two origin, so it is carried verbatim — nothing is re-rooted twice — and
    //    its delivery refusal names the artifact this Task bound, not the Task whose output the
    //    first re-rooting recorded in that origin.
    let resolved = binding["resolved_artifact_id"].as_str().unwrap().to_owned();
    let file = json!({"schema":"af.task-file/1","task_id":"chain-rebound","kind":"implement",
        "goal":"Re-bind the re-rooted tree by exact artifact",
        "inputs":{"source":{"artifact":resolved}},
        "pipeline":{"name":"fixture/plain","fallback":"refuse"},
        "strategy":"small","facts":{},
        "limits":{"tokens":1000,"max_attempts":3,"wall_ms":60000,
            "verification":{"tokens":200,"attempts":2,"wall_ms":10000}}});
    let bytes = serde_json::to_vec_pretty(&file).unwrap();
    std::fs::write(repo.join("rebound.json"), bytes).unwrap();
    let rebound = start("rebound.json");
    assert_eq!(rebound["result"]["acceptance"], "satisfied");
    let record = &rebound["input_bindings"]["bindings"]["source"];
    assert_eq!(record["artifact_id"], json!(resolved));
    assert_eq!(record["snapshot_id"], rerooted);
    assert!(record.get("task").is_none(), "no Task was named");
    assert!(record.get("rerooted_snapshot_id").is_none(), "{record}");
    let again = deliver(
        "chain-rebound",
        "chain/rebound",
        &root.path().join("again"),
        1,
    );
    let reason = again["error"].as_str().unwrap();
    assert!(reason.contains(&format!("artifact {resolved}")), "{reason}");
    assert!(!reason.contains("chain-base"), "{reason}");
    assert!(reason.contains("no commit"), "{reason}");

    // The referenced Task's own documents are exactly what they were.
    assert_eq!(show("chain-base"), base_shown);
}
