//! Real native compilation and token-free planning of the staged operator catalog.
use crate::{common, fake_models, task_cli};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
fn git(repo: &Path, args: &[&str]) {
    let o = std::process::Command::new("git")
        .current_dir(repo)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
fn install(repo: &Path) {
    let root = workspace();
    let staged = root.join("fixtures/issue-workflows");
    task_cli::copy_tree(&staged, &repo.join("fixtures/issue-workflows"));
    task_cli::copy_tree(
        &staged.join("task-packages"),
        &repo.join(".af/task-packages"),
    );
    for policy in ["code-policy.toml", "report-policy.toml"] {
        std::fs::copy(root.join(".af").join(policy), repo.join(".af").join(policy)).unwrap();
    }
    std::fs::copy(
        staged.join("replacement-catalog.toml"),
        repo.join(".af/task-catalog.toml"),
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 2\n[project]\nname = \"fixture\"\n",
    )
    .unwrap();
    if !repo.join("README.md").exists() {
        std::fs::write(repo.join("README.md"), "# Fixture\n").unwrap();
    }
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "staged catalog"]);
}
fn text(o: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn catalog_contracts_and_all_seven_native_plans_preserve_roles_gates_and_stage_dataflow() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = crate::task_report::starter(temp.path());
    let original = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let original = String::from_utf8(original.stdout).unwrap();
    install(&repo);
    // A real, independently accepted command-fixture Report Task on this exact source.
    // Its old authority chooses only hermetic command Workers; no production policy is weakened.
    let report = common::af()
        .current_dir(&repo)
        .args([
            "task",
            "start",
            "--execute",
            "--file",
            "report.json",
            "--authority",
            original.trim(),
            "--json",
        ])
        .arg("--state")
        .arg(&state)
        .output()
        .unwrap();
    assert!(report.status.success(), "{}", text(&report));
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report["result"]["acceptance"], "satisfied");
    let report_task = report["task_id"].as_str().unwrap();
    let o = common::af()
        .current_dir(&repo)
        .args([
            "catalog",
            "test",
            "--source",
            ".",
            "--manifest",
            "fixtures/issue-workflows/catalog.toml",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let tested: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(tested["attempts"], 0);
    assert_eq!(tested["contract_fixtures"], "passed");
    let home = temp.path().join("home");
    let (sdk, path, _) = fake_models::install_pi(temp.path(), &home, "kimi-document");
    let sdk_text = std::fs::read_to_string(&sdk)
        .unwrap()
        .replace("id: 'k3'", "id: 'k3.1-pro'");
    std::fs::write(&sdk, sdk_text).unwrap();
    let bin = PathBuf::from(&path);
    let auth = home.join("codex");
    fake_models::install("codex", &bin, &auth);
    for (name, source) in [("env", "/usr/bin/env"), ("bash", "/bin/bash")] {
        std::os::unix::fs::symlink(source, bin.join(name)).unwrap();
    }
    std::fs::write(bin.join("npx"), "#!/bin/sh\nexit 99\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("npx"), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        home.join("providers.toml"),
        toml::to_string(&json!({"version":4,"providers":[
      {"id":"pi-personal","kind":"pi","backend":"kimi-coding","auth_dir":home,"sdk_entrypoint":sdk},
      {"id":"codex-personal","kind":"codex","auth_dir":auth}]}))
        .unwrap(),
    )
    .unwrap();
    let selections: Value = serde_json::from_slice(
        &std::fs::read(repo.join("fixtures/issue-workflows/selections.json")).unwrap(),
    )
    .unwrap();
    let plan = |name: &str, id: &str, evidence: bool| {
        let file = temp.path().join(format!("{id}.json"));
        let mut task = json!({"schema":"af.task-file/1","task_id":id,"kind":"implement","goal":"Resolve one bounded issue against the pinned source","pipeline":{"name":name,"fallback":"refuse"},"strategy":"small","verification":"review","facts":{"standard":true},"limits":{"tokens":16000000,"max_attempts":64,"wall_ms":172800000,"verification":{"tokens":8000000,"attempts":40,"wall_ms":86400000}}});
        if evidence
            && (name.contains("research") || name.contains("design"))
            && !name.starts_with("issue-internal/")
        {
            task["inputs"] = json!({"research_report":{"task":report_task,"port":"report"}});
        }
        if name == "issue-internal/report" || name == "issue-internal/design" {
            task["kind"] = json!("report");
            task["strategy"] = json!("report");
            task.as_object_mut().unwrap().remove("verification");
        }
        if name == "issue-internal/verification" {
            task["kind"] = json!("verification");
            task["strategy"] = json!("light");
            task["verification"] = json!("evaluation");
        }
        std::fs::write(&file, serde_json::to_vec(&task).unwrap()).unwrap();
        common::af()
            .current_dir(&repo)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("AF_PI_HOST_MODULE", &sdk)
            .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
            .env("PATH", &path)
            .args(["task", "plan", "--file"])
            .arg(file)
            .arg("--state")
            .arg(&state)
            .arg("--json")
            .output()
            .unwrap()
    };
    for (i, (name, _)) in selections["selections"]
        .as_object()
        .unwrap()
        .iter()
        .enumerate()
    {
        let o = plan(name, &format!("selection-{i}"), true);
        assert!(o.status.success(), "{name}: {}", text(&o));
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        let bindings = v["plan"]["bindings"].as_object().unwrap();
        assert!(!bindings.is_empty());
        let light = name.contains("/light-");
        for (slot, b) in bindings {
            let role = slot.rsplit('.').next().unwrap();
            let (model, effort) = match role {
                "implementer" if name.contains("kimi") => ("k3.1-pro", "high"),
                "implementer" => ("gpt-6.1-sol", "medium"),
                "bugs" => ("gpt-6.1-terra", "high"),
                "correctness" if light => ("gpt-6.1-terra", "high"),
                "correctness" => ("gpt-6-sol", "high"),
                "evaluator" if light => ("gpt-6-sol", "medium"),
                "evaluator" | "measurement_evaluator" => ("gpt-6-astra", "medium"),
                "author" if name.contains("design") => ("gpt-6-astra", "medium"),
                "author" => ("gpt-6.1-sol", "high"),
                "verifier" => ("gpt-6.1-terra", "high"),
                _ => panic!("unexpected role {slot}"),
            };
            assert_eq!(b["execution"]["model"], model, "{name} {slot}");
            assert_eq!(b["execution"]["effort"], effort);
            assert_eq!(
                b["execution"]["provider"],
                if model == "k3.1-pro" {
                    "pi-personal"
                } else {
                    "codex-personal"
                }
            );
        }
        let graph = &v["graph"];
        let rendered = graph.to_string();
        assert!(rendered.contains("markdownlint") && rendered.contains("kernel"));
        assert!(
            !rendered.contains("publish-gate")
                && !rendered.contains("codex-work")
                && !rendered.contains("uix")
        );
        if name.contains("research") || name.contains("design") {
            assert_eq!(
                graph["nodes"]["root.nodes.implement"]["inputs"]["research_report"]["port"],
                "research_report"
            );
            let implement = &graph["nodes"]["root.nodes.implement"];
            assert!(
                implement.to_string().contains("research_report"),
                "{implement}"
            );
        }
        if name.starts_with("perf/") {
            assert!(
                rendered.contains("\"op\":\"measure\"") && rendered.contains("\"op\":\"compare\"")
            );
        }
        let inspection = temp.path().join("inspection.json");
        std::fs::write(&inspection, serde_json::to_vec(&v).unwrap()).unwrap();
        let checked = std::process::Command::new("python3")
            .arg("-I")
            .arg(workspace().join("scripts/issue-workflows.py"))
            .args(["check-plan", name])
            .arg(&inspection)
            .output()
            .unwrap();
        assert!(checked.status.success(), "{name}: {}", text(&checked));
        if i == 0 {
            for field in ["model", "effort", "provider"] {
                let mut altered = v.clone();
                altered["plan"]["bindings"]["root.slots.implementer"]["execution"][field] =
                    json!(if field == "provider" {
                        "codex-work"
                    } else {
                        "wrong"
                    });
                std::fs::write(&inspection, serde_json::to_vec(&altered).unwrap()).unwrap();
                let checked = std::process::Command::new("python3")
                    .arg("-I")
                    .arg(workspace().join("scripts/issue-workflows.py"))
                    .args(["check-plan", name])
                    .arg(&inspection)
                    .output()
                    .unwrap();
                assert!(!checked.status.success(), "changed {field} must refuse");
            }
            let mut altered = v.clone();
            altered["plan"]["bindings"]
                .as_object_mut()
                .unwrap()
                .remove("root.slots.evaluator");
            std::fs::write(&inspection, serde_json::to_vec(&altered).unwrap()).unwrap();
            let checked = std::process::Command::new("python3")
                .arg("-I")
                .arg(workspace().join("scripts/issue-workflows.py"))
                .args(["check-plan", name])
                .arg(&inspection)
                .output()
                .unwrap();
            assert!(!checked.status.success(), "missing evaluator must refuse");
        }
        assert_eq!(v["attempts"], 0);
    }
    for (i, old) in [
        "default/implementation",
        "default-light/implementation",
        "pi-light/implementation",
        "warm/implementation-reviewed",
        "kernel/implementation-reviewed",
    ]
    .into_iter()
    .enumerate()
    {
        let o = plan(old, &format!("old-{i}"), false);
        assert!(!o.status.success(), "{old} unexpectedly selectable");
    }
    for (i, name) in [
        "issue-internal/report",
        "issue-internal/design",
        "issue-internal/verification",
    ]
    .into_iter()
    .enumerate()
    {
        let o = plan(name, &format!("stage-{i}"), false);
        assert!(o.status.success(), "{name}: {}", text(&o));
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        if name.ends_with("verification") {
            assert!(!v["graph"]["slots"].to_string().contains("implementer"));
        } else {
            let graph = v["graph"].to_string();
            assert!(graph.contains("report_accept") && graph.contains("report_check"));
        }
    }
    for (i, name) in [
        "bugfix/research-fix-codex",
        "impl/design-codex",
        "perf/research-experiment-codex",
    ]
    .into_iter()
    .enumerate()
    {
        let o = plan(name, &format!("missing-{i}"), false);
        assert!(
            !o.status.success(),
            "missing accepted Document must refuse: {name}"
        );
    }
    // Capture real template pins, compose every public route, and ask the native Scenario
    // validator to check the full typed dataflow and authority. No Scenario is executed.
    let commit = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let commit = String::from_utf8(commit.stdout).unwrap();
    for (i, (name, choice)) in selections["selections"]
        .as_object()
        .unwrap()
        .iter()
        .enumerate()
    {
        let mut stages = Vec::new();
        if !choice["research_pipeline"].is_null() {
            stages.push(if name.contains("design") {
                "stage-1.json".to_owned()
            } else {
                "stage-0.json".to_owned()
            });
        }
        stages.push(format!("selection-{i}.json"));
        stages.push("stage-2.json".to_owned());
        let mut pins = Vec::new();
        let mut ids = Vec::new();
        for (j, file) in stages.iter().enumerate() {
            let mut document: Value =
                serde_json::from_slice(&std::fs::read(temp.path().join(file)).unwrap()).unwrap();
            document.as_object_mut().unwrap().remove("inputs");
            document["base"] = json!({"commit":commit.trim()});
            let file = temp.path().join(format!("template-{i}-{j}.json"));
            std::fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
            let pinned = common::af()
                .current_dir(&repo)
                .env("HOME", &home)
                .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
                .env("PATH", &path)
                .args(["scenario", "template-pin", "--file"])
                .arg(file)
                .arg("--repo")
                .arg(&repo)
                .arg("--task-state")
                .arg(&state)
                .args(["--authority-commit", commit.trim(), "--json"])
                .output()
                .unwrap();
            assert!(pinned.status.success(), "{name}: {}", text(&pinned));
            let value: Value = serde_json::from_slice(&pinned.stdout).unwrap();
            ids.push(value["template_id"].clone());
            let pin = temp.path().join(format!("pin-{i}-{j}.json"));
            std::fs::write(&pin, pinned.stdout).unwrap();
            pins.push(pin);
        }
        let output = std::process::Command::new("python3")
            .arg("-I")
            .arg(workspace().join("scripts/issue-workflows.py"))
            .args(["compose", name])
            .args(&pins)
            .arg("--routing")
            .arg(workspace().join("fixtures/issue-workflows/routing-known.json"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output));
        let bundle = temp.path().join(format!("bundle-{i}.json"));
        std::fs::write(&bundle, output.stdout).unwrap();
        let policy = temp.path().join(format!("policy-{i}.json"));
        std::fs::write(&policy,serde_json::to_vec(&json!({"schema":"af/scenario-policy@5","adapters":{},"task_plans":[],"task_templates":ids,"file_roots":[],"decision_keys":{}})).unwrap()).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let validated = common::af()
            .args(["scenario", "validate", "--file"])
            .arg(bundle)
            .arg("--authority")
            .arg(policy)
            .arg("--json")
            .output()
            .unwrap();
        assert!(validated.status.success(), "{name}: {}", text(&validated));
    }
    // Exact artifacts cannot impersonate an accepted native report predecessor.
    let mut raw: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("missing-0.json")).unwrap())
            .unwrap();
    raw["task_id"] = json!("raw-research");
    raw["inputs"] = json!({"research_report":{"artifact":report["result"]["outputs"]["report"]["artifact_ids"][0]}});
    let file = temp.path().join("raw.json");
    std::fs::write(&file, serde_json::to_vec(&raw).unwrap()).unwrap();
    let refused = common::af()
        .current_dir(&repo)
        .args(["task", "plan", "--file"])
        .arg(file)
        .arg("--state")
        .arg(&state)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(text(&refused).contains("only a recorded Task"));
    // Delete actual comparison evidence in copied fixture authority, then re-pin that copy.
    // No gate assertion, model contract, production authority or historical Task is relaxed.
    let trial = repo.join(".af/task-packages/issue-internal/experiment-trial/pipeline.toml");
    let mut definition: toml::Value =
        toml::from_str(&std::fs::read_to_string(&trial).unwrap()).unwrap();
    definition["nodes"]
        .as_array_mut()
        .unwrap()
        .retain(|n| n["id"].as_str() != Some("compare"));
    std::fs::write(&trial, toml::to_string(&definition).unwrap()).unwrap();
    let catalog = repo.join(".af/task-catalog.toml");
    let mut installed: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog).unwrap()).unwrap();
    installed["packages"]["issue-internal/experiment-trial"]["digest"] = toml::Value::String(
        af_pipeline_config::lock::package_digest(
            "issue-internal/experiment-trial",
            trial.parent().unwrap(),
        )
        .unwrap(),
    );
    std::fs::write(catalog, toml::to_string(&installed).unwrap()).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "negative comparison fixture"]);
    let refused = plan("perf/research-experiment-codex", "missing-comparison", true);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("Condition must refer to another declared receipt producer"),
        "{}",
        text(&refused)
    );
    assert_eq!(fake_models::turns(&auth), "");
}

#[test]
fn pilot_manifest_schema_matches_fixture_and_refuses_unknown_or_lost_identity() {
    let value: Value = serde_json::from_slice(
        &std::fs::read(workspace().join("fixtures/issue-workflows/pilot.example.json")).unwrap(),
    )
    .unwrap();
    let validator = crate::schemas::validator("issue-workflow-pilot-v1.json");
    crate::schemas::valid(&validator, &value);
    for key in ["issue", "source_commit", "tasks"] {
        let mut bad = value.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(!validator.is_valid(&bad), "missing {key}");
    }
    let mut bad = value.clone();
    bad["selection"] = json!("default/implementation");
    assert!(!validator.is_valid(&bad));
    let mut bad = value;
    bad["unexpected"] = json!(true);
    assert!(!validator.is_valid(&bad));
}
