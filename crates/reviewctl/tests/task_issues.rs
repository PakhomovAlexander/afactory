//! Account-free issue -> shared implementation/Review -> verified local delivery.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
fn run(repo: &Path, args: &[&str], expected: i32) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(expected),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn task(repo: &Path, state: &Path, args: &[&str], expected: i32) -> Value {
    let mut args = args.to_vec();
    args.extend(["--state", state.to_str().unwrap()]);
    run(repo, &args, expected)
}
fn git(repo: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(repo)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}
fn setup(root: &Path) -> (PathBuf, PathBuf) {
    run(
        root,
        &[
            "catalog",
            "init",
            "--profile",
            "software",
            "--destination",
            "project",
        ],
        0,
    );
    let repo = root.join("project");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    (repo, root.join("state"))
}
fn issue() -> review_core::task::source::IssueInputV1 {
    serde_json::from_value(json!({"schema":"af.issue-input/1","id":"10042","key":"AF-42","revision":"2026-09-12T10:00:00Z",
        "summary":"Offset and limit pagination","description":"Preserve the original input values.","acceptance":{"customfield_1":"Reject noninteger and negative bounds."}})).unwrap()
}
#[test]
fn local_json_and_toml_capture_equivalent_issue_requirements_then_review_and_deliver() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = setup(root.path());
    std::fs::write(
        repo.join("issue.json"),
        serde_json::to_vec_pretty(&issue()).unwrap(),
    )
    .unwrap();
    std::fs::write(repo.join("issue.toml"), toml::to_string(&issue()).unwrap()).unwrap();
    let original: Value =
        serde_json::from_slice(&std::fs::read(repo.join("implementation-reviewed.json")).unwrap())
            .unwrap();
    for format in ["json", "toml"] {
        let mut file = original.clone();
        file["task_id"] = json!(format!("issue-{format}"));
        file["issue"] = json!({"kind":"local","path":format!("issue.{format}")});
        std::fs::write(
            repo.join(format!("task-{format}.json")),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "capture source authority"]);
    let mut captures = vec![];
    let mut requirements = vec![];
    for format in ["json", "toml"] {
        let id = format!("issue-{format}");
        let file = format!("task-{format}.json");
        let planned = task(&repo, &state, &["task", "plan", "--file", &file], 0);
        assert_eq!(planned["attempts"], 0);
        let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
        let revision = cas
            .get_json(planned["revision_id"].as_str().unwrap())
            .unwrap();
        let input = cas
            .get_json(
                revision["payload"]["inputs"]["requirements"]["artifact_ids"][0]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(input["payload"]["text"], issue().requirements(None).text);
        let capture = input["input_artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|id| cas.get_json(id.as_str().unwrap()).ok())
            .find(|value| value["type"] == "af/TaskSourceCapture@1")
            .unwrap();
        assert_eq!(capture["payload"]["external_key"], "AF-42");
        assert_eq!(capture["payload"]["fields"].as_object().unwrap().len(), 3);
        captures.push(capture);
        requirements.push(input["payload"].clone());
        // This does not refresh a submitted Task. Execution stays on its captured source.
        std::fs::write(
            repo.join(format!("issue.{format}")),
            "changed and invalid input",
        )
        .unwrap();
        let result = task(&repo, &state, &["task", "run", &id], 0);
        assert_eq!(result["attempts"], 4);
        assert_eq!(result["result"]["acceptance"], "satisfied");
        assert_eq!(result["review_rounds"].as_array().unwrap().len(), 1);
        assert_eq!(task(&repo, &state, &["task", "run", &id], 0), result);
        git(&repo, &["restore", &format!("issue.{format}")]);
    }
    assert_eq!(requirements[0], requirements[1]);
    assert_eq!(
        captures[0]["payload"]["fields"],
        captures[1]["payload"]["fields"]
    );
    assert_ne!(
        captures[0]["payload"]["raw_source_id"],
        captures[1]["payload"]["raw_source_id"]
    );
    let output = root.path().join("delivered");
    let delivered = task(
        &repo,
        &state,
        &[
            "task",
            "deliver",
            "issue-json",
            "--branch",
            "af/issue-json",
            "--worktree",
            output.to_str().unwrap(),
            "--confirm",
            "issue-json",
        ],
        0,
    );
    assert!(
        serde_json::to_string(&delivered)
            .unwrap()
            .contains("issue-json")
    );
    assert!(
        std::fs::read_to_string(output.join("pagination.py"))
            .unwrap()
            .contains("offset:offset + limit")
    );
    assert_eq!(
        Command::new("git")
            .current_dir(&repo)
            .args(["status", "--porcelain"])
            .output()
            .unwrap()
            .stdout,
        b""
    );
}
