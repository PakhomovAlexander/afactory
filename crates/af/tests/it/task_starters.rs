//! Execute the actual emitted starter packages and their public contracts through the CLI.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn run(repo: &Path, args: &[&str], code: i32) -> Value {
    let out = crate::common::af()
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
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    assert_eq!(catalog["review"]["generation"].as_integer(), Some(2));
    for reviewer in ["correctness", "bugs"] {
        let package = &catalog["packages"][format!("builtin/{reviewer}")];
        let worker: review_config::task::catalog::TaskWorkerManifest = toml::from_str(
            &std::fs::read_to_string(
                repo.join(package["path"].as_str().unwrap())
                    .join("worker.toml"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            worker.signature.contract.inputs["subject"].artifact_type,
            "af/TaskReviewSubject@2"
        );
        assert_eq!(
            worker.signature.contract.inputs["assignment"].artifact_type,
            "af/TaskReviewAssignment@1"
        );
        assert_eq!(
            worker.signature.worker_output_type.as_deref(),
            Some("review.kernel/ReviewerResult@2")
        );
    }
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
        ("implementation-reviewed", 5),
        ("implementation-repair-targeted", 5),
        ("implementation-repair-heavy", 5),
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
        let result = task(&repo, &state, &["task", "run", "--execute", id], 0);
        assert_eq!(
            result["result"]["acceptance"], "satisfied",
            "{name}: {result:#}"
        );
        assert_eq!(result["attempts"], attempts, "{name}: {result:#}");
        assert_eq!(
            task(&repo, &state, &["task", "run", "--execute", id], 0),
            result
        );
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

/// #425 item 5: the emitted reviewed implementation names no `strategy`, `verification` or
/// `facts.standard`, and plans with the values its kind's defaults supply: the same revision as
/// the file that names them.
#[test]
fn the_reviewed_starter_leaves_its_kind_defaults_and_plans_with_them() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = setup(root.path(), None);
    let file: Value =
        serde_json::from_slice(&std::fs::read(repo.join("implementation-reviewed.json")).unwrap())
            .unwrap();
    for field in ["strategy", "verification"] {
        assert!(file.get(field).is_none(), "{field}: {file:#}");
    }
    assert_eq!(
        file["facts"],
        json!({"capability": "tutorial.pagination/1"}),
        "{file:#}"
    );
    let revision = |file: &Path, state: &Path| {
        let planned = task(
            &repo,
            state,
            &["task", "plan", "--file", file.to_str().unwrap()],
            0,
        );
        assert_eq!(planned["state"]["phase"], "planned", "{planned:#}");
        assert_eq!(
            planned["selection"]["assessment"]["decision"]["pipeline"],
            "builtin/implementation-reviewed",
            "{planned:#}"
        );
        let id = planned["revision_id"].as_str().unwrap();
        let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
        cas.get_json(id).unwrap()["payload"].clone()
    };
    let mut defaulted = revision(&repo.join("implementation-reviewed.json"), &state);
    assert_eq!(defaulted["strategy"], "reviewed");
    assert_eq!(
        defaulted["facts"],
        json!({"standard": true, "capability": "tutorial.pagination/1"})
    );
    let mut stated = file.clone();
    stated["strategy"] = json!("reviewed");
    stated["verification"] = json!("review");
    stated["facts"]["standard"] = json!(true);
    let stated_file = root.path().join("stated.json");
    std::fs::write(&stated_file, serde_json::to_vec(&stated).unwrap()).unwrap();
    let mut stated = revision(&stated_file, &root.path().join("stated-state"));
    // Each plan's absolute deadline is its own clock's.
    for revision in [&mut defaulted, &mut stated] {
        revision["limits"]
            .as_object_mut()
            .unwrap()
            .remove("deadline_unix_ms")
            .expect("a planned deadline");
    }
    for field in [
        "kind",
        "goal",
        "strategy",
        "pipeline",
        "facts",
        "limits",
        "required_outputs",
        "acceptance",
    ] {
        assert_eq!(defaulted[field], stated[field], "{field}");
    }
}

fn starter_repair_uses_current_evidence(name: &str, attempts: u64) {
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
    // Up to eleven Attempts of 5 s each inside a 180 s Task is a deadline no loaded gate host
    // keeps, and no deadline is this test's subject (ADR-0114): the emitted starter's walls
    // become the load-safe wall here, in the test's own copy, never in the shipped starter.
    crate::task_cli::load_safe_walls(&repo);
    let file = repo.join(format!("{name}.json"));
    let mut ticket: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    crate::task_cli::load_safe_limits(&mut ticket["limits"]);
    std::fs::write(&file, serde_json::to_vec_pretty(&ticket).unwrap()).unwrap();
    repin(&repo);
    // An unavailable reviewer is a failed Worker (6).
    let code = if name == "implementation-reviewed" {
        6
    } else {
        0
    };
    let file = format!("{name}.json");
    let result = task(
        &repo,
        &state,
        &["task", "start", "--execute", "--file", &file],
        code,
    );
    assert!(
        result.get("unknown_usage_attempts").is_none(),
        "{name}: an Attempt's usage is unknown (a lease lost under load is one cause): {result:#}"
    );
    assert_eq!(result["attempts"], attempts, "{name}: {result:#}");
    assert_eq!(
        result["result"]["acceptance"],
        if code == 0 {
            "satisfied"
        } else {
            "inconclusive"
        }
    );
    assert_eq!(
        task(&repo, &state, &["task", "run", "--execute", name], code),
        result
    );
    if code == 0 {
        assert_eq!(
            result["review_rounds"][0]["conclusion"],
            "changes_requested"
        );
        let expected = if name.ends_with("heavy") { 2 } else { 1 };
        assert_eq!(result["review_rounds"].as_array().unwrap().len(), expected);
    }
}

#[test]
fn targeted_starter_repair_uses_current_evidence() {
    starter_repair_uses_current_evidence("implementation-repair-targeted", 8);
}

#[test]
fn heavy_starter_repair_uses_current_evidence() {
    starter_repair_uses_current_evidence("implementation-repair-heavy", 11);
}

#[test]
fn reviewed_starter_with_a_missing_reviewer_remains_incomplete() {
    starter_repair_uses_current_evidence("implementation-reviewed", 5);
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
    let waiting = task(
        &repo,
        &state,
        &["task", "run", "--execute", "generated-pagination"],
        0,
    );
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
    let done = task(
        &repo,
        &state,
        &["task", "run", "--execute", "generated-pagination"],
        0,
    );
    assert_eq!(done["attempts"], 6);
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
        &["task", "start", "--execute", "--file", "planning.json"],
        0,
    );
    assert_eq!(reused["attempts"], 5);
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

fn reviewed_implementation_preserves_evidence(failed_review: bool) {
    use review_config::task::catalog::TaskWorkerManifest;
    use review_core::task::pipeline::{PipelineDefinitionV1, TaskOperatorV1};
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = setup(root.path(), None);
    let author = worker_path(&repo, "builtin/implementer");
    if failed_review {
        std::fs::write(&author,"import json,sys\njson.load(sys.stdin)\nopen('pagination.py','w').write('def paginate(items, offset=0, limit=2):\\n    return items[offset:offset+limit]\\n')\nprint(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Injected missing bounds check'}]}}))\n").unwrap();
    }
    let failure = repo.join(".af/independent-failure");
    std::fs::create_dir_all(failure.join("outputs")).unwrap();
    for name in ["input.schema.json", "outputs/report.schema.json"] {
        std::fs::copy(author.parent().unwrap().join(name), failure.join(name)).unwrap();
    }
    let mut worker: TaskWorkerManifest = toml::from_str(
        &std::fs::read_to_string(author.parent().unwrap().join("worker.toml")).unwrap(),
    )
    .unwrap();
    worker.name = "fixture/independent-failure".into();
    std::fs::write(
        failure.join("worker.toml"),
        toml::to_string(&worker).unwrap(),
    )
    .unwrap();
    std::fs::write(
        failure.join("worker.py"),
        "raise Exception('independent configured Worker failed')\n",
    )
    .unwrap();
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let pipeline_path = repo
        .join(
            catalog["packages"]["builtin/implementation-reviewed"]["path"]
                .as_str()
                .unwrap(),
        )
        .join("pipeline.toml");
    let mut pipeline: PipelineDefinitionV1 =
        toml::from_str(&std::fs::read_to_string(&pipeline_path).unwrap()).unwrap();
    let mut node = pipeline
        .nodes
        .iter()
        .find(|n| matches!(n.operator, TaskOperatorV1::Worker { .. }))
        .unwrap()
        .clone();
    let TaskOperatorV1::Worker { slot } = &node.operator else {
        unreachable!()
    };
    let mut binding = pipeline.slots[slot].clone();
    binding.worker = worker.name.clone();
    pipeline.slots.insert("independent_failure".into(), binding);
    node.id = "independent_failure".into();
    node.operator = TaskOperatorV1::Worker {
        slot: "independent_failure".into(),
    };
    pipeline.nodes.push(node);
    pipeline.max_attempts += 1;
    std::fs::write(pipeline_path, toml::to_string(&pipeline).unwrap()).unwrap();
    let pin = json!({"path":".af/independent-failure","version":"1.0.0","digest":review_config::lock::package_digest(&worker.name,&failure).unwrap()});
    catalog["packages"].as_table_mut().unwrap().insert(
        worker.name.clone(),
        toml::Value::try_from(pin.clone()).unwrap(),
    );
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    let path = repo.join("catalog.toml");
    let mut shared: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    shared["packages"]
        .as_table_mut()
        .unwrap()
        .insert(worker.name.clone(), toml::Value::try_from(pin).unwrap());
    std::fs::write(path, toml::to_string(&shared).unwrap()).unwrap();
    let path = repo.join("implementation-reviewed.json");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    file["limits"]["max_attempts"] = json!(6);
    std::fs::write(path, serde_json::to_vec(&file).unwrap()).unwrap();
    repin(&repo);
    // A review that asked for changes stays the verdict (3); without it, the failed independent
    // Worker is (6).
    let code = if failed_review { 3 } else { 6 };
    let result = task(
        &repo,
        &state,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            "implementation-reviewed.json",
        ],
        code,
    );
    assert_eq!(result["attempts"], 6, "{result:#}");
    assert_eq!(result["result"]["execution"], "exhausted");
    assert_eq!(
        result["result"]["acceptance"],
        if failed_review {
            "unsatisfied"
        } else {
            "inconclusive"
        }
    );
    assert_eq!(
        result["review_rounds"][0]["conclusion"],
        if failed_review {
            "changes_requested"
        } else {
            "pass"
        }
    );
    assert!(!result["result"]["evidence"].as_array().unwrap().is_empty());
    assert_eq!(
        task(
            &repo,
            &state,
            &["task", "run", "--execute", "implementation-reviewed"],
            code
        ),
        result
    );
}

#[test]
fn reviewed_implementation_preserves_evidence_when_independent_work_fails() {
    reviewed_implementation_preserves_evidence(false);
}

#[test]
fn reviewed_implementation_preserves_evidence_when_its_review_also_fails() {
    reviewed_implementation_preserves_evidence(true);
}

/// `af catalog init --profile experiment` (ADR-0132): the emitted catalog passes its own
/// contract fixtures, and its Task measures the source, measures the sealed candidate, compares
/// the two and is verified by command Workers alone, without a credential or a model.
#[test]
fn the_experiment_starter_measures_compares_and_verifies() {
    let root = tempfile::tempdir().unwrap();
    let init = run(
        root.path(),
        &[
            "catalog",
            "init",
            "--profile",
            "experiment",
            "--destination",
            "project",
        ],
        0,
    );
    assert_eq!(init["task"], "experiment.json");
    assert_eq!(init["attempts"], 0);
    let repo = root.path().join("project");
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
    let tested = run(&repo, &["catalog", "test", "--source", "."], 0);
    assert_eq!(tested["contract_fixtures"], "passed");
    assert_eq!(
        tested["pipelines"],
        json!(["builtin/experiment", "builtin/experiment-trial"])
    );
    let state = root.path().join("state");
    let planned = task(
        &repo,
        &state,
        &["task", "plan", "--file", "experiment.json"],
        0,
    );
    assert_eq!(planned["attempts"], 0);
    let finished = task(
        &repo,
        &state,
        &[
            "task",
            "run",
            "experiment",
            "--confirm-plan",
            planned["plan_id"].as_str().unwrap(),
        ],
        0,
    );
    assert_eq!(finished["result"]["domain_conclusion"], "verified");
    for port in ["baseline", "candidate", "comparison"] {
        assert!(finished["result"]["outputs"][port].is_object(), "{port}");
    }
    let markdown = root.path().join("comparison.md");
    task(
        &repo,
        &state,
        &[
            "task",
            "output",
            "experiment",
            "--port",
            "comparison",
            "--format",
            "markdown",
            "--output",
            markdown.to_str().unwrap(),
        ],
        0,
    );
    let text = std::fs::read_to_string(markdown).unwrap();
    assert!(
        text.contains(
            "| bytes_written (objective) | bytes | 100 | 80 | 20 | 0.2 | 3 / 3 | improved |"
        ),
        "{text}"
    );
}

#[test]
fn pi_light_family_compiles_current_receipts_exact_roles_and_local_checks() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = setup(root.path(), None);
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let home = root.path().join("private-home");
    let (sdk, path, pi_state) = crate::fake_models::install_pi(root.path(), &home, "kimi-document");
    let bin = PathBuf::from(&path);
    let codex_auth = home.join("codex");
    crate::fake_models::install("codex", &bin, &codex_auth);
    // The real kernel check is launched through env. Planning must resolve it even though
    // this fixture never executes a check, a paid admission, or a Worker.
    for (name, source) in [("env", "/usr/bin/env"), ("bash", "/bin/bash")] {
        std::os::unix::fs::symlink(source, bin.join(name)).unwrap();
    }
    std::fs::write(bin.join("npx"), "#!/bin/sh\nexit 99\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("npx"), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(home.join("providers.toml"), toml::to_string(&json!({"version":3,"providers":[
        {"id":"pi-kimi","kind":"pi","backend":"kimi-coding","auth_dir":home,"sdk_entrypoint":sdk},
        {"id":"codex-main","kind":"codex","auth_dir":codex_auth}
    ]})).unwrap()).unwrap();
    crate::task_cli::copy_tree(
        &workspace.join(".af/task-packages/pi-light"),
        &repo.join(".af/task-packages/pi-light"),
    );
    std::fs::copy(
        workspace.join(".af/code-policy.toml"),
        repo.join(".af/code-policy.toml"),
    )
    .unwrap();
    let source_catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(workspace.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    for name in [
        "implementer",
        "correctness",
        "evaluator",
        "implementation",
        "verification",
        "review",
    ] {
        let key = format!("pi-light/{name}");
        catalog["packages"]
            .as_table_mut()
            .unwrap()
            .insert(key.clone(), source_catalog["packages"][&key].clone());
    }
    for name in ["implementer", "correctness", "evaluator"] {
        let key = format!("pi-light/{name}");
        catalog["providers"]
            .as_table_mut()
            .unwrap()
            .insert(key.clone(), source_catalog["providers"][&key].clone());
    }
    // Every copied kind mapping must retain its exact package authority, including
    // kinds this fixture does not select. Planning validates the complete catalog.
    for kind in source_catalog["kinds"].as_table().unwrap().values() {
        let name = kind.as_str().unwrap();
        let kind_pin = &source_catalog["packages"][name];
        let kind_path = kind_pin["path"].as_str().unwrap();
        crate::task_cli::copy_tree(&workspace.join(kind_path), &repo.join(kind_path));
        catalog["packages"]
            .as_table_mut()
            .unwrap()
            .insert(name.into(), kind_pin.clone());
    }
    catalog
        .as_table_mut()
        .unwrap()
        .insert("kinds".into(), source_catalog["kinds"].clone());
    for key in ["provider_admission", "independence", "review"] {
        catalog
            .as_table_mut()
            .unwrap()
            .insert(key.into(), source_catalog[key].clone());
    }
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    let template: Value =
        serde_json::from_slice(&std::fs::read(repo.join("implementation-reviewed.json")).unwrap())
            .unwrap();
    for kind in ["implementation", "verification"] {
        let mut file = template.clone();
        file["task_id"] = json!(format!("pi-light-{kind}"));
        file["kind"] = json!(if kind == "implementation" {
            "implement"
        } else {
            "verification"
        });
        file["pipeline"] = json!({"name":format!("pi-light/{kind}"),"fallback":"refuse"});
        file["facts"]["standard"] = json!(true);
        file["limits"] = json!({"tokens":3000000,"max_attempts":16,"wall_ms":43200000,
            "verification":{"tokens":1500000,"attempts":8,"wall_ms":21600000}});
        std::fs::write(
            repo.join(format!("pi-light-{kind}.json")),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();
    }
    commit(&repo);
    for kind in ["implementation", "verification"] {
        let output = crate::common::af()
            .current_dir(&repo)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("PATH", &path)
            .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
            .args([
                "task",
                "plan",
                "--file",
                &format!("pi-light-{kind}.json"),
                "--json",
                "--state",
            ])
            .arg(&state)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
        let envelope = cas.get_json(result["plan_id"].as_str().unwrap()).unwrap();
        assert_eq!(envelope["type"], "af/ExecutionPlan@3");
        let plan: review_core::task::plan::ExecutionPlanV3 =
            serde_json::from_value(envelope["payload"].clone()).unwrap();
        plan.validate().unwrap();
        let bindings = &envelope["payload"]["bindings"];
        let expected = if kind == "implementation" { 3 } else { 2 };
        assert_eq!(bindings.as_object().unwrap().len(), expected);
        for (slot, model, effort) in [
            ("correctness", "gpt-6-sol", "high"),
            ("evaluator", "gpt-6-astra", "medium"),
        ] {
            let execution = &bindings[format!("root.slots.{slot}")]["execution"];
            assert_eq!(execution["provider_kind"], "codex");
            assert_eq!(execution["model"], model);
            assert_eq!(execution["effort"], effort);
        }
        if kind == "implementation" {
            let execution = &bindings["root.slots.implementer"]["execution"];
            assert_eq!(execution["model"], "k3");
            assert_eq!(execution["effort"], "high");
            assert_eq!(execution["pi"]["backend"], "kimi-coding");
            assert_eq!(execution["pi"]["identity_mode"], "unavailable");
            assert!(execution["principal_id"].is_null());
        }
        let graph = cas.get_json(&plan.compiled_graph_id).unwrap();
        let graph_text = graph.to_string();
        assert!(graph_text.contains("af/TaskCheckReceipt@2"));
        assert!(!graph_text.contains("af/TaskCheckReceipt@1"));
        assert!(graph_text.contains("markdownlint") && graph_text.contains("kernel"));
        assert!(!graph_text.contains("publish-gate"));
    }
    // The trusted policy, rather than a profile alias or backend difference, controls
    // whether unavailable identity can be admitted. No paid probe is run to discover this.
    catalog["independence"]["distinct_principals"] = toml::Value::Boolean(true);
    std::fs::write(
        repo.join(".af/task-catalog.toml"),
        toml::to_string(&catalog).unwrap(),
    )
    .unwrap();
    let mut strict_file: Value =
        serde_json::from_slice(&std::fs::read(repo.join("pi-light-implementation.json")).unwrap())
            .unwrap();
    strict_file["task_id"] = json!("pi-strict-principals");
    std::fs::write(
        repo.join("pi-strict.json"),
        serde_json::to_vec(&strict_file).unwrap(),
    )
    .unwrap();
    commit(&repo);
    let refused = crate::common::af()
        .current_dir(&repo)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("PATH", &path)
        .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
        .args([
            "task",
            "plan",
            "--file",
            "pi-strict.json",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        format!(
            "{}{}",
            String::from_utf8_lossy(&refused.stdout),
            String::from_utf8_lossy(&refused.stderr)
        )
        .contains("known account identities")
    );
    assert!(!pi_state.with_extension("json.calls").exists());
    assert!(crate::fake_models::turns(&codex_auth).is_empty());
}
