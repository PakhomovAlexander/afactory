//! ADR-0203 end to end: a reviewed Task continued from its candidate with typed findings, and a
//! short Task file that takes the project's `[task.defaults]` and lists its context by digest.
//!
//! The fixture is `fixtures/task-runtime/continue`. Its first Task ends `changes_requested` on
//! one reviewer finding; `af task run second --from first` plans and runs a new Task whose source
//! is that reviewed candidate and whose implementer reads the finding on its `findings` input.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::task_cli;

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap()
}

fn commit(repo: &Path, message: &str) {
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", message].as_slice(),
    ] {
        let output = git(repo, args);
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "git {args:?}: {err}");
    }
}

fn run(repo: &Path, state: &Path, args: &[&str], json: bool) -> Output {
    let mut command = crate::common::af();
    command.current_dir(repo).args(args);
    if json {
        command.arg("--json");
    }
    command.arg("--state").arg(state).output().unwrap()
}

/// The one JSON document a command printed, its exit code checked, and its stderr.
fn json(repo: &Path, state: &Path, args: &[&str], expected: i32) -> (Value, String) {
    let output = run(repo, state, args, true);
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    let shown = format!("{args:?}\n{out}\n{err}");
    assert_eq!(output.status.code(), Some(expected), "{shown}");
    (serde_json::from_str(&out).expect(&shown), err)
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
    let registry = registry.prepare().unwrap();
    jsonschema::options()
        .with_registry(&registry)
        .build(&value)
        .unwrap()
}

fn valid(name: &str, value: &Value) {
    let schema = validator(name);
    let errors: Vec<_> = schema
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{name}: {}", errors.join("\n"));
}

/// The fixture with every package digest pinned from the copied tree, committed.
fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    fixture_with(root, |_| {})
}

/// [`fixture`], with `edit` applied to the copied tree before its digests are pinned.
fn fixture_with(root: &Path, edit: impl FnOnce(&Path)) -> (PathBuf, PathBuf) {
    let (repo, state) = task_cli::fixture_named(root, "continue");
    edit(&repo);
    let path = repo.join(".af/task-catalog.toml");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut catalog: toml::Value = toml::from_str(&source).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        let package = repo.join(pin["path"].as_str().unwrap());
        let digest = review_config::lock::package_digest(name, &package);
        pin["digest"] = toml::Value::String(digest.unwrap());
    }
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    commit(&repo, "pin packages");
    (repo, state)
}

/// The payload of one recorded artifact.
fn payload(cas: &review_store::Cas, id: &Value) -> Value {
    cas.get_json(id.as_str().unwrap()).unwrap()["payload"].clone()
}

/// The bytes of `path` in the Snapshot an `af/SourceTree@1` artifact names.
fn file_in(cas: &review_store::Cas, source_tree: &Value, path: &str) -> Option<Vec<u8>> {
    let snapshot = payload(cas, source_tree)["snapshot_id"].clone();
    let (_, manifest) =
        review_source_git::task::read_snapshot(cas, snapshot.as_str().unwrap()).unwrap();
    let entry = manifest.get(path)?;
    Some(cas.get(&entry.content).unwrap())
}

#[test]
fn a_reviewed_task_continues_from_its_candidate_with_typed_findings() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());

    // The first Task ends on its review: one major finding, no NOTES.md.
    let start = ["task", "start", "--execute", "--file", "first.json"];
    let (first, _) = json(&repo, &state, &start, 3);
    assert_eq!(first["state"]["outcome"], "changes_requested", "{first}");
    let first_revision_id = first["revision_id"].clone();
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let first_revision = payload(&cas, &first_revision_id);
    assert!(
        first_revision["inputs"].get("findings").is_none(),
        "a Task that continues nothing plans without the findings port"
    );
    assert!(first.get("continues").is_none());

    // A round that does not exist, and a Task that does not, are refused by name.
    let wrong = run(
        &repo,
        &state,
        &["task", "run", "second", "--from", "first", "--round", "2"],
        false,
    );
    let err = String::from_utf8_lossy(&wrong.stderr).into_owned();
    assert_eq!(wrong.status.code(), Some(1), "{err}");
    assert!(
        err.contains("has no review round 2; its rounds are 1"),
        "{err}"
    );
    let absent = run(
        &repo,
        &state,
        &["task", "run", "second", "--from", "nope"],
        false,
    );
    let err = String::from_utf8_lossy(&absent.stderr).into_owned();
    assert_eq!(absent.status.code(), Some(1), "{err}");
    assert!(err.contains("--from") && err.contains("nope"), "{err}");

    // Without --execute it plans and waits for the plan's confirmation.
    let plan = ["task", "run", "second", "--from", "first"];
    let (planned, _) = json(&repo, &state, &plan, 0);
    assert_eq!(planned["schema"], "af/task-inspection@13");
    assert_eq!(planned["continues"]["task_id"], "first");
    assert_eq!(planned["continues"]["round"], 1);
    assert_eq!(planned["continues"]["findings"], 1);
    valid("task-inspection-v13.json", &planned);

    let plan_id = planned["plan_id"].as_str().unwrap().to_owned();
    let confirm = ["task", "run", "second", "--confirm-plan", &plan_id];
    let (second, _) = json(&repo, &state, &confirm, 0);
    assert_eq!(second["result"]["acceptance"], "satisfied", "{second}");

    // The revision binds the reviewed candidate as its source and the findings as a typed port.
    let revision = payload(&cas, &second["revision_id"]);
    let findings_id = revision["inputs"]["findings"]["artifact_ids"][0].clone();
    assert_eq!(findings_id, planned["continues"]["findings_id"]);
    assert_eq!(
        revision["inputs"]["findings"]["artifact_type"],
        "af/TaskReviewFindings@1"
    );
    let findings = payload(&cas, &findings_id);
    valid("task-review-findings-v1.json", &findings);
    assert_eq!(findings["continues"]["task_id"], "first");
    assert_eq!(findings["findings"][0]["id"], "bugs-1");
    assert_eq!(findings["findings"][0]["severity"], "major");
    assert_eq!(findings["findings"][0]["title"], "paginate is undocumented");
    // One builder makes a round's findings: `af task findings --round 1` and the continued
    // implementer's findings carry the same set for the same round.
    let (printed, _) = json(
        &repo,
        &state,
        &["task", "findings", "first", "--round", "1"],
        0,
    );
    let mut shown: Vec<Value> = Vec::new();
    for reviewer in printed["round"]["reviewers"].as_array().unwrap() {
        for (index, finding) in reviewer["findings"].as_array().unwrap().iter().enumerate() {
            shown.push(serde_json::json!({
                "id": format!("{}-{}", reviewer["reviewer"].as_str().unwrap(), index + 1),
                "by": reviewer["reviewer"], "severity": finding["severity"],
                "title": finding["title"], "file": finding["file"], "line": finding["line"],
                "body": finding["evidence"], "fix": finding["fix"],
            }));
        }
    }
    let handed: Vec<Value> = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["origin"] == "reviewer")
        .map(|finding| {
            serde_json::json!({
                "id": finding["id"], "by": finding["by"], "severity": finding["severity"],
                "title": finding["title"], "file": finding.get("file").cloned().unwrap_or(Value::Null),
                "line": finding.get("line").cloned().unwrap_or(Value::Null),
                "body": finding["body"],
                "fix": finding.get("fix").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    assert!(!handed.is_empty());
    assert_eq!(shown, handed, "{printed:#}\n{findings:#}");
    let requirements = payload(&cas, &revision["inputs"]["requirements"]["artifact_ids"][0]);
    let text = requirements["text"].as_str().unwrap();
    assert!(
        text.starts_with("Implement offset/limit pagination\n\nRead first"),
        "the goal is the prior goal: {text}"
    );
    assert!(
        !text.contains("undocumented") && !text.contains("offset:offset+limit"),
        "nothing of the findings or the candidate is pasted into it: {text}"
    );
    let base = &first_revision["inputs"]["source"];
    let source = &revision["inputs"]["source"];
    assert_ne!(source["snapshot_id"], base["snapshot_id"], "not the base");
    let candidate = file_in(&cas, &source["artifact_ids"][0], "pagination.py").unwrap();
    assert!(
        String::from_utf8(candidate)
            .unwrap()
            .contains("offset:offset+limit")
    );

    // What the implementer saw in its sandbox: the candidate tree and the findings document.
    let sealed = &second["result"]["outputs"]["snapshot"]["artifact_ids"][0];
    let seen = file_in(&cas, sealed, "seen-findings.json").unwrap();
    let seen: Value = serde_json::from_slice(&seen).unwrap();
    assert_eq!(seen, findings);
    assert!(file_in(&cas, sealed, "NOTES.md").is_some());

    // The lineage is shown, in the document and as text.
    let (shown, _) = json(&repo, &state, &["task", "show", "second"], 0);
    assert_eq!(shown["continues"], planned["continues"]);
    valid("task-inspection-v13.json", &shown);
    let text = run(&repo, &state, &["task", "show", "second"], false);
    let text = String::from_utf8_lossy(&text.stdout).into_owned();
    assert!(
        text.contains("continues first round 1 (1 finding on the findings port)"),
        "{text}"
    );

    // `af task report` shows the chain: the continued Task's row names what it continues.
    let report = run(&repo, &state, &["task", "report", "first", "second"], false);
    let report = String::from_utf8_lossy(&report.stdout).into_owned();
    let row = report
        .lines()
        .find(|line| line.contains("| second |"))
        .expect(&report);
    assert!(row.contains("continues first round 1"), "{report}");
}

#[test]
fn a_task_file_with_only_an_id_a_kind_and_a_goal_plans_with_the_project_defaults_and_context() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());
    let plan = ["task", "plan", "--file", "short.json"];
    let (planned, err) = json(&repo, &state, &plan, 0);
    assert!(
        err.contains(
            "defaults: limits (tokens 1000, max_attempts 5, wall_ms 60000), pipeline \
             fixture/implementation (fallback refuse) from .af/af.toml [task.defaults]"
        ),
        "{err}"
    );
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let revision = payload(&cas, &planned["revision_id"]);
    assert_eq!(revision["limits"]["tokens"], 1000);
    assert_eq!(revision["limits"]["max_attempts"], 5);
    assert_eq!(revision["pipeline"]["name"], "fixture/implementation");
    let requirements_id = revision["inputs"]["requirements"]["artifact_ids"][0].clone();
    let text = payload(&cas, &requirements_id)["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let rules = text.find("- .af/rules.md (sha256:").expect(&text);
    let design = text
        .find("- docs/design.md#paginate (sha256:")
        .expect(&text);
    let code = text.find("- pagination.py (sha256:").expect(&text);
    assert!(
        rules < design && design < code,
        "the rules come first: {text}"
    );
    assert!(text.starts_with("Document paginate; the design says how.\n\nRead first"));
    let record = revision["provenance"]["input_artifact_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| cas.get_json(id.as_str().unwrap()).unwrap())
        .find(|envelope| envelope["type"] == "af/TaskReadFirst@1")
        .expect("the read-first record is provenance");
    valid("task-read-first-v1.json", &record["payload"]);

    // The same files plan the same requirements; a changed context file changes them, and with
    // them the plan.
    let again = root.path().join("again");
    let (unchanged, _) = json(&repo, &again, &plan, 0);
    let unchanged = payload(
        &review_store::Cas::open_existing(again.join("cas")).unwrap(),
        &unchanged["revision_id"],
    );
    assert_eq!(
        unchanged["inputs"]["requirements"]["artifact_ids"][0],
        requirements_id
    );
    std::fs::write(
        repo.join("docs/design.md"),
        "# Design\n\n## paginate\n\nOffsets past the end return an empty page.\n",
    )
    .unwrap();
    commit(&repo, "design changed");
    let changed_state = root.path().join("changed");
    let (changed, _) = json(&repo, &changed_state, &plan, 0);
    let changed_cas = review_store::Cas::open_existing(changed_state.join("cas")).unwrap();
    let changed_revision = payload(&changed_cas, &changed["revision_id"]);
    assert_ne!(
        changed_revision["inputs"]["requirements"]["artifact_ids"][0],
        requirements_id
    );
    assert_ne!(changed["plan_id"], planned["plan_id"]);

    // A Task file without limits in a project without default limits is refused with both
    // places to state them.
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[project]\nname = \"x\"\nmin_af = \"0.2\"\n[defaults]\npipeline = \"d\"\n",
    )
    .unwrap();
    commit(&repo, "no task defaults");
    let bare = root.path().join("bare");
    let refused = run(&repo, &bare, &plan, false);
    let err = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert_eq!(refused.status.code(), Some(1), "{err}");
    assert!(
        err.contains("names no `limits`") && err.contains("[task.defaults.limits]"),
        "{err}"
    );
}

/// A Task file's `base` names the tree its judging Workers read at `../base`: the reviewer of
/// a Task based on the fixture's first commit sees the catalog pin in its diff subject, which
/// the default base, the source the candidate was sealed from, does not hold.
#[test]
fn a_declared_base_is_the_tree_the_reviewers_read_beside_their_own() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());
    let first_commit = git(&repo, &["rev-list", "--max-parents=0", "HEAD"]);
    let first_commit = String::from_utf8(first_commit.stdout).unwrap();
    let first_commit = first_commit.trim();
    let mut file: Value =
        serde_json::from_slice(&std::fs::read(repo.join("first.json")).unwrap()).unwrap();
    file["task_id"] = "based".into();
    file["base"] = serde_json::json!({"commit": first_commit});
    std::fs::write(repo.join("based.json"), file.to_string()).unwrap();
    commit(&repo, "based");

    let start = ["task", "start", "--execute", "--file", "based.json"];
    let (based, _) = json(&repo, &state, &start, 3);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let revision = payload(&cas, &based["revision_id"]);
    let record = revision["provenance"]["input_artifact_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| cas.get_json(id.as_str().unwrap()).unwrap())
        .find(|envelope| envelope["type"] == "af/TaskBase@1")
        .expect("the declared base is provenance");
    valid("task-base-v1.json", &record["payload"]);
    assert_eq!(record["payload"]["commit"], first_commit);

    // What the reviewer said about the base it was given, read through a continuation.
    let (planned, _) = json(
        &repo,
        &state,
        &["task", "run", "next", "--from", "based"],
        0,
    );
    let findings = payload(&cas, &planned["continues"]["findings_id"]);
    let titles: Vec<&str> = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["title"].as_str().unwrap())
        .collect();
    assert!(
        titles.contains(&"the base predates the pinned catalog"),
        "{titles:?}"
    );
    let continued = payload(&cas, &planned["revision_id"]);
    assert!(
        !continued["provenance"]["input_artifact_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| cas.get_json(id.as_str().unwrap()).unwrap()["type"] == "af/TaskBase@1"),
        "a continuation's base is the candidate it continues"
    );

    // A base that is not a commit of this repository is refused by name.
    file["task_id"] = "nowhere".into();
    file["base"] = serde_json::json!({"commit": "no-such-ref"});
    std::fs::write(repo.join("nowhere.json"), file.to_string()).unwrap();
    commit(&repo, "nowhere");
    let refused = run(
        &repo,
        &state,
        &["task", "plan", "--file", "nowhere.json"],
        false,
    );
    let err = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert_eq!(refused.status.code(), Some(1), "{err}");
    assert!(err.contains("base.commit no-such-ref"), "{err}");
}

/// The `continue` fixture as a Task of two review rounds on one candidate: round 1 asks for
/// `NOTES.md` in a minor finding and requests changes, since the review needs two clean rounds,
/// and a strict evaluator, run once that round has ended, fails it; round 2 reviews the same Snapshot, where the reviewer reports
/// nothing new, so it passes, and the goal's evaluator passes it.
fn two_rounds(repo: &Path) {
    let packages = repo.join(".af/task-packages/fixture");
    let strict = packages.join("strict-evaluator");
    std::fs::create_dir_all(strict.join("outputs")).unwrap();
    for file in [
        "input.schema.json",
        "outputs/result.schema.json",
        "worker.toml",
    ] {
        std::fs::copy(packages.join("evaluator").join(file), strict.join(file)).unwrap();
    }
    let manifest = std::fs::read_to_string(strict.join("worker.toml")).unwrap();
    std::fs::write(
        strict.join("worker.toml"),
        manifest.replace("fixture/evaluator", "fixture/strict-evaluator"),
    )
    .unwrap();
    std::fs::write(
        strict.join("worker.py"),
        "import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'schema':'af.worker-reply/1',\
         'outputs':{'result':[{'outcome':'failed','reason':'Round 1 verdict: NOTES.md is \
         missing'}]}}))\n",
    )
    .unwrap();
    // The reviewer reports only in the round that has no earlier finding to answer, and below
    // the gate: two clean rounds are needed, so round 1 requests changes and round 2 passes.
    let bugs = packages.join("bugs/worker.py");
    let worker = std::fs::read_to_string(&bugs).unwrap();
    let first_round = "reports=[] if os.path.exists('NOTES.md') else [{'severity':'major'";
    assert!(worker.contains(first_round), "{worker}");
    std::fs::write(
        &bugs,
        worker.replace(
            first_round,
            "reports=[] if os.path.exists('NOTES.md') or \
             r['inputs']['assignment'][0]['payload']['findings'] else [{'severity':'minor'",
        ),
    )
    .unwrap();

    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut pin = catalog["packages"]["fixture/evaluator"].clone();
    pin["path"] = ".af/task-packages/fixture/strict-evaluator".into();
    catalog["packages"]
        .as_table_mut()
        .unwrap()
        .insert("fixture/strict-evaluator".into(), pin);
    catalog["review"]["max_rounds"] = 2.into();
    catalog["review"]["clean_rounds"] = 2.into();
    std::fs::write(&path, toml::to_string(&catalog).unwrap()).unwrap();

    let path = packages.join("implementation/pipeline.toml");
    let mut pipeline: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut slot = pipeline["slots"]["evaluator"].clone();
    slot["worker"] = "fixture/strict-evaluator".into();
    pipeline["slots"]
        .as_table_mut()
        .unwrap()
        .insert("strict".into(), slot);
    let node = |value: &str| -> toml::Value { toml::from_str(value).unwrap() };
    let nodes = pipeline["nodes"].as_array_mut().unwrap();
    let after_review = nodes
        .iter()
        .position(|n| n["id"].as_str() == Some("review"))
        .unwrap()
        + 1;
    nodes.insert(
        after_review,
        node(
            r#"id = "accept_round"
operator = { op = "review_accept" }
inputs.source = { kind = "node", node = "seal", port = "snapshot" }
inputs.review = { kind = "node", node = "review", port = "review" }
inputs.checks = { kind = "node", node = "review", port = "checks" }"#,
        ),
    );
    nodes.insert(
        after_review + 1,
        node(
            r#"id = "evaluate_round"
operator = { op = "verify", slot = "strict" }
inputs.source = { kind = "node", node = "seal", port = "snapshot" }
inputs.requirements = { kind = "input", port = "requirements" }
inputs.checks = { kind = "node", node = "review", port = "checks" }
when = { node = "accept_round", outcome = "failed" }"#,
        ),
    );
    nodes.insert(
        after_review + 2,
        node(
            r#"id = "review_again"
operator = { op = "call", pipeline = "fixture/review", bindings = { bugs = "bugs", correctness = "correctness" } }
inputs.source = { kind = "node", node = "seal", port = "snapshot" }
inputs.base = { kind = "input", port = "source" }
inputs.history = { kind = "node", node = "review", port = "history" }
inputs.requirements = { kind = "input", port = "requirements" }"#,
        ),
    );
    // Acceptance, the final checks and the exposed ledger follow the second round.
    for node in nodes.iter_mut() {
        let id = node["id"].as_str().unwrap().to_owned();
        if !["accept", "final_checks"].contains(&id.as_str()) {
            continue;
        }
        for (_, input) in node["inputs"].as_table_mut().unwrap().iter_mut() {
            if input.get("node").and_then(|n| n.as_str()) == Some("review") {
                input["node"] = "review_again".into();
            }
        }
    }
    pipeline["outputs"]["history"]["node"] = "review_again".into();
    // One node at a time, in the order declared: round 1's verdict is recorded before round 2.
    pipeline["max_parallel"] = 1.into();
    pipeline["max_attempts"] = 12.into();
    std::fs::write(&path, toml::to_string(&pipeline).unwrap()).unwrap();

    let mut task: Value =
        serde_json::from_str(&std::fs::read_to_string(repo.join("first.json")).unwrap()).unwrap();
    task["task_id"] = "twice".into();
    task["limits"] = serde_json::json!({
        "tokens": 1000, "max_attempts": 12, "wall_ms": 120000,
        "verification": {"tokens": 200, "attempts": 10, "wall_ms": 60000},
    });
    std::fs::write(
        repo.join("twice.json"),
        serde_json::to_vec_pretty(&task).unwrap(),
    )
    .unwrap();
}

/// GA final fix item 2: `af task findings --round N` pairs round N's findings with round N's
/// verdict, from the Task log, and a continuation from round N receives that verdict, never the
/// verdict of a later round that reviewed the same Snapshot.
#[test]
fn each_round_carries_its_own_evaluator_verdict() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_with(root.path(), two_rounds);
    let start = ["task", "start", "--execute", "--file", "twice.json"];
    let (twice, err) = json(&repo, &state, &start, 0);
    assert_eq!(twice["state"]["outcome"], "verified", "{twice}\n{err}");

    let (first, _) = json(
        &repo,
        &state,
        &["task", "findings", "twice", "--round", "1"],
        0,
    );
    valid("task-findings-v1.json", &first);
    let (second, _) = json(
        &repo,
        &state,
        &["task", "findings", "twice", "--round", "2"],
        0,
    );
    valid("task-findings-v1.json", &second);
    assert_eq!(first["rounds"], 2, "{first}");
    // Both rounds reviewed the one candidate.
    assert_eq!(
        first["round"]["snapshot_id"], second["round"]["snapshot_id"],
        "{first}\n{second}"
    );
    assert_eq!(first["round"]["conclusion"], "changes_requested", "{first}");
    assert_eq!(first["round"]["counts"]["minor"], 1, "{first}");
    assert_eq!(first["evaluation"]["node"], "evaluate_round", "{first}");
    assert_eq!(first["evaluation"]["outcome"], "failed", "{first}");
    assert_eq!(
        first["evaluation"]["reason"], "Round 1 verdict: NOTES.md is missing",
        "{first}"
    );
    assert_eq!(second["round"]["conclusion"], "pass", "{second}");
    assert_eq!(second["evaluation"]["node"], "evaluate_goal", "{second}");
    assert_eq!(second["evaluation"]["outcome"], "passed", "{second}");
    // The last round is the default, with its own verdict.
    let (last, _) = json(&repo, &state, &["task", "findings", "twice"], 0);
    assert_eq!(last["evaluation"], second["evaluation"], "{last}");

    let text = run(
        &repo,
        &state,
        &["task", "findings", "twice", "--round", "1"],
        false,
    );
    assert_eq!(text.status.code(), Some(0));
    let text = String::from_utf8_lossy(&text.stdout).into_owned();
    assert!(
        text.contains("review round 1 of 2 (changes_requested)"),
        "{text}"
    );
    assert!(
        text.contains("evaluator (evaluate_round): failed"),
        "{text}"
    );
    assert!(
        text.contains("  Round 1 verdict: NOTES.md is missing"),
        "{text}"
    );
    assert!(!text.contains("evaluate_goal"), "{text}");

    // A continuation from round 1 receives round 1's verdict as its evaluator finding.
    let plan = ["task", "run", "next", "--from", "twice", "--round", "1"];
    let (planned, _) = json(&repo, &state, &plan, 0);
    assert_eq!(planned["continues"]["round"], 1, "{planned}");
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let findings = payload(&cas, &planned["continues"]["findings_id"]);
    valid("task-review-findings-v1.json", &findings);
    let evaluator: Vec<&Value> = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["origin"] == "evaluator")
        .collect();
    assert_eq!(evaluator.len(), 1, "{findings:#}");
    assert_eq!(evaluator[0]["title"], "Evaluator verdict: failed");
    assert_eq!(evaluator[0]["body"], "Round 1 verdict: NOTES.md is missing");
    assert_eq!(
        evaluator[0]["artifact_id"], first["evaluation"]["artifact_id"],
        "{findings:#}"
    );
    let reviewers: Vec<&Value> = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["origin"] == "reviewer")
        .collect();
    assert_eq!(reviewers.len(), 1, "{findings:#}");
    assert_eq!(reviewers[0]["title"], "paginate is undocumented");
    // From round 2 the verdict passed, so no evaluator finding is handed on.
    let plan = ["task", "run", "later", "--from", "twice", "--round", "2"];
    let (planned, _) = json(&repo, &state, &plan, 0);
    let findings = payload(&cas, &planned["continues"]["findings_id"]);
    assert!(
        findings["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|finding| finding["origin"] != "evaluator"),
        "{findings:#}"
    );
}
