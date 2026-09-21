//! The CLI selects from exact shared definitions without creating a Planner invocation.
use review_core::task::pipeline::PipelineDefinitionV1;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
#[path = "support/task_cli.rs"]
mod task_cli;

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn commit(repo: &Path) {
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "selection authority"].as_slice(),
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
fn catalog(repo: &Path) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap()).unwrap()
}
fn save_catalog(repo: &Path, catalog: &toml::Value) {
    std::fs::write(
        repo.join(".af/task-catalog.toml"),
        toml::to_string(catalog).unwrap(),
    )
    .unwrap();
}
fn original(repo: &Path) -> PipelineDefinitionV1 {
    toml::from_str(
        &std::fs::read_to_string(
            repo.join(".af/task-packages/fixture/implementation/pipeline.toml"),
        )
        .unwrap(),
    )
    .unwrap()
}
fn pipeline(repo: &Path, catalog: &mut toml::Value, pipeline: &PipelineDefinitionV1) {
    let path = format!(".af/task-packages/{}", pipeline.name);
    let directory = repo.join(&path);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("pipeline.toml"),
        toml::to_string(pipeline).unwrap(),
    )
    .unwrap();
    catalog["packages"].as_table_mut().unwrap().insert(
        pipeline.name.clone(),
        toml::Value::try_from(json!({"version":"1.0.0","path":path,
            "digest":review_config::lock::package_digest(&pipeline.name, &directory).unwrap()}))
        .unwrap(),
    );
}
fn run(repo: &Path, state: &Path, args: &[&str], expected: i32) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
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
fn state_of<'a>(selection: &'a Value, name: &str) -> &'a Value {
    &selection["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["pipeline"] == name)
        .unwrap()["state"]
}

#[test]
fn incompatible_small_selects_existing_heavy_and_resume_ignores_edited_catalog() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(root.path(), "pagination");
    let mut cat = catalog(&repo);
    let mut small = original(&repo);
    let mut heavy = small.clone();
    heavy.name = "fixture/heavy".into();
    small
        .accepts
        .required_facts
        .insert("small".into(), serde_json::from_value(json!(true)).unwrap());
    pipeline(&repo, &mut cat, &small);
    pipeline(&repo, &mut cat, &heavy);
    save_catalog(&repo, &cat);
    let mut task = read_json(&repo.join("ticket.json"));
    task["pipeline"]["fallback"] = json!("generate");
    task["facts"] = json!({"small":false});
    write_json(&repo.join("ticket.json"), &task);
    commit(&repo);
    let planned = run(&repo, &state, &["task", "plan", "--file", "ticket.json"], 0);
    assert_eq!(planned["attempts"], 0);
    let selection = &planned["selection"]["assessment"];
    assert_eq!(
        selection["decision"],
        json!({"kind":"selected","pipeline":"fixture/heavy"})
    );
    assert_eq!(
        state_of(selection, "fixture/implementation")["kind"],
        "no_fit"
    );
    assert_eq!(
        planned["graph"]["calls"]["root"]["pipeline"],
        "fixture/heavy"
    );
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let revision = cas
        .get_json(planned["revision_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        revision["payload"]["pipeline"]["name"], "fixture/implementation",
        "The request is not rewritten to conceal fallback"
    );
    let record = cas
        .get_json(planned["selection"]["artifact_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        record["input_artifacts"],
        json!([planned["selection"]["request_revision_id"]])
    );
    assert!(planned["execution_records"].as_array().unwrap().is_empty());
    std::fs::write(repo.join(".af/task-catalog.toml"), "invalid edited policy").unwrap();
    std::fs::remove_dir_all(repo.join(".af/task-packages/fixture/heavy")).unwrap();
    let finished = run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        0,
    );
    assert_eq!(finished["attempts"], 3);
    assert_eq!(finished["result"]["acceptance"], "satisfied");
    assert_eq!(finished["selection"], planned["selection"]);
    let replay = run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        0,
    );
    assert_eq!(replay, finished);
}

#[test]
fn selection_refusals_are_persisted_distinct_and_dispatch_nothing() {
    for case in [
        "unknown",
        "ambiguous",
        "budget",
        "deadline",
        "headroom",
        "missing_provider",
        "missing_tool",
        "no_fit",
        "refuse",
        "automatic_no_fit",
    ] {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(root.path(), "pagination");
        let mut cat = catalog(&repo);
        let mut definition = original(&repo);
        let mut task = read_json(&repo.join("ticket.json"));
        task["pipeline"]["fallback"] = json!("generate");
        let expected = match case {
            "unknown" => {
                definition
                    .accepts
                    .required_facts
                    .insert("small".into(), serde_json::from_value(json!(true)).unwrap());
                "needs_facts"
            }
            "ambiguous" => {
                let mut heavy = definition.clone();
                heavy.name = "fixture/heavy".into();
                pipeline(&repo, &mut cat, &heavy);
                task.as_object_mut().unwrap().remove("pipeline");
                "ambiguous"
            }
            "budget" => {
                task["limits"]["verification"]["attempts"] = json!(1);
                "infeasible"
            }
            "deadline" => {
                task["limits"]["wall_ms"] = json!(1);
                "infeasible"
            }
            "headroom" => {
                task["limits"]["wall_ms"] = json!(12000);
                "infeasible"
            }
            "missing_provider" | "missing_tool" => {
                let path = repo.join(".af/task-packages/fixture/implementer");
                let mut worker: review_config::task::catalog::TaskWorkerManifest =
                    toml::from_str(&std::fs::read_to_string(path.join("worker.toml")).unwrap())
                        .unwrap();
                if case == "missing_provider" {
                    worker.signature.attempt.as_mut().unwrap().tokens = 100;
                    worker.runner = review_config::task::catalog::TaskWorkerRunner::Model {
                        provider_kind: "claude".into(),
                        model: "claude-fixture-1".into(),
                        effort: "high".into(),
                    };
                } else if let review_config::task::catalog::TaskWorkerRunner::Command { command } =
                    &mut worker.runner
                {
                    command.program = "af-no-such-tool-6cfdf0".into();
                }
                std::fs::write(path.join("worker.toml"), toml::to_string(&worker).unwrap())
                    .unwrap();
                cat["packages"]["fixture/implementer"]["digest"] = toml::Value::String(
                    review_config::lock::package_digest("fixture/implementer", &path).unwrap(),
                );
                "unavailable"
            }
            _ => {
                definition
                    .accepts
                    .required_facts
                    .insert("small".into(), serde_json::from_value(json!(true)).unwrap());
                task["facts"] = json!({"small":false});
                if case == "refuse" {
                    task["pipeline"]["fallback"] = json!("refuse");
                    "refused"
                } else if case == "automatic_no_fit" {
                    task.as_object_mut().unwrap().remove("pipeline");
                    cat.as_table_mut()
                        .unwrap()
                        .insert("no_match".into(), toml::Value::String("generate".into()));
                    "needs_generation"
                } else {
                    "needs_generation"
                }
            }
        };
        pipeline(&repo, &mut cat, &definition);
        save_catalog(&repo, &cat);
        write_json(&repo.join("ticket.json"), &task);
        commit(&repo);
        let result = run(
            &repo,
            &state,
            &["task", "start", "--execute", "--file", "ticket.json"],
            1,
        );
        assert_eq!(
            result["selection"]["decision"]["kind"], expected,
            "{case}: {result}"
        );
        assert_eq!(result["attempts"], 0);
        assert_eq!(result["chargeable_tokens"], 0);
        let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
        cas.verify(result["selection_id"].as_str().unwrap())
            .unwrap();
        let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        assert!(
            store.task_ids(&cas).unwrap().is_empty(),
            "{case} opened execution state"
        );
    }
}

#[test]
fn trusted_ranking_and_over_budget_fallback_choose_only_feasible_definitions() {
    for case in ["rank", "over_budget", "explicit"] {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(root.path(), "pagination");
        let mut cat = catalog(&repo);
        let mut small = original(&repo);
        let mut heavy = small.clone();
        heavy.name = "fixture/heavy".into();
        let mut task = read_json(&repo.join("ticket.json"));
        let expected = match case {
            "rank" => {
                task.as_object_mut().unwrap().remove("pipeline");
                cat.as_table_mut().unwrap().insert(
                    "selection".into(),
                    toml::Value::try_from(
                        json!({"small":{"fixture/heavy":1,"fixture/implementation":2}}),
                    )
                    .unwrap(),
                );
                "fixture/heavy"
            }
            "over_budget" => {
                small.max_attempts = 2;
                task["pipeline"]["fallback"] = json!("generate");
                "fixture/heavy"
            }
            _ => {
                task["pipeline"]["fallback"] = json!("generate");
                "fixture/implementation"
            }
        };
        pipeline(&repo, &mut cat, &small);
        pipeline(&repo, &mut cat, &heavy);
        save_catalog(&repo, &cat);
        write_json(&repo.join("ticket.json"), &task);
        commit(&repo);
        let plan = run(&repo, &state, &["task", "plan", "--file", "ticket.json"], 0);
        assert_eq!(
            plan["selection"]["assessment"]["decision"]["pipeline"], expected,
            "{case}: {plan}"
        );
        assert_eq!(plan["attempts"], 0);
        if case == "over_budget" {
            assert_eq!(
                state_of(&plan["selection"]["assessment"], "fixture/implementation")["kind"],
                "infeasible"
            );
        }
        let finished = run(
            &repo,
            &state,
            &["task", "run", "--execute", "pagination-cli"],
            0,
        );
        assert_eq!(finished["attempts"], 3);
    }
}
