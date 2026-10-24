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
    let (repo, state) = task_cli::fixture_named(root, "continue");
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
