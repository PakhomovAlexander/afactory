//! Native accepted-candidate export over an ordinary command-backed Task; no model/service.
use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

#[test]
fn ordinary_accepted_candidate_export_preserves_bytes_and_refuses_forged_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(directory.path(), true);
    let (code, stdout, stderr) = run_task(&repo, &home, &state);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let outcome: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(outcome["result"]["acceptance"], "satisfied");
    let task_id = outcome["task_id"].as_str().unwrap();
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task = store.task_projection(&cas, task_id).unwrap().unwrap();
    let review_core::task::TaskPhaseV1::Finished { result_id } = &task.phase else {
        panic!()
    };
    let source_commit = Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let source_commit = String::from_utf8(source_commit.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let parent = directory
        .path()
        .canonicalize()
        .unwrap()
        .join("private-captures");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let request_path = parent.join("request.json");
    let into = parent.join("candidate");
    let request = json!({"schema":"af/git-candidate-source-request@1",
        "receipt":{"task_id":task_id,"plan_id":task.plan_id,"revision_id":task.revision_id,
            "result_id":result_id,"charged_tokens":outcome["chargeable_tokens"].as_str().unwrap().parse::<u64>().unwrap(),"result":outcome["result"]},
        "source_commit":source_commit,"candidate_snapshot_id":outcome["result"]["outputs"]["snapshot"]["snapshot_id"],
        "predecessor_snapshot_id":null});
    crate::schemas::valid(
        &crate::schemas::validator("git-candidate-source-request-v1.json"),
        &request,
    );
    let export = || {
        run_af(
            &repo,
            &home,
            &[
                "scenario",
                "candidate-source",
                "--file",
                request_path.to_str().unwrap(),
                "--repo",
                repo.to_str().unwrap(),
                "--task-state",
                state.to_str().unwrap(),
                "--into",
                into.to_str().unwrap(),
                "--json",
            ],
        )
    };
    let write =
        |value: &Value| std::fs::write(&request_path, serde_json::to_vec(value).unwrap()).unwrap();
    for field in ["result_id", "plan_id", "revision_id"] {
        let mut forged = request.clone();
        forged["receipt"][field] = json!(format!("sha256:{}", "f".repeat(64)));
        write(&forged);
        assert_ne!(export().0, 0, "accepted forged {field}");
        assert!(!into.exists());
    }
    write(&request);
    let (code, stdout, stderr) = export();
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let receipt: Value = serde_json::from_str(&stdout).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("git-candidate-source-v1.json"),
        &receipt,
    );
    assert_eq!(
        receipt["candidate_snapshot"],
        request["candidate_snapshot_id"]
    );
    assert_eq!(
        export().1,
        stdout,
        "restart must return the same exact object receipt"
    );
    let objects = receipt["repository"].as_str().unwrap();
    let commit = receipt["transport_commit"].as_str().unwrap();
    let contents = Command::new("git")
        .arg("--git-dir")
        .arg(objects)
        .args(["show", &format!("{commit}:implemented.txt")])
        .output()
        .unwrap();
    assert!(contents.status.success());
    assert_eq!(contents.stdout, b"derived\n");
    assert!(!repo.join("implemented.txt").exists());
    assert!(task.deliveries.is_empty(), "export is not Task delivery");
    for slot in 0..4 {
        let stage = parent.join(format!("af-candidate-stage-uncertain-{slot}"));
        std::fs::create_dir(&stage).unwrap();
        std::fs::write(stage.join("foreign"), b"retain uncertain native staging").unwrap();
    }
    let (code, _, diagnostic) = export();
    assert_ne!(code, 0);
    assert!(
        diagnostic.contains("staging budget exhausted"),
        "{diagnostic}"
    );
    for slot in 0..4 {
        assert_eq!(
            std::fs::read(parent.join(format!("af-candidate-stage-uncertain-{slot}/foreign")))
                .unwrap(),
            b"retain uncertain native staging"
        );
    }
}

#[test]
fn privately_installed_workspace_adapter_prepares_native_accepted_candidate_without_service() {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use std::process::Stdio;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let (repo, home, state) = fixture(&root, true);
    let (code, stdout, stderr) = run_task(&repo, &home, &state);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let outcome: Value = serde_json::from_str(&stdout).unwrap();
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task = store
        .task_projection(&cas, outcome["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let review_core::task::TaskPhaseV1::Finished { result_id } = &task.phase else {
        panic!()
    };
    let source = task.revision.inputs["source"].snapshot_id.as_ref().unwrap();
    let (source, _) = review_source_git::task::read_snapshot(&cas, source).unwrap();
    let origin = review_source_git::task::read_origin(&cas, &source.origin_id).unwrap();
    let machine = root.join("machine");
    std::fs::create_dir(&machine).unwrap();
    std::fs::set_permissions(&machine, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tools = machine.join("tools");
    let worktrees = machine.join("worktrees");
    for path in [&tools, &worktrees] {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // Private installation, never an af binary inheriting the developer's real home.
    let binary = crate::common::privately_installed_af(&tools);
    let hash = |path: &Path| {
        format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(std::fs::read(path).unwrap()))
        )
    };
    let ssh = machine.join("ssh-config");
    std::fs::write(&ssh, "Host github.com\n  BatchMode yes\n").unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o600)).unwrap();
    let git = std::fs::canonicalize("/usr/bin/git").unwrap();
    let config = json!({"schema":"af/git-workspace-config@1", "repository":"octo/documentation","repository_id":17,
        "actor":{"id":7,"login":"automation"}, "token_file":machine.join("absent-token"),"state_dir":machine.join("receipts"),
        "repo":repo,"task_store":state,"worktree_root":worktrees,"base":"main","branch_prefix":"accepted",
        "source_commit":origin.source_revision().unwrap(),"source_content_digest":source.content_digest,
        "af":binary,"af_digest":hash(&binary),"git":git,"git_digest":hash(&git),"ssh_config":ssh,"ssh_config_digest":hash(&ssh),
        "author_name":"Fixture","author_email":"fixture@example.invalid","required_checks":[{"kind":"check","name":"CI","app_id":4}],
        "reviewers":[],"publication_mode":"exclusive_owned_writer","operations":["prepare_candidate_v1"]});
    let config_path = machine.join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let executable = tools.join("workspace-adapter");
    let installed = Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(workspace.join("scripts/install-scenario-git-workspace.py"))
        .arg("--config")
        .arg(&config_path)
        .arg("--output")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let receipt = json!({"task_id":task.task_id,"plan_id":task.plan_id,"revision_id":task.revision_id,"result_id":result_id,
        "charged_tokens":0,"result":outcome["result"]});
    let text = |value: Value| json!({"type":"text","value":value});
    let request = json!({"schema":"af/scenario-adapter-request@3","method":"execute","intent_id":format!("sha256:{}","a".repeat(64)),
        "input_digest":format!("sha256:{}","b".repeat(64)),"operation":"prepare_candidate_v1","destination":"github:octo/documentation",
        "inputs":{"subject":text(json!(result_id)),"revision":text(outcome["result"]["outputs"]["snapshot"]["snapshot_id"].clone()),
            "body":text(json!("{\"schema\":\"af/git-workspace-content@1\",\"title\":\"Accepted pagination\",\"body\":\"Exact candidate\"}")),
            "candidate":{"type":"task","value":receipt}}});
    let call = || {
        let mut child = Command::new(&executable)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let response = call();
    assert_eq!(response["outcome"], "done", "{response:#}");
    assert_eq!(call(), response);
    let prepared: Value = serde_json::from_str(response["evidence"].as_str().unwrap()).unwrap();
    let worktree = Path::new(prepared["worktree"].as_str().unwrap());
    assert_eq!(
        std::fs::read(worktree.join("implemented.txt")).unwrap(),
        b"derived\n"
    );
    assert!(worktree.join("target").is_dir());
    assert!(
        !machine.join("absent-token").exists(),
        "preparation needs no service credential"
    );
}
