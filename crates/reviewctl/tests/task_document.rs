//! The shipped data-only starter exercises the real compiler, Store and independent verifier.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
#[path = "task_document/provider_admission.rs"]
mod provider_admission;

fn af(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap()
}
fn success(output: std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn commit(repo: &Path) {
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "captured document definitions"],
    ] {
        let out = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
fn setup(root: &Path) -> (PathBuf, PathBuf) {
    let init = success(af(
        root,
        &["catalog", "init", "--destination", "project", "--json"],
    ));
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
fn run(repo: &Path, state: &Path, args: &[&str], code: i32) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
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

#[test]
fn document_starter_runs_without_credentials_code_artifacts_or_code_checks_and_replays_exactly() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = setup(root.path());
    let catalog = std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap();
    assert!(!catalog.contains("code_policy"));
    assert!(
        !af(
            root.path(),
            &["catalog", "init", "--destination", "project", "--json"]
        )
        .status
        .success()
    );
    assert_eq!(
        std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap(),
        catalog
    );
    assert_eq!(
        success(af(&repo, &["catalog", "test", "--source", ".", "--json"]))["attempts"],
        0
    );
    let planned = run(
        &repo,
        &state,
        &["task", "plan", "--file", "document.json"],
        0,
    );
    assert_eq!(planned["attempts"], 0);
    assert_eq!(
        planned["plan"]["inputs"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["requirements", "sources"]
    );
    assert_eq!(planned["plan"]["authority"]["allowed_effects"], json!([]));
    assert!(!planned["plan"].to_string().contains("af/SourceTree@1"));
    assert!(!planned["plan"].to_string().contains("af/CandidateTree@1"));
    std::fs::write(
        repo.join("sources.json"),
        "changed and unavailable live source",
    )
    .unwrap();
    std::fs::write(repo.join(".af/task-catalog.toml"), "changed authority").unwrap();
    let done = run(&repo, &state, &["task", "run", "release-notes"], 0);
    assert_eq!(done["attempts"], 3);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    assert!(done["result"]["outputs"]["snapshot"].is_null());
    let document_id = done["result"]["outputs"]["document"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let document: review_core::ArtifactEnvelope =
        serde_json::from_value(cas.get_json(document_id).unwrap()).unwrap();
    assert_eq!(document.artifact_type, "af/Document@1");
    assert!(document.subject_snapshot_id.is_none());
    let text = document.payload["text"].as_str().unwrap();
    assert!(text.contains("offset and a limit") && text.contains("https://example.invalid/AF-42"));
    let file = root.path().join("release-notes.md");
    let output = run(
        &repo,
        &state,
        &[
            "task",
            "output",
            "release-notes",
            "--port",
            "document",
            "--format",
            "markdown",
            "--output",
            file.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(output["artifact_id"], document_id);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    let refused = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "output",
            "release-notes",
            "--port",
            "document",
            "--format",
            "markdown",
            "--output",
        ])
        .arg(&file)
        .arg("--state")
        .arg(&state)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert_eq!(
        run(&repo, &state, &["task", "run", "release-notes"], 0),
        done
    );
}

#[test]
fn document_failures_keep_negative_or_missing_evidence_and_never_accept_a_stale_draft() {
    for case in [
        "missing_section",
        "unsafe_link",
        "stale_document",
        "negative",
        "missing_verifier",
    ] {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = setup(root.path());
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        match case {
            "missing_section" => {
                let path = repo.join(".af/document-policy.toml");
                let mut policy: toml::Value =
                    toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                policy["required_sections"] =
                    toml::Value::try_from(vec!["Summary", "Compatibility"]).unwrap();
                std::fs::write(&path, toml::to_string(&policy).unwrap()).unwrap();
                // Bind the verifier's declared evidence to the newly captured exact policy.
                let typed: review_pipeline::task::document::DocumentTaskPolicy =
                    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
                let id = review_store::canonical::content_id(&serde_json::to_value(typed).unwrap())
                    .unwrap();
                let path = repo
                    .join(
                        catalog["packages"]["builtin/document-verifier"]["path"]
                            .as_str()
                            .unwrap(),
                    )
                    .join("worker.toml");
                let mut worker: review_config::task::catalog::TaskWorkerManifest =
                    toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                worker
                    .signature
                    .evidence
                    .insert("result".into(), std::collections::BTreeSet::from([id]));
                std::fs::write(path, toml::to_string(&worker).unwrap()).unwrap();
            }
            "unsafe_link" => {
                let path = repo.join("sources.json");
                let mut value: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                value["sources"]["pagination"]["uri"] = json!("javascript:alert(1)");
                std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            _ => {
                let path = repo
                    .join(
                        catalog["packages"]["builtin/document-verifier"]["path"]
                            .as_str()
                            .unwrap(),
                    )
                    .join("worker.py");
                let old = std::fs::read_to_string(&path).unwrap();
                let new = match case {
                    "stale_document" => old.replace(
                        "'document_id':i['document']['artifact_id']",
                        "'document_id':i['sources']['artifact_id']",
                    ),
                    "negative" => old.replace("'passed' if accepted else 'failed'", "'failed'"),
                    "missing_verifier" => "raise Exception('verifier unavailable')\n".into(),
                    _ => unreachable!(),
                };
                std::fs::write(path, new).unwrap();
            }
        }
        for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
            pin["digest"] = toml::Value::String(
                review_config::lock::package_digest(
                    name,
                    &repo.join(pin["path"].as_str().unwrap()),
                )
                .unwrap(),
            );
        }
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        commit(&repo);
        let code = if matches!(case, "stale_document" | "missing_verifier") {
            4
        } else {
            3
        };
        let done = run(
            &repo,
            &state,
            &["task", "start", "--file", "document.json"],
            code,
        );
        assert_ne!(done["result"]["acceptance"], "satisfied", "{case}");
        assert_eq!(
            done["attempts"],
            if matches!(case, "missing_section" | "unsafe_link") {
                2
            } else {
                3
            },
            "{case}"
        );
        assert_eq!(
            run(&repo, &state, &["task", "run", "release-notes"], code),
            done,
            "{case}"
        );
    }
}
