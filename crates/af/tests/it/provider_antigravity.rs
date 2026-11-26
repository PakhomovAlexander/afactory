//! End-to-end hermetic integration tests for the Antigravity (`agy`) provider:
//! `af provider detect`, `setup`, `add`, `status`, `models`, compatibility checks, and
//! a real `af task plan` -> `af task run` execution against a deterministic fake `agy` binary.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};

const AGY_HELP: &str = "\
Usage: agy [options]

Options:
  --input-format <format>      Input format (text, stream-json)
  --output-format <format>     Output format (text, json, stream-json)
  --disable-slash-commands     Disable slash commands
  --model <model>              Model ID
  --effort <level>             Thinking effort level
  --json-schema <path>         JSON Schema file for structured output
  --sandbox                    Run inside sandbox
  --settings <path>            Session settings file
  --version                    Show version
  --help                       Show help
";

fn fake_agy_script(authenticated: bool, help_page: &str, acp_ok: bool) -> String {
    let auth_py = if authenticated { "True" } else { "False" };
    let acp_py = if acp_ok { "True" } else { "False" };
    let help_json = serde_json::to_string(help_page).unwrap();
    let probe_input =
        serde_json::to_string(std::str::from_utf8(af_runner::task::provider::PROBE_INPUT).unwrap())
            .unwrap();
    format!(
        r#"#!/usr/bin/python3 -B
import json, os, sys

AUTH = {auth_py}
ACP_OK = {acp_py}
HELP = {help_json}
PROBE_INPUT = {probe_input}

args = sys.argv[1:]
if args == ["--version"]:
    print("agy 1.5.18")
    sys.exit(0)
if args == ["--help"]:
    sys.stdout.write(HELP)
    sys.exit(0)

if args[:1] == ["acp"]:
    if not ACP_OK:
        sys.stderr.write("error: acp protocol unsupported\n")
        sys.exit(2)
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        req = json.loads(line)
        rid = req.get("id")
        method = req.get("method")
        if method == "initialize":
            print(json.dumps({{
                "jsonrpc": "2.0",
                "id": rid,
                "result": {{
                    "protocolVersion": 1,
                    "agentInfo": {{"name": "Antigravity ACP Server", "version": "1.5.18"}},
                    "authMethods": [{{"id": "oauth"}}]
                }}
            }}), flush=True)
        elif method == "session/new":
            if not AUTH:
                print(json.dumps({{
                    "jsonrpc": "2.0",
                    "id": rid,
                    "error": {{"code": -32000, "message": "Authentication required"}}
                }}), flush=True)
            else:
                print(json.dumps({{
                    "jsonrpc": "2.0",
                    "id": rid,
                    "result": {{
                        "sessionId": "sess-fixture",
                        "models": {{
                            "availableModels": [
                                {{"modelId": "gemini-4-neon-high", "name": "Gemini 4 Neon (High)"}},
                                {{"modelId": "claude-sonnet-4-6", "name": "Claude Sonnet 4.6"}}
                            ]
                        }},
                        "configOptions": [
                            {{
                                "id": "thought_level",
                                "currentValue": "high",
                                "options": [
                                    {{"value": "low"}},
                                    {{"value": "medium"}},
                                    {{"value": "high"}},
                                    {{"value": "xhigh"}}
                                ]
                            }}
                        ],
                        "_meta": {{
                            "antigravity": {{
                                "tier": {{"name": "Google AI Pro"}}
                            }}
                        }}
                    }}
                }}), flush=True)
    sys.exit(0)

# stream-json worker execution
model = "gemini-4-neon-high"
if "--model" in args:
    model = args[args.index("--model") + 1]
settings_path = args[args.index("--settings") + 1] if "--settings" in args else None
assert settings_path and os.path.isfile(settings_path), "adapter must pass --settings"
settings = json.load(open(settings_path))
assert settings["sandbox"]["enabled"] is True
assert "mcp(*)" in settings["permissions"]["deny"]

raw_turn = sys.stdin.readline()
turn = json.loads(raw_turn)
assert turn["event"] == "user"
content = turn["message"]["content"]

print(json.dumps({{
    "event": "init",
    "conversation_id": "conv-fixture",
    "init": {{"model": model}}
}}), flush=True)

if content == PROBE_INPUT:
    print(json.dumps({{
        "event": "result",
        "result": {{
            "conversation_id": "conv-fixture",
            "status": "SUCCESS",
            "response": "OK",
            "usage": {{
                "input_tokens": 12,
                "output_tokens": 2,
                "thinking_tokens": 0,
                "cache_read_tokens": 0,
                "total_tokens": 14
            }}
        }}
    }}), flush=True)
else:
    req = json.loads(content)
    s = req["inputs"]["sources"][0]["payload"]["sources"]
    reply = {{
        "schema": "af.worker-reply/1",
        "outputs": {{
            "draft": [{{
                "schema": "af.document-draft/3",
                "title": "Release notes",
                "sections": [{{
                    "heading": "Summary",
                    "body": "\n\n".join(s[k]["text"] for k in sorted(s))
                }}],
                "citations": [{{"kind": "source", "name": name}} for name in sorted(s)]
            }}]
        }}
    }}
    print(json.dumps({{
        "event": "result",
        "result": {{
            "conversation_id": "conv-fixture",
            "status": "SUCCESS",
            "structured_output": reply,
            "response": json.dumps(reply),
            "usage": {{
                "input_tokens": 120,
                "output_tokens": 30,
                "thinking_tokens": 0,
                "cache_read_tokens": 40,
                "total_tokens": 150
            }}
        }}
    }}), flush=True)
"#
    )
}

fn install_fake_agy(bin: &Path, authenticated: bool, help_page: &str, acp_ok: bool) -> PathBuf {
    std::fs::create_dir_all(bin).unwrap();
    let program = bin.join("agy");
    std::fs::write(&program, fake_agy_script(authenticated, help_page, acp_ok)).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

fn run_af(root: &Path, bin: &Path, registry: &Path, args: &[&str]) -> Output {
    let path_var = format!("{}:/usr/bin:/bin", bin.display());
    crate::common::af()
        .args(args)
        .current_dir(root)
        .env("HOME", root)
        .env("USER", "fixture")
        .env("PATH", path_var)
        .env("AF_PROVIDERS_FILE", registry)
        .env("AF_TEST_DETECT_SYSTEM_DIRS", "")
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap()
}

fn json_doc(output: &Output, schema_file: &str) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let doc: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    crate::schemas::valid(&crate::schemas::validator(schema_file), &doc);
    doc
}

#[test]
fn antigravity_detect_setup_status_and_models_conform_to_schemas() {
    let root = crate::common::provider_fixture_fs::tempdir().unwrap();
    let bin = root.path().join("bin");
    let registry = root.path().join("config/af/providers.toml");
    let default_dir = root.path().join(".gemini/antigravity-cli");
    crate::common::provider_fixture_fs::create_dir_all(&default_dir).unwrap();
    crate::common::provider_fixture_fs::write(default_dir.join("installation_id"), "id-1\n")
        .unwrap();
    crate::common::provider_fixture_fs::write(default_dir.join("jetski_state.pbtxt"), "ok\n")
        .unwrap();
    install_fake_agy(&bin, true, AGY_HELP, true);

    // 1. Detect finds agy + ~/.gemini/antigravity-cli and registers it with --apply.
    let detect = run_af(
        root.path(),
        &bin,
        &registry,
        &[
            "provider",
            "detect",
            "--kind",
            "antigravity",
            "--apply",
            "--json",
        ],
    );
    assert_eq!(detect.status.code(), Some(0));
    let doc = json_doc(&detect, "provider-detect-v5.json");
    assert_eq!(doc["schema"], "af/provider-detect@5");
    assert_eq!(doc["binaries"][0]["kind"], "antigravity");
    assert_eq!(doc["binaries"][0]["compatible"], true);
    assert_eq!(doc["candidates"][0]["kind"], "antigravity");
    assert_eq!(doc["candidates"][0]["auth"], "authenticated");
    assert_eq!(doc["candidates"][0]["applied"]["id"], "antigravity-main");
    assert_eq!(doc["candidates"][0]["applied"]["result"], "registered");

    // 2. Status reports authenticated with subscription credential, and --usage is unsupported (still exit 0).
    let status = run_af(
        root.path(),
        &bin,
        &registry,
        &["provider", "status", "--usage", "--json"],
    );
    assert_eq!(status.status.code(), Some(0));
    let status_doc = json_doc(&status, "provider-status-v7.json");
    assert_eq!(status_doc["schema"], "af/provider-status@7");
    let row = status_doc["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "antigravity-main")
        .expect("antigravity-main row");
    assert_eq!(row["kind"], "antigravity");
    assert_eq!(row["auth"], "authenticated");
    assert_eq!(row["credential"], "subscription");
    assert_eq!(row["usage"]["state"], "unsupported");

    // 3. Models lists the ACP catalog and resolves a display name + effort.
    let models = run_af(
        root.path(),
        &bin,
        &registry,
        &[
            "provider",
            "models",
            "antigravity-main",
            "--resolve",
            "Claude Sonnet 4.6/high",
            "--json",
        ],
    );
    assert_eq!(
        models.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&models.stderr)
    );
    let models_doc = json_doc(&models, "provider-models-v5.json");
    assert_eq!(models_doc["schema"], "af/provider-models@5");
    let provider = &models_doc["providers"][0];
    assert_eq!(provider["kind"], "antigravity");
    assert_eq!(provider["source"], "agy acp session/new");
    assert_eq!(provider["harness_version"], "1.5.18");
    assert_eq!(provider["resolution"]["outcome"], "display_name");
    assert_eq!(provider["resolution"]["id"], "claude-sonnet-4-6");
    assert_eq!(provider["resolution"]["effort_check"], "listed");

    // 4. Setup on a logged-out context returns human_action_required (exit 3) with schema v3.
    let logged_out_dir = root.path().join("agy-logged-out");
    crate::common::provider_fixture_fs::create_dir_all(&logged_out_dir).unwrap();
    install_fake_agy(&bin, false, AGY_HELP, true);
    let setup = run_af(
        root.path(),
        &bin,
        &registry,
        &[
            "provider",
            "setup",
            "agy-other",
            "--kind",
            "antigravity",
            "--auth-dir",
            logged_out_dir.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(setup.status.code(), Some(3));
    let setup_doc = json_doc(&setup, "provider-setup-v3.json");
    assert_eq!(setup_doc["schema"], "af/provider-setup@3");
    assert_eq!(setup_doc["result"], "human_action_required");
    assert_eq!(setup_doc["provider"]["kind"], "antigravity");
}

#[test]
fn antigravity_incompatible_cli_or_broken_acp_is_refused_with_exit_4() {
    let root = crate::common::provider_fixture_fs::tempdir().unwrap();
    let bin = root.path().join("bin");
    let registry = root.path().join("config/af/providers.toml");
    let auth_dir = root.path().join("agy-auth");
    crate::common::provider_fixture_fs::create_dir_all(registry.parent().unwrap()).unwrap();
    crate::common::provider_fixture_fs::create_dir_all(&auth_dir).unwrap();
    crate::common::provider_fixture_fs::write(auth_dir.join("installation_id"), "id-1\n").unwrap();
    crate::common::provider_fixture_fs::write(
        &registry,
        toml::to_string(&json!({
            "version": 5,
            "providers": [{"id": "agy-main", "kind": "antigravity", "auth_dir": auth_dir}]
        }))
        .unwrap(),
    )
    .unwrap();

    // Missing --sandbox in --help -> incompatible, exit 4.
    let missing_sandbox: String = AGY_HELP
        .lines()
        .filter(|line| !line.contains("--sandbox"))
        .map(|line| format!("{line}\n"))
        .collect();
    install_fake_agy(&bin, true, &missing_sandbox, true);
    let status = run_af(
        root.path(),
        &bin,
        &registry,
        &["provider", "status", "--json"],
    );
    assert_eq!(status.status.code(), Some(4));
    let doc = json_doc(&status, "provider-status-v7.json");
    assert_eq!(doc["providers"][0]["auth"], "incompatible");
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(stderr.contains("--sandbox"), "{stderr}");
    assert!(stderr.contains("agy update"), "{stderr}");

    // Full --help, but `agy acp --stdio` fails before initialize -> incompatible, exit 4.
    install_fake_agy(&bin, true, AGY_HELP, false);
    let status = run_af(
        root.path(),
        &bin,
        &registry,
        &["provider", "status", "--json"],
    );
    assert_eq!(status.status.code(), Some(4));
    let doc = json_doc(&status, "provider-status-v7.json");
    assert_eq!(doc["providers"][0]["auth"], "incompatible");
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(stderr.contains("agy acp --stdio"), "{stderr}");
}

#[test]
fn antigravity_task_plan_and_run_succeeds_and_refuses_distinct_principals() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let state = root.path().join("state");
    let home = root.path().join("home");
    let bin = root.path().join("bin");
    crate::common::provider_fixture_fs::create_dir_all(&home).unwrap();
    crate::common::provider_fixture_fs::write(home.join("installation_id"), "id-1\n").unwrap();
    install_fake_agy(&bin, true, AGY_HELP, true);

    // Initialize a minimal document-task repository using `af catalog init`.
    let init = crate::common::af()
        .current_dir(root.path())
        .args(["catalog", "init", "--destination", "repo", "--json"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "catalog init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);

    let registry = home.join("providers.toml");
    crate::common::provider_fixture_fs::write(
        &registry,
        toml::to_string(&json!({
            "version": 5,
            "providers": [{"id": "agy-personal", "kind": "antigravity", "auth_dir": home}]
        }))
        .unwrap(),
    )
    .unwrap();

    // Switch builtin/document-author to an Antigravity Model Worker.
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    catalog["providers"] = json!({"builtin/document-author": "agy-personal"});
    catalog["independence"] = json!({
        "command_workers_by_package": true,
        "distinct_principals": false,
        "distinct_providers": false,
        "distinct_models": false
    });
    let pkg_rel = catalog["packages"]["builtin/document-author"]["path"]
        .as_str()
        .unwrap()
        .to_string();
    let worker_path = repo.join(&pkg_rel).join("worker.toml");
    let mut worker: af_pipeline_config::task::catalog::TaskWorkerManifest =
        toml::from_str(&std::fs::read_to_string(&worker_path).unwrap()).unwrap();
    worker.runner = af_pipeline_config::task::catalog::TaskWorkerRunner::Model {
        provider_kind: "antigravity".into(),
        model: "gemini-4-neon-high".into(),
        effort: "high".into(),
    };
    worker.signature.attempt.as_mut().unwrap().tokens = 16384;
    worker.signature.attempt.as_mut().unwrap().wall_ms = 180000;
    std::fs::write(&worker_path, toml::to_string(&worker).unwrap()).unwrap();
    let digest =
        af_pipeline_config::lock::package_digest("builtin/document-author", &repo.join(&pkg_rel))
            .unwrap();
    catalog["packages"]["builtin/document-author"]["digest"] = json!(digest);

    let pipeline_rel = catalog["packages"]["builtin/release-notes"]["path"]
        .as_str()
        .unwrap()
        .to_string();
    let pipeline_dir = repo.join(&pipeline_rel);
    let mut pipeline: af_contracts::task::pipeline::PipelineDefinition =
        toml::from_str(&std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap())
            .unwrap();
    pipeline.max_attempts = 4;
    std::fs::write(
        pipeline_dir.join("pipeline.toml"),
        toml::to_string(&pipeline).unwrap(),
    )
    .unwrap();
    catalog["packages"]["builtin/release-notes"]["digest"] = json!(
        af_pipeline_config::lock::package_digest("builtin/release-notes", &pipeline_dir).unwrap()
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();

    let task_path = repo.join("document.json");
    let mut task: Value = serde_json::from_slice(&std::fs::read(&task_path).unwrap()).unwrap();
    task["task_id"] = json!("agy-doc-1");
    task["limits"] = json!({
        "tokens": 65536,
        "max_attempts": 4,
        "wall_ms": 300000,
        "verification": {"tokens": 0, "attempts": 2, "wall_ms": 10000}
    });
    std::fs::write(&task_path, serde_json::to_vec(&task).unwrap()).unwrap();

    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "Antigravity document worker"]);

    let path_var = format!("{}:/usr/bin:/bin", bin.display());
    let plan = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "plan",
            "--state",
            state.to_str().unwrap(),
            "--file",
            task_path.to_str().unwrap(),
            "--json",
        ])
        .env("HOME", &home)
        .env("USER", "fixture")
        .env("PATH", &path_var)
        .env("AF_PROVIDERS_FILE", &registry)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert_eq!(
        plan.status.code(),
        Some(0),
        "plan failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan_doc = json_doc(&plan, "task-inspection-v23.json");
    let plan_id = plan_doc["plan_id"].as_str().unwrap().to_string();

    let run = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "run",
            "agy-doc-1",
            "--state",
            state.to_str().unwrap(),
            "--confirm-plan",
            &plan_id,
            "--json",
        ])
        .env("HOME", &home)
        .env("USER", "fixture")
        .env("PATH", &path_var)
        .env("AF_PROVIDERS_FILE", &registry)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert_eq!(
        run.status.code(),
        Some(0),
        "run failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let run_doc = json_doc(&run, "task-inspection-v23.json");
    assert_eq!(run_doc["state"]["phase"], "finished");
    assert_eq!(run_doc["state"]["outcome"], "verified");

    // Enabling distinct_principals must refuse planning because Antigravity has principal_id: null.
    catalog["independence"]["distinct_principals"] = json!(true);
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    git(&repo, &["add", ".af/task-catalog.toml"]);
    git(&repo, &["commit", "-m", "Require distinct principals"]);
    let task_file2 = root.path().join("task2.json");
    task["task_id"] = json!("agy-doc-2");
    std::fs::write(&task_file2, serde_json::to_vec(&task).unwrap()).unwrap();
    let refused = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "plan",
            "--state",
            state.to_str().unwrap(),
            "--file",
            task_file2.to_str().unwrap(),
        ])
        .env("HOME", &home)
        .env("USER", "fixture")
        .env("PATH", &path_var)
        .env("AF_PROVIDERS_FILE", &registry)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert_ne!(refused.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("distinct_principals") || stderr.contains("known account identities"),
        "{stderr}"
    );
}

fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}
