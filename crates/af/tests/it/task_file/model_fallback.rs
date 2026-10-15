//! Issue #263, end to end: a fake `claude` answers the Provider-admission probe exactly like the
//! result Claude Code returned on 2026-10-13, 2026-10-14 and 2026-10-15, when the safety
//! classifier of the bound `claude-fable-5-1` flagged the probe and `claude-opus-5` answered.
//! `af review run` refuses the Provider before any business Worker starts, charges the 9496
//! tokens the result reports, and says on its own stderr what happened and what to do next
//! (ADR-0147).
use std::process::Command;

use serde_json::Value;

use crate::task_cli::fixture_named;

/// The recorded result, byte for byte in its numbers.
const RECORDED_FALLBACK: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"OK","num_turns":2,"safety_stops":2,"stop_reason":"end_turn","fallback_credit":null,"usage":{"input_tokens":4,"output_tokens":4,"cache_creation_input_tokens":9488,"cache_read_input_tokens":3001},"modelUsage":{"claude-fable-5-1":{"inputTokens":2,"outputTokens":0,"cacheCreationInputTokens":3210,"cacheReadInputTokens":3001,"canonicalModel":"claude-fable-5-1"},"claude-opus-5":{"inputTokens":2,"outputTokens":4,"cacheCreationInputTokens":6278,"cacheReadInputTokens":0,"canonicalModel":"claude-opus-5"}}}"#;

#[test]
fn a_safety_classifier_fallback_at_admission_is_refused_charged_and_named_on_stderr() {
    use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "review");
    let home = directory.path().join("home");
    let bin = directory.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(home.join("help"), crate::provider_cli::CLAUDE_HELP).unwrap();
    std::fs::write(home.join("version"), "2.1.300 (Claude Code)\n").unwrap();
    std::fs::write(home.join("result"), RECORDED_FALLBACK).unwrap();
    // The fake records every model call with the guard it received, and answers the admission
    // probe with the recorded fallback; any other request is a business Worker.
    let stub = r#"#!/usr/bin/python3
import os,json,sys
home=os.environ['CLAUDE_CONFIG_DIR']
if sys.argv[1:] in (['--help'],['--version']):
 sys.stdout.write(open(home+'/'+sys.argv[1][2:]).read())
 sys.exit(0)
if sys.argv[1:3]==['auth','status']:
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':'developer@example.test'}))
 sys.exit(0)
request=sys.stdin.read()
kind='admission' if request=='Reply with exactly: OK\n' else 'business'
with open(home+'/calls','a') as f: f.write(kind+' '+os.environ.get('CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK','unset')+' '+os.environ.get('CLAUDE_CODE_NO_MODEL_FALLBACK','unset')+'\n')
sys.stdout.write(open(home+'/result').read())
"#;
    let installed = bin.join("claude");
    std::fs::write(&installed, stub).unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry = home.join("providers.toml");
    std::fs::write(&registry,toml::to_string(&serde_json::json!({"version":1,"providers":[{"id":"claude-personal","kind":"claude","auth_dir":home}]})).unwrap()).unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    let mut providers = toml::map::Map::new();
    for name in ["bugs", "correctness"] {
        let package = format!("fixture/{name}");
        let path = repo.join(format!(".af/task-packages/{package}"));
        let mut worker: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(path.join("worker.toml")).unwrap()).unwrap();
        worker.runner = TaskWorkerRunner::Model {
            provider_kind: "claude".into(),
            model: "claude-fable-5-1".into(),
            effort: "xhigh".into(),
        };
        let attempt = worker.signature.attempt.as_mut().unwrap();
        attempt.tokens = 1000;
        attempt.wall_ms = super::NATIVE_REVIEWER_WALL_MS;
        std::fs::write(path.join("worker.toml"), toml::to_string(&worker).unwrap()).unwrap();
        catalog["packages"][&package]["digest"] =
            toml::Value::String(review_config::lock::package_digest(&package, &path).unwrap());
        providers.insert(package, toml::Value::String("claude-personal".into()));
    }
    let pipeline_dir = repo.join(".af/task-packages/fixture/review");
    let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
        toml::from_str(&std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap())
            .unwrap();
    pipeline.max_attempts = 4;
    std::fs::write(
        pipeline_dir.join("pipeline.toml"),
        toml::to_string(&pipeline).unwrap(),
    )
    .unwrap();
    catalog["packages"]["fixture/review"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("fixture/review", &pipeline_dir).unwrap(),
    );
    catalog
        .as_table_mut()
        .unwrap()
        .insert("providers".into(), toml::Value::Table(providers));
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    let file_path = repo.join("review.json");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&file_path).unwrap()).unwrap();
    file["limits"] = super::native_model_limits();
    std::fs::write(file_path, serde_json::to_vec(&file).unwrap()).unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "model bindings"]] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let run = |args: &[&str]| {
        crate::common::af()
            .current_dir(&repo)
            .env("HOME", &home)
            .env("USER", "fixture")
            .env("PATH", &path)
            .env("AF_PROVIDERS_FILE", &registry)
            .args(args)
            .args(["--json", "--state"])
            .arg(&state)
            .output()
            .unwrap()
    };
    let output = run(&["review", "run", "--file", "review.json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stdout}\n{stderr}");
    // Only the admission probe ran, with the fallback turned off and model substitution kept.
    assert_eq!(
        std::fs::read_to_string(home.join("calls")).unwrap(),
        "admission 1 unset\n",
        "{stderr}"
    );
    // One line on the run's own stderr names the node, the Provider, the bound model and effort,
    // both models with their output tokens, the counts, the cause, af's refusal and the next step.
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("af: root.providers."))
        .collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    let line = lines[0];
    for part in [
        "failed: Provider claude-personal (claude), model claude-fable-5-1, effort xhigh: ",
        "the safety classifier of claude-fable-5-1 flagged the request and Claude Code switched models, a safety-classifier fallback (model_refused)",
        "the Worker bound claude-fable-5-1 (effort xhigh), which wrote 0 output tokens",
        "claude-opus-5 (4 output tokens) answered (safety_stops 2, num_turns 2)",
        "af refused the reply, because af never runs or accepts a model the Worker did not bind",
        "this is neither a credential nor a model-spelling problem",
        "next: af already sets CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1",
    ] {
        assert!(line.contains(part), "{part}: {line}");
    }
    for absent in ["Reply with exactly", "developer@example.test", "transport:"] {
        assert!(!line.contains(absent), "{absent}: {line}");
    }
    // The probe is charged exactly what its result reports, and `af task show` keeps the full
    // diagnostic.
    let shown = run(&["task", "show", "review-cli"]);
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["chargeable_tokens"], "9496", "{shown}");
    let diagnostics: Vec<&str> = shown["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|record| record["diagnostic"]["error"].as_str())
        .collect();
    assert_eq!(diagnostics.len(), 1, "{shown}");
    assert!(
        diagnostics[0]
            .starts_with("Claude Worker failed: the safety classifier of claude-fable-5-1"),
        "{}",
        diagnostics[0]
    );
    assert!(
        diagnostics[0].ends_with("; transport: exited 0"),
        "{}",
        diagnostics[0]
    );
}
