//! Execute the actual emitted starter packages and their public contracts through the CLI.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn run(repo: &Path, args: &[&str], code: i32) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(code),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn task(repo: &Path, state: &Path, args: &[&str], code: i32) -> Value {
    let mut args = args.to_vec();
    args.extend(["--state", state.to_str().unwrap()]);
    run(repo, &args, code)
}
fn commit(repo: &Path) {
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "starter authority"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
}
fn setup(root: &Path, key: Option<&str>) -> (PathBuf, PathBuf) {
    let key_path = root.join("owner.pub");
    let mut args = vec![
        "catalog",
        "init",
        "--profile",
        "all",
        "--destination",
        "project",
    ];
    if let Some(key) = key {
        std::fs::write(&key_path, key).unwrap();
        args.extend(["--developer-public-key", key_path.to_str().unwrap()]);
    }
    let init = run(root, &args, 0);
    assert_eq!(init["attempts"], 0);
    let repo = root.join("project");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    commit(&repo);
    (repo, root.join("state"))
}
fn repin(repo: &Path) {
    for path in [".af/task-catalog.toml", "catalog.toml"] {
        let file = repo.join(path);
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
            pin["digest"] = toml::Value::String(
                review_config::lock::package_digest(
                    name,
                    &repo.join(pin["path"].as_str().unwrap()),
                )
                .unwrap(),
            );
        }
        std::fs::write(file, toml::to_string(&catalog).unwrap()).unwrap();
    }
    commit(repo);
}
fn worker_path(repo: &Path, name: &str) -> PathBuf {
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    repo.join(catalog["packages"][name]["path"].as_str().unwrap())
        .join("worker.py")
}

#[test]
fn emitted_starters_validate_and_execute_without_credentials_on_one_common_runtime() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = setup(root.path(), None);
    let checked = run(&repo, &["catalog", "test", "--source", "."], 0);
    assert_eq!(checked["attempts"], 0);
    for (name, attempts) in [
        ("implementation-small", 3),
        ("implementation-heavy", 3),
        ("implementation-reviewed", 4),
        ("implementation-repair-targeted", 4),
        ("implementation-repair-heavy", 4),
        ("document", 3),
    ] {
        let file = format!("{name}.json");
        let planned = task(&repo, &state, &["task", "plan", "--file", &file], 0);
        assert_eq!(planned["attempts"], 0, "{name}: {planned:#}");
        let id = if name == "document" {
            "release-notes"
        } else {
            name
        };
        let result = task(&repo, &state, &["task", "run", id], 0);
        assert_eq!(
            result["result"]["acceptance"], "satisfied",
            "{name}: {result:#}"
        );
        assert_eq!(result["attempts"], attempts, "{name}: {result:#}");
        assert_eq!(task(&repo, &state, &["task", "run", id], 0), result);
    }
    // The standalone review sees a real committed implementation of the same source problem.
    std::fs::write(repo.join("pagination.py"),"def paginate(items, offset=0, limit=2):\n    if type(offset) is not int or type(limit) is not int or offset < 0 or limit < 0:\n        raise ValueError('invalid bounds')\n    return items[offset:offset+limit]\n").unwrap();
    commit(&repo);
    for (name, attempts, rounds) in [("review-light", 3, 1), ("review-heavy", 6, 2)] {
        let file = format!("{name}.json");
        let result = task(&repo, &state, &["review", "--file", &file], 0);
        assert_eq!(result["result"]["acceptance"], "satisfied");
        assert_eq!(result["attempts"], attempts);
        assert_eq!(result["review_rounds"].as_array().unwrap().len(), rounds);
    }
}

#[test]
fn starter_repairs_use_current_evidence_and_missing_reviewers_remain_incomplete() {
    for (name, attempts) in [
        ("implementation-repair-targeted", 7),
        ("implementation-repair-heavy", 10),
        ("implementation-reviewed", 4),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = setup(root.path(), None);
        if name == "implementation-reviewed" {
            std::fs::write(
                worker_path(&repo, "builtin/bugs"),
                "raise Exception('reviewer unavailable')\n",
            )
            .unwrap();
        } else {
            // Explicit fault injection into the test's committed Worker, never the shipped default.
            std::fs::write(worker_path(&repo,"builtin/implementer"),"import json,sys\njson.load(sys.stdin)\nopen('pagination.py','w').write('def paginate(items, offset=0, limit=2):\\n    return items[offset:offset+limit]\\n')\nprint(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Injected missing bounds check'}]}}))\n").unwrap();
        }
        repin(&repo);
        let code = if name == "implementation-reviewed" {
            4
        } else {
            0
        };
        let file = format!("{name}.json");
        let result = task(&repo, &state, &["task", "start", "--file", &file], code);
        assert_eq!(result["attempts"], attempts, "{name}: {result:#}");
        assert_eq!(
            result["result"]["acceptance"],
            if code == 0 {
                "satisfied"
            } else {
                "inconclusive"
            }
        );
        assert_eq!(task(&repo, &state, &["task", "run", name], code), result);
        if code == 0 {
            assert_eq!(
                result["review_rounds"][0]["conclusion"],
                "changes_requested"
            );
            let expected = if name.ends_with("heavy") { 2 } else { 1 };
            assert_eq!(result["review_rounds"].as_array().unwrap().len(), expected);
        }
    }
}

#[test]
fn starter_planner_waits_for_a_signed_decision_then_exports_for_a_second_developer() {
    let root = tempfile::tempdir().unwrap();
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let (repo, state) = setup(root.path(), Some(&key.pk.to_box().unwrap().into_string()));
    let planned = task(
        &repo,
        &state,
        &["task", "plan", "--file", "planning.json"],
        0,
    );
    assert_eq!(planned["attempts"], 0);
    let waiting = task(&repo, &state, &["task", "run", "generated-pagination"], 0);
    assert_eq!(
        waiting["phase"]["reason"], "needs_plan_review",
        "{waiting:#}"
    );
    assert_eq!(waiting["attempts"], 1, "{waiting:#}");
    let before_export = task(&repo, &state, &["task", "show", "generated-pagination"], 0);
    let exported = task(
        &repo,
        &state,
        &[
            "task",
            "export",
            "generated-pagination",
            "--name",
            "team/pagination",
            "--destination",
            "exported",
        ],
        0,
    );
    assert_eq!(exported["execution_authorized"], false);
    let original_worker = std::fs::read(worker_path(&repo, "builtin/implementer")).unwrap();
    let exported_worker = repo
        .join("exported")
        .join(
            exported["packages"]["builtin/implementer"]["path"]
                .as_str()
                .unwrap(),
        )
        .join("worker.py");
    assert_eq!(
        std::fs::read(exported_worker).unwrap(),
        original_worker,
        "The shared Worker's public specification checker remains byte-identical"
    );
    assert_eq!(
        task(&repo, &state, &["task", "show", "generated-pagination"], 0),
        before_export
    );
    commit(&repo);
    assert_eq!(
        run(
            &repo,
            &[
                "catalog",
                "test",
                "--source",
                ".",
                "--manifest",
                "exported/catalog.toml"
            ],
            0
        )["attempts"],
        0
    );
    let payload = root.path().join("approval.payload");
    let signature = root.path().join("approval.minisig");
    task(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            "generated-pagination",
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "Reviewed exact starter plan and verification",
            "--output",
            payload.to_str().unwrap(),
        ],
        0,
    );
    std::fs::write(
        &signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            std::fs::read(&payload).unwrap().as_slice(),
            Some("exact generated starter"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    task(
        &repo,
        &state,
        &[
            "task",
            "approve",
            "generated-pagination",
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
        0,
    );
    let done = task(&repo, &state, &["task", "run", "generated-pagination"], 0);
    assert_eq!(done["attempts"], 5);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    let second = root.path().join("second");
    std::fs::create_dir(&second).unwrap();
    let (consumer, consumer_state) = setup(&second, None);
    run(
        &consumer,
        &[
            "catalog",
            "sync",
            "--source",
            repo.to_str().unwrap(),
            "--manifest",
            "exported/catalog.toml",
            "--destination",
            ".af/vendor/team",
        ],
        0,
    );
    let path = consumer.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    catalog["packages"] = toml::Value::Table(toml::map::Map::new());
    catalog["kinds"] = toml::Value::Table(toml::map::Map::new());
    catalog.as_table_mut().unwrap().insert(
        "imports".into(),
        toml::Value::try_from(vec![".af/vendor/team/catalog.lock.json"]).unwrap(),
    );
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    let ticket = consumer.join("planning.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&ticket).unwrap()).unwrap();
    value["task_id"] = json!("reused-pagination");
    value["pipeline"]["name"] = json!("team/pagination");
    value["pipeline"]["fallback"] = json!("refuse");
    std::fs::write(ticket, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    commit(&consumer);
    let reused = task(
        &consumer,
        &consumer_state,
        &["task", "start", "--file", "planning.json"],
        0,
    );
    assert_eq!(reused["attempts"], 4);
    assert!(reused["planning"].is_null());
    assert_eq!(reused["result"]["acceptance"], "satisfied");
    let explanation = task(
        &consumer,
        &consumer_state,
        &["task", "explain", "reused-pagination"],
        0,
    );
    assert_eq!(explanation["plan"]["generated_origins"], json!([]));
}
