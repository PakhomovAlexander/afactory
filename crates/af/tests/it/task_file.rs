//! Public Task-file path: captured authority survives CLI process boundaries and edits.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

#[path = "task_file/attempt_resilience.rs"]
mod attempt_resilience;
#[path = "task_file/model_discovery.rs"]
mod model_discovery;
#[path = "task_file/model_fallback.rs"]
mod model_fallback;
#[path = "task_file/wall_bounds.rs"]
mod wall_bounds;

#[path = "task_file/selected_inputs.rs"]
mod selected_inputs;

use crate::task_cli::{copy_tree, fixture_named};

/// Each native-model reviewer's Attempt wall: two minutes, the load-safe wall (ADR-0114), in
/// place of the fixture manifests' 5 s. A reviewer starts a Python fake provider for its
/// identity recheck and again for its model call; on a loaded gate (seven nextest threads, load
/// 7-28) that took about 5 s, so the Attempt timed out although its subject is accounting and
/// account currentness, never the wall. A reviewer that answers still ends when it answers.
const NATIVE_REVIEWER_WALL_MS: u64 = 120_000;
/// The protected allowances the verification reserve must cover: the catalog's default Provider
/// admission, the two reviewers and the fixture code policy's `check_wall_ms`.
const PROVIDER_ADMISSION_WALL_MS: u64 = 45_000;
const CHECK_WALL_MS: u64 = 5_000;
/// The reserve grows with the reviewer walls, never shrinks: it is exactly the protected
/// verifier allocation, as the fixture's 60 s was for its 5 s reviewers (ADR-0114).
const NATIVE_MODEL_VERIFICATION_WALL_MS: u64 =
    PROVIDER_ADMISSION_WALL_MS + 2 * NATIVE_REVIEWER_WALL_MS + CHECK_WALL_MS;
/// Native-model fixtures plan, reject one changed account, restore it, then resume the same Task.
/// Keep that absolute deadline away from loaded-gate latency without changing any dispatch
/// bound: ten minutes of slack above the whole verification reserve (ADR-0114).
const NATIVE_MODEL_TASK_SLACK_MS: u64 = 600_000;
const NATIVE_MODEL_TASK_WALL_MS: u64 =
    NATIVE_MODEL_VERIFICATION_WALL_MS + NATIVE_MODEL_TASK_SLACK_MS;

fn native_model_limits() -> Value {
    serde_json::json!({
        "tokens": 10_000,
        "max_attempts": 4,
        "wall_ms": NATIVE_MODEL_TASK_WALL_MS,
        "verification": {"tokens": 6_096, "attempts": 4, "wall_ms": NATIVE_MODEL_VERIFICATION_WALL_MS}
    })
}

fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    fixture_named(root, "pagination")
}

#[test]
fn review_file_uses_common_task_state_and_keeps_changes_requested_exit() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "review");
    let output = crate::common::af()
        .current_dir(&repo)
        .args([
            "review",
            "run",
            "--file",
            "review.json",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(result["result"]["domain_conclusion"], "changes_requested");
    assert_eq!(result["attempts"], 3);
    assert_eq!(
        result["review_rounds"][0]["selected_results"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert!(state.join("events.sqlite").is_file());
    let replay = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "run",
            "--execute",
            "review-cli",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(replay.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap(),
        result
    );
    let wrong_kind = crate::common::af()
        .current_dir(&repo)
        .args([
            "review",
            "run",
            "--file",
            "review.json",
            "--heavy",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(wrong_kind.status.code(), Some(2));
}

#[test]
fn review_without_a_requirements_port_uses_its_own_schema_contract() {
    use af_contracts::task::pipeline::{PipelineDefinition, TaskOperatorV1};
    use serde_json::json;
    for required in [false, true] {
        for named in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let (repo, state) = fixture_named(directory.path(), "review");
            let packages = repo.join(".af/task-packages/fixture");
            let path = packages.join("review/pipeline.toml");
            let mut pipeline: PipelineDefinition =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            pipeline.contract.inputs.remove("requirements");
            for node in &mut pipeline.nodes {
                if matches!(node.operator, TaskOperatorV1::Verify { .. }) {
                    node.inputs.remove("requirements");
                }
            }
            std::fs::write(path, toml::to_string(&pipeline).unwrap()).unwrap();
            if required {
                let path = packages.join("bugs/input.schema.json");
                let original: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                std::fs::write(
                    path,
                    serde_json::to_vec(&json!({"allOf":[original,{"required":["requirements"]}]}))
                        .unwrap(),
                )
                .unwrap();
            }
            let catalog_path = repo.join(".af/task-catalog.toml");
            let mut catalog: toml::Value =
                toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
            for name in ["review", "bugs"] {
                catalog["packages"][format!("fixture/{name}")]["digest"] = toml::Value::String(
                    af_pipeline_config::lock::package_digest(
                        &format!("fixture/{name}"),
                        &packages.join(name),
                    )
                    .unwrap(),
                );
            }
            std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
            let file_path = repo.join("review.json");
            let mut file: Value =
                serde_json::from_slice(&std::fs::read(&file_path).unwrap()).unwrap();
            if !named {
                file.as_object_mut().unwrap().remove("pipeline");
            }
            std::fs::write(file_path, serde_json::to_vec(&file).unwrap()).unwrap();
            for args in [
                vec!["add", "-A"],
                vec!["commit", "-qm", "no requirements root"],
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
            let output = crate::common::af()
                .current_dir(&repo)
                .args([
                    "review",
                    "run",
                    "--file",
                    "review.json",
                    "--json",
                    "--state",
                ])
                .arg(&state)
                .output()
                .unwrap();
            let diagnostic = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                output.status.code(),
                Some(if required { 1 } else { 3 }),
                "{diagnostic}"
            );
            if required {
                assert!(
                    diagnostic.contains("Worker input presence cannot be proved"),
                    "{diagnostic}"
                );
            } else {
                let result: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["result"]["execution"], "completed");
                assert_eq!(result["result"]["acceptance"], "satisfied");
                assert_eq!(result["attempts"], 3);
            }
        }
    }
}

/// Required schema inputs must be proved before the syntax gate or any Worker can run.
#[test]
fn review_requirements_are_refused_before_checks_or_delivered_explicitly() {
    use serde_json::json;
    for composition in [
        "required",
        "allOf",
        "reference",
        "single_one_of",
        "count",
        "conditional_count",
        "forbidden_property",
        "forbidden_additional",
        "unknown_one_of",
        "overlapping_count",
    ] {
        for named in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let (repo, state) = fixture_named(directory.path(), "review");
            let catalog_path = repo.join(".af/task-catalog.toml");
            let mut catalog: toml::Value =
                toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
            for name in ["bugs", "correctness"] {
                let package = format!("fixture/{name}");
                let path = repo.join(format!(".af/task-packages/{package}"));
                let schema_path = path.join("input.schema.json");
                let original: Value =
                    serde_json::from_slice(&std::fs::read(&schema_path).unwrap()).unwrap();
                let required = json!({"required":["requirements"], "properties":{"requirements":{"items":{"properties":{"payload":{"properties":{"specification":{"const":{"acceptance":["Keep this exact explicit specification"]}}},"required":["specification"]}}}}}});
                let schema = match composition {
                    "required" => json!({"allOf":[original, required]}),
                    "allOf" => json!({"allOf":[original, {"allOf":[required]}]}),
                    "reference" => {
                        json!({"$defs":{"business":required},"allOf":[original, {"$ref":"#/$defs/business"}]})
                    }
                    "single_one_of" => json!({"allOf":[original, required,{
                        "properties":{"source":{"oneOf":[{"type":"array"}]}}
                    }]}),
                    "forbidden_property" => json!({"allOf":[original,{
                        "if":{"required":["requirements"]},
                        "else":{"properties":{"source":{"$ref":"#/$defs/forbidden"}}}
                    }],"$defs":{"forbidden":{"allOf":[{"not":{}}]}}}),
                    "forbidden_additional" => json!({"allOf":[original,{
                        "if":{"required":["requirements"]},
                        "else":{"properties":{"assignment":true,"checks":true,"history":true,"subject":true},"additionalProperties":{"not":{}}}
                    }]}),
                    "unknown_one_of" => json!({"allOf":[original,{
                        "if":{"required":["requirements"]},
                        "else":{"oneOf":[{"properties":{"source":{"type":"array"}}},{"properties":{"source":{"type":"array"}}}]}
                    }]}),
                    "overlapping_count" => json!({"allOf":[original,{
                        "oneOf":[{"maxProperties":5,"additionalProperties":true},{}]
                    }]}),
                    _ => {
                        let count = json!({"minProperties":6,"additionalProperties":false,"properties":{"assignment":{},"checks":{},"history":{},"source":{},"subject":{},"requirements":{}}});
                        if composition == "count" {
                            let mut direct = original;
                            direct["minProperties"] = json!(6);
                            let envelope = direct["additionalProperties"].clone();
                            direct["properties"] = count["properties"].clone();
                            for property in
                                direct["properties"].as_object_mut().unwrap().values_mut()
                            {
                                *property = envelope.clone();
                            }
                            direct["additionalProperties"] = json!(false);
                            direct
                        } else {
                            json!({"allOf":[original,{"if":{"required":["source"]},"then":{"$ref":"#/$defs/count"}}],"$defs":{"count":count}})
                        }
                    }
                };
                std::fs::write(schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
                catalog["packages"][&package]["digest"] = toml::Value::String(
                    af_pipeline_config::lock::package_digest(&package, &path).unwrap(),
                );
            }
            std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
            let file_path = repo.join("review.json");
            let mut file: Value =
                serde_json::from_slice(&std::fs::read(&file_path).unwrap()).unwrap();
            if !named {
                file.as_object_mut().unwrap().remove("pipeline");
            }
            std::fs::write(&file_path, serde_json::to_vec(&file).unwrap()).unwrap();
            for args in [
                vec!["add", "-A"],
                vec!["commit", "-qm", "required schema inputs"],
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
            let invoke = || {
                crate::common::af()
                    .current_dir(&repo)
                    .args([
                        "review",
                        "run",
                        "--file",
                        "review.json",
                        "--json",
                        "--state",
                    ])
                    .arg(&state)
                    .output()
                    .unwrap()
            };
            let refused = invoke();
            let diagnostic = format!(
                "{}\n{}",
                String::from_utf8_lossy(&refused.stdout),
                String::from_utf8_lossy(&refused.stderr)
            );
            assert_eq!(refused.status.code(), Some(1), "{diagnostic}");
            assert!(
                diagnostic.contains("Worker input presence cannot be proved"),
                "{diagnostic}"
            );
            assert!(!diagnostic.contains("malformed_reply"), "{diagnostic}");
            // No Task was admitted: not even the first check Attempt may have started.
            let listed = crate::common::af()
                .current_dir(&repo)
                .args(["task", "list", "--json", "--state"])
                .arg(&state)
                .output()
                .unwrap();
            assert!(
                listed.status.success(),
                "{}",
                String::from_utf8_lossy(&listed.stderr)
            );
            assert!(!String::from_utf8_lossy(&listed.stdout).contains("review-cli"));
            file["requirements"] = json!({"acceptance":["Keep this exact explicit specification"]});
            std::fs::write(&file_path, serde_json::to_vec(&file).unwrap()).unwrap();
            let completed = invoke();
            assert_eq!(
                completed.status.code(),
                Some(3),
                "{}\n{}",
                String::from_utf8_lossy(&completed.stdout),
                String::from_utf8_lossy(&completed.stderr)
            );
            let result: Value = serde_json::from_slice(&completed.stdout).unwrap();
            assert_eq!(result["result"]["execution"], "completed");
            assert_eq!(result["result"]["acceptance"], "satisfied");
            assert_eq!(result["attempts"], 3);
        }
    }
}

#[test]
fn native_model_cli_admission_is_shared_and_account_changes_refuse_dispatch() {
    native_model_case(false, false);
}

#[test]
fn native_model_cli_retains_wide_failed_usage_in_json_and_text_inspection() {
    native_model_case(true, false);
}

#[test]
fn native_codex_multiturn_usage_survives_common_accounting_and_fresh_inspection() {
    native_model_case(true, true);
}

/// How the fixture client updates itself while its Task runs.
#[derive(Clone, Copy, PartialEq)]
enum ClientUpdate {
    None,
    /// During Provider admission: a new version file behind the repointed launcher; the old
    /// file stays.
    KeepsOldVersion,
    /// The same, and the old version file is deleted.
    RemovesOldVersion,
    /// The old version file is deleted while the next Worker's identity recheck runs on it;
    /// that recheck still answers, and the Worker's own start then finds no file.
    RemovesOldVersionDuringRecheck,
    /// The same, and that recheck fails.
    RemovesOldVersionAndFailsRecheck,
}

fn native_model_case(wide: bool, codex: bool) {
    native_model_drift_case(wide, codex, None, ClientUpdate::None);
}

#[test]
fn native_task_account_change_after_admission_refuses_private_worker_context() {
    native_model_drift_case(false, false, Some(1), ClientUpdate::None);
}
#[test]
fn native_task_account_change_between_workers_retains_original_spend() {
    native_model_drift_case(false, false, Some(2), ClientUpdate::None);
}

/// A native client that updates itself during a Task repoints its launcher at a new version
/// file. The Task keeps running the executable it captured (ADR-0126).
#[test]
fn native_client_update_between_workers_keeps_the_captured_executable() {
    native_model_drift_case(false, false, None, ClientUpdate::KeepsOldVersion);
}

/// An update that also deletes the captured version file moves the Task's remaining Workers to
/// the installed client, under the same account (ADR-0126).
#[test]
fn native_client_update_that_removes_the_old_version_continues_on_the_installed_client() {
    native_model_drift_case(false, false, None, ClientUpdate::RemovesOldVersion);
}

/// The captured file can vanish between a Worker's identity recheck and its start. The program
/// that could not start received nothing, so the same Attempt runs on the installed client.
#[test]
fn native_client_removed_between_recheck_and_start_runs_the_same_attempt_on_the_installed_one() {
    native_model_drift_case(
        false,
        false,
        None,
        ClientUpdate::RemovesOldVersionDuringRecheck,
    );
}

/// The captured file can vanish while it is being rechecked. A recheck that failed on a removed
/// executable is repeated on the installed client instead of refusing the Worker.
#[test]
fn native_client_removed_during_a_failed_recheck_is_rechecked_on_the_installed_one() {
    native_model_drift_case(
        false,
        false,
        None,
        ClientUpdate::RemovesOldVersionAndFailsRecheck,
    );
}

#[test]
fn native_model_fixture_widens_only_its_walls_and_the_reserve_that_covers_them() {
    let limits = native_model_limits();
    assert_eq!(limits["wall_ms"], NATIVE_MODEL_TASK_WALL_MS);
    assert_eq!(limits["tokens"], 10_000);
    assert_eq!(limits["max_attempts"], 4);
    assert_eq!(limits["verification"]["tokens"], 6_096);
    assert_eq!(limits["verification"]["attempts"], 4);
    // The reserve is the sum it encodes: Provider admission, both reviewers and the check.
    assert_eq!(
        limits["verification"]["wall_ms"],
        PROVIDER_ADMISSION_WALL_MS + 2 * NATIVE_REVIEWER_WALL_MS + CHECK_WALL_MS
    );
    assert_eq!(
        limits["wall_ms"].as_u64().unwrap() - limits["verification"]["wall_ms"].as_u64().unwrap(),
        NATIVE_MODEL_TASK_SLACK_MS
    );
    // The parts are the fixture's own: its code policy's check wall and the 5 s reviewer walls
    // this fixture raises, which are below the load-safe wall.
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let review = workspace.join("fixtures/task-runtime/review/.af");
    let policy: toml::Value =
        toml::from_str(&std::fs::read_to_string(review.join("code-policy.toml")).unwrap()).unwrap();
    assert_eq!(
        policy["check_wall_ms"].as_integer(),
        Some(CHECK_WALL_MS as i64)
    );
    for name in ["bugs", "correctness"] {
        let worker: toml::Value = toml::from_str(
            &std::fs::read_to_string(
                review.join(format!("task-packages/fixture/{name}/worker.toml")),
            )
            .unwrap(),
        )
        .unwrap();
        let wall = worker["signature"]["attempt"]["wall_ms"]
            .as_integer()
            .unwrap();
        assert!((wall as u64) < NATIVE_REVIEWER_WALL_MS, "{name}: {wall}");
    }
}

/// Issue #259: Claude Code 2.1.101 is authenticated but has no `--safe-mode` or `--restricted`.
/// The Task binding to it is refused at Provider admission with the compatibility report, before
/// any Worker process starts and before any token is charged; it is never worded as a login
/// problem, and no isolation flag is dropped to make the old CLI run.
#[test]
fn an_authenticated_claude_without_its_isolation_flags_is_refused_at_admission() {
    let old: String = crate::provider_cli::CLAUDE_HELP
        .lines()
        .filter(|line| !line.contains("--safe-mode") && !line.contains("--restricted"))
        .map(|line| format!("{line}\n"))
        .collect();
    native_model_drift_case_with(
        false,
        false,
        None,
        ClientUpdate::None,
        Some(Incompatible::Help(&old)),
    );
}

/// Issue #259, round 2: a codex whose `exec --help` is complete and whose account answers, but
/// whose bare app-server exits 2 before `initialize`, is refused at Provider admission with the
/// report `af provider status` and setup give, before any Worker process starts.
#[test]
fn a_codex_whose_app_server_exits_before_initialize_is_refused_at_admission() {
    native_model_drift_case_with(
        false,
        true,
        None,
        ClientUpdate::None,
        Some(Incompatible::AppServer),
    );
}

/// Issue #259, round 3: a codex whose bare app-server answers `initialize` with `{"id":1}`, no
/// result object, and exits 0 never proved the protocol; admission refuses it with the same
/// report, before any Worker process starts.
#[test]
fn a_codex_answering_initialize_without_a_result_is_refused_at_admission() {
    native_model_drift_case_with(
        false,
        true,
        None,
        ClientUpdate::None,
        Some(Incompatible::InitializeWithoutResult),
    );
}

/// Issue #259, round 4: a codex whose bare app-server answers `initialize` with an error object
/// and exits 0 is refused at admission with the report status and setup give — its exit status
/// and the error line — before any Worker process starts.
#[test]
fn a_codex_rejecting_initialize_is_refused_at_admission() {
    native_model_drift_case_with(
        false,
        true,
        None,
        ClientUpdate::None,
        Some(Incompatible::InitializeRejected),
    );
}

/// Issue #259, round 5: a codex whose bare app-server writes `{"foo":"bar"}`, neither a response
/// nor a notification, before any `initialize` answer and stays running is refused at admission
/// at once, with the line and the report that af stopped it, before any Worker process starts.
/// Every other case's app-server writes a notification and a response to another id before its
/// result, which are accepted. Round 6: `{"id":2,"foo":"bar"}`, `[1,2]` and `not json` are refused
/// by the same line grammar.
#[test]
fn a_codex_writing_a_malformed_object_before_initialize_is_refused_at_admission() {
    for line in [
        r#"{"foo":"bar"}"#,
        r#"{"id":2,"foo":"bar"}"#,
        "[1,2]",
        "not json",
    ] {
        native_model_drift_case_with(
            false,
            true,
            None,
            ClientUpdate::None,
            Some(Incompatible::InitializeMalformed(line)),
        );
    }
}

/// Issue #259, round 6: the identity probe reads the lines after the `initialize` answer by the
/// same grammar. A codex whose app-server answers `initialize` but writes `{"id":2,"foo":"bar"}`
/// in place of its `account/read` response and stays running is refused at admission at once as
/// missing the protocol, before any Worker process starts.
#[test]
fn a_codex_writing_a_malformed_line_before_its_identity_answer_is_refused_at_admission() {
    native_model_drift_case_with(
        false,
        true,
        None,
        ClientUpdate::None,
        Some(Incompatible::IdentityMalformed),
    );
}

/// Issue #259, round 7: a response answers a request only if af read it after writing the
/// request. A codex whose app-server writes a well-formed `account/read` answer for
/// `wrong@example.test` before its `initialize` answer, then exits on af's `account/read` without
/// answering it, is refused at admission for the missing identity answer: the premature line binds
/// no principal and no Worker process starts.
#[test]
fn a_codex_answering_account_read_before_it_was_asked_is_refused_at_admission() {
    native_model_drift_case_with(
        false,
        true,
        None,
        ClientUpdate::None,
        Some(Incompatible::IdentityPremature),
    );
}

/// An installed client that cannot serve af's invocation or answer its identity probe; the case
/// then ends at the admission refusal.
#[derive(Clone, Copy)]
enum Incompatible<'a> {
    /// A Claude Code whose help page is this one, without some option af passes.
    Help(&'a str),
    /// A codex whose bare app-server exits 2 before answering `initialize`.
    AppServer,
    /// A codex whose bare app-server answers `initialize` with `{"id":1}` and exits 0.
    InitializeWithoutResult,
    /// A codex whose bare app-server answers `initialize` with an error object and exits 0.
    InitializeRejected,
    /// A codex whose bare app-server writes this malformed line and stays running.
    InitializeMalformed(&'a str),
    /// A codex whose bare app-server answers `initialize`, then writes `{"id":2,"foo":"bar"}` in
    /// place of its `account/read` response and stays running.
    IdentityMalformed,
    /// A codex whose bare app-server writes a well-formed `account/read` answer for another
    /// account before its `initialize` answer, then exits 0 on af's `account/read` without
    /// answering it.
    IdentityPremature,
}

fn native_model_drift_case(
    wide: bool,
    codex: bool,
    switch_after: Option<usize>,
    client_update: ClientUpdate,
) {
    native_model_drift_case_with(wide, codex, switch_after, client_update, None);
}

/// `incompatible`, when given, is how the installed client cannot serve af's invocation; the case
/// then ends at the admission refusal.
fn native_model_drift_case_with(
    wide: bool,
    codex: bool,
    switch_after: Option<usize>,
    client_update: ClientUpdate,
    incompatible: Option<Incompatible>,
) {
    use af_pipeline_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "review");
    let home = directory.path().join("home");
    let bin = directory.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let email = home.join("account-email");
    std::fs::write(&email, "developer@example.test").unwrap();
    let calls = home.join("calls");
    if let Some(after) = switch_after {
        std::fs::write(home.join("switch-after"), after.to_string()).unwrap();
    }
    if wide {
        std::fs::write(home.join("wide-usage"), b"fixture").unwrap();
    }
    // The fake serves no model catalog: it ends the token-free catalog session of plan time at
    // once, so af plans every binding as unverified against the catalog (ADR-0148).
    let stub = r#"#!/usr/bin/python3
import os,json,sys
home=os.environ['CLAUDE_CONFIG_DIR']
if 'AF_TEST_PROVIDER_PROBE_TIMEOUT_MS' in os.environ: open(home+'/probe-setting-leaked','a').write(' '.join(sys.argv[1:3])+'\n')
if sys.argv[1:] in (['--help'],['--version']):
 sys.stdout.write(open(home+'/'+sys.argv[1][2:]).read())
 sys.exit(0)
if '--input-format' in sys.argv:
 sys.exit(0)
def update():
 import shutil
 try: os.mkdir(home+'/updated')
 except FileExistsError: return False
 link=open(home+'/update-link').read()
 new=os.path.join(os.path.dirname(sys.argv[0]),'2')
 shutil.copy2(sys.argv[0],new)
 os.symlink(new,link+'.new')
 os.replace(link+'.new',link)
 if os.path.isfile(home+'/update-removes-old'): os.remove(sys.argv[0])
 return True
update_at=open(home+'/update-at').read() if os.path.isfile(home+'/update-at') else ''
admitted=os.path.isfile(home+'/calls') and len(open(home+'/calls').readlines())==1
if sys.argv[1:3]==['auth','status']:
 if update_at.startswith('recheck') and admitted and update() and update_at=='recheck-fails': sys.exit(1)
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':open(home+'/account-email').read()}))
 sys.exit(0)
request=sys.stdin.read()
with open(home+'/calls','a') as f: f.write(open(home+'/account-email').read()+'\n')
with open(home+'/served-by','a') as f: f.write(sys.argv[0]+'\n')
if update_at=='admission' and len(open(home+'/calls').readlines())==1: update()
if os.path.isfile(home+'/switch-after') and len(open(home+'/calls').readlines())==int(open(home+'/switch-after').read()):
 with open(home+'/account-email','w') as f: f.write('changed@example.test')
# Under `stream-json` (Telemetry at `spans` and above) Claude prints its envelope as the
# `result` line; under `json` this fixture prints it without a `type`.
shape={'type':'result'} if 'stream-json' in sys.argv else {}
if os.path.isfile(home+'/wide-usage'):
 print(json.dumps({**shape,'is_error':True,'result':'fixture provider failed after reporting usage','usage':{'input_tokens':18446744073709551615,'output_tokens':20,'cache_creation_input_tokens':0}}))
 sys.exit(0)
if request==PROBE_INPUT:
 result='OK'
else:
 value=json.loads(request)
 assert set(value['inputs'])=={'source','subject','history','checks','assignment'}
 assert value['inputs']['assignment'][0]['payload']['findings']==[]
 result=json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'reports':[],'benchmark_demands':[],'dispositions':[]}]}})
envelope={**shape,'is_error':False,'result':result,'usage':{'input_tokens':10,'output_tokens':2,'cache_creation_input_tokens':0}}
if request!=PROBE_INPUT:
 assert '--json-schema' in sys.argv
 envelope['structured_output']=json.loads(result)
print(json.dumps(envelope))
"#;
    let codex_stub = r#"#!/usr/bin/python3
import os,json,sys
home=os.environ['CODEX_HOME']
if 'AF_TEST_PROVIDER_PROBE_TIMEOUT_MS' in os.environ: open(home+'/probe-setting-leaked','a').write(' '.join(sys.argv[1:3])+'\n')
if sys.argv[1:]==['exec','--help']:
 sys.stdout.write(open(home+'/help').read())
 sys.exit(0)
if sys.argv[1:]==['--version']:
 print('codex-cli 0.121.0')
 sys.exit(0)
if sys.argv[1:2]==['app-server'] and os.path.isfile(home+'/app-server-exits'):
 sys.stderr.write("error: unrecognized subcommand 'app-server'\n")
 sys.exit(2)
if sys.argv[1:2]==['app-server'] and os.path.isfile(home+'/app-server-no-result'):
 sys.stdin.readline()
 print('{"id":1}',flush=True)
 sys.exit(0)
if sys.argv[1:2]==['app-server'] and os.path.isfile(home+'/app-server-rejects'):
 sys.stdin.readline()
 print('{"id":1,"error":{"code":-32600,"message":"bad init"}}',flush=True)
 sys.exit(0)
if sys.argv[1:2]==['app-server'] and os.path.isfile(home+'/app-server-malformed'):
 sys.stdin.readline()
 print(open(home+'/app-server-malformed').read(),flush=True)
 for line in sys.stdin: pass
 sys.exit(0)
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  request=json.loads(line)
  if request.get('id')==1 and os.path.isfile(home+'/app-server-identity-premature'): print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':'wrong@example.test'}}})+'\n'+json.dumps({'id':1,'result':{}}),flush=True); continue
  if request.get('id')==1: print(json.dumps({'method':'remoteControl/status/changed','params':{}})+'\n'+json.dumps({'id':7,'result':{}})+'\n'+json.dumps({'id':1,'result':{}}),flush=True)
  if request.get('id')==2 and os.path.isfile(home+'/app-server-identity-premature'): sys.exit(0)
  if request.get('id')==2 and os.path.isfile(home+'/app-server-identity-malformed'): print('{"id":2,"foo":"bar"}',flush=True); continue
  if request.get('id')==2: print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':open(home+'/account-email').read()}}}),flush=True)
 sys.exit(0)
sys.stdin.read()
with open(home+'/calls','a') as f: f.write('model\n')
for i,c,o,r,w in [(18446744073709551615,7,18446744073709551615,18446744073709551615,18446744073709551615),(20,3,30,40,50)]:
 print(json.dumps({'type':'turn.completed','usage':{'input_tokens':i,'cached_input_tokens':c,'output_tokens':o,'reasoning_output_tokens':r,'cache_write_input_tokens':w}}))
print(json.dumps({'type':'turn.failed','error':{'message':'fixture failed after two paid turns'}}))
"#;
    let kind = if codex { "codex" } else { "claude" };
    let provider = format!("{kind}-personal");
    // What the installed client's capability check answers (ADR-0146).
    let old_help = match incompatible {
        Some(Incompatible::Help(page)) => Some(page),
        _ => None,
    };
    std::fs::write(
        home.join("help"),
        old_help.unwrap_or(crate::provider_cli::page(kind)),
    )
    .unwrap();
    if matches!(incompatible, Some(Incompatible::AppServer)) {
        std::fs::write(home.join("app-server-exits"), b"fixture").unwrap();
    }
    if matches!(incompatible, Some(Incompatible::InitializeWithoutResult)) {
        std::fs::write(home.join("app-server-no-result"), b"fixture").unwrap();
    }
    if matches!(incompatible, Some(Incompatible::InitializeRejected)) {
        std::fs::write(home.join("app-server-rejects"), b"fixture").unwrap();
    }
    if let Some(Incompatible::InitializeMalformed(line)) = incompatible {
        std::fs::write(home.join("app-server-malformed"), line).unwrap();
    }
    if matches!(incompatible, Some(Incompatible::IdentityMalformed)) {
        std::fs::write(home.join("app-server-identity-malformed"), b"fixture").unwrap();
    }
    if matches!(incompatible, Some(Incompatible::IdentityPremature)) {
        std::fs::write(home.join("app-server-identity-premature"), b"fixture").unwrap();
    }
    let version = if old_help.is_some() {
        "2.1.101"
    } else {
        "2.1.299"
    };
    std::fs::write(home.join("version"), format!("{version} (Claude Code)\n")).unwrap();
    // An updating client installs each version as its own file behind a launcher symlink.
    let versions = directory.path().join("versions");
    let installed = if client_update != ClientUpdate::None {
        std::fs::create_dir_all(&versions).unwrap();
        if client_update != ClientUpdate::KeepsOldVersion {
            std::fs::write(home.join("update-removes-old"), b"fixture").unwrap();
        }
        let update_at = match client_update {
            ClientUpdate::RemovesOldVersionDuringRecheck => "recheck",
            ClientUpdate::RemovesOldVersionAndFailsRecheck => "recheck-fails",
            _ => "admission",
        };
        std::fs::write(home.join("update-at"), update_at).unwrap();
        std::os::unix::fs::symlink(versions.join("1"), bin.join(kind)).unwrap();
        std::fs::write(home.join("update-link"), bin.join(kind).to_str().unwrap()).unwrap();
        versions.join("1")
    } else {
        bin.join(kind)
    };
    std::fs::write(
        &installed,
        if codex {
            codex_stub.to_owned()
        } else {
            crate::provider_cli::with_probe(stub)
        },
    )
    .unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry = home.join("providers.toml");
    std::fs::write(&registry,toml::to_string(&serde_json::json!({"version":4,"providers":[{"id":provider,"kind":kind,"auth_dir":home}]})).unwrap()).unwrap();
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
            provider_kind: kind.into(),
            model: format!("{kind}-fixture-1"),
            effort: "high".into(),
        };
        let attempt = worker.signature.attempt.as_mut().unwrap();
        attempt.tokens = 1000;
        attempt.wall_ms = NATIVE_REVIEWER_WALL_MS;
        std::fs::write(path.join("worker.toml"), toml::to_string(&worker).unwrap()).unwrap();
        catalog["packages"][&package]["digest"] =
            toml::Value::String(af_pipeline_config::lock::package_digest(&package, &path).unwrap());
        providers.insert(package, toml::Value::String(provider.clone()));
    }
    let pipeline_dir = repo.join(".af/task-packages/fixture/review");
    let mut pipeline: af_contracts::task::pipeline::PipelineDefinition =
        toml::from_str(&std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap())
            .unwrap();
    pipeline.max_attempts = 4;
    if switch_after.is_some() {
        pipeline.max_parallel = 1;
    }
    std::fs::write(
        pipeline_dir.join("pipeline.toml"),
        toml::to_string(&pipeline).unwrap(),
    )
    .unwrap();
    catalog["packages"]["fixture/review"]["digest"] = toml::Value::String(
        af_pipeline_config::lock::package_digest("fixture/review", &pipeline_dir).unwrap(),
    );
    catalog
        .as_table_mut()
        .unwrap()
        .insert("providers".into(), toml::Value::Table(providers));
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    let file_path = repo.join("review.json");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&file_path).unwrap()).unwrap();
    file["limits"] = native_model_limits();
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
    let started = std::time::Instant::now();
    let plan = run(&["review", "plan", "--file", "review.json"]);
    if matches!(
        incompatible,
        Some(
            Incompatible::InitializeMalformed(_)
                | Incompatible::IdentityMalformed
                | Incompatible::IdentityPremature
        )
    ) {
        // Refused at once, not waited on until the raised probe timeout: well within the
        // production one of 15 seconds.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "admission waited {:?}",
            started.elapsed()
        );
    }
    if let Some(incompatible) = incompatible {
        let program = std::fs::canonicalize(&installed).unwrap();
        let refusal = match incompatible {
            Incompatible::Help(_) => format!(
                "Task Provider admission refused before any Worker was dispatched: provider claude-personal: incompatible Provider CLI: the claude CLI `{}` (version 2.1.101 (Claude Code)) does not accept af's invocation; missing: --safe-mode, --restricted; fix: upgrade it with `npm install -g @anthropic-ai/claude-code@latest`, then rerun `af provider status`",
                program.display()
            ),
            Incompatible::AppServer => format!(
                "Task Provider admission refused before any Worker was dispatched: provider codex-personal: incompatible Provider CLI: the codex CLI `{}` (version codex-cli 0.121.0) does not accept af's invocation; missing: `codex app-server` answering JSON-RPC `initialize` over its default stdio transport; codex app-server exited 2 before answering initialize: error: unrecognized subcommand 'app-server'; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`",
                program.display()
            ),
            Incompatible::InitializeWithoutResult => format!(
                "Task Provider admission refused before any Worker was dispatched: provider codex-personal: incompatible Provider CLI: the codex CLI `{}` (version codex-cli 0.121.0) does not accept af's invocation; missing: `codex app-server` answering JSON-RPC `initialize` over its default stdio transport; codex app-server exited 0 after answering initialize without a result object: {{\"id\":1}}; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`",
                program.display()
            ),
            Incompatible::InitializeRejected => format!(
                "Task Provider admission refused before any Worker was dispatched: provider codex-personal: incompatible Provider CLI: the codex CLI `{}` (version codex-cli 0.121.0) does not accept af's invocation; missing: `codex app-server` answering JSON-RPC `initialize` over its default stdio transport; codex app-server exited 0 after answering initialize with an error: {{\"id\":1,\"error\":{{\"code\":-32600,\"message\":\"bad init\"}}}}; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`",
                program.display()
            ),
            Incompatible::InitializeMalformed(line) => format!(
                "Task Provider admission refused before any Worker was dispatched: provider codex-personal: incompatible Provider CLI: the codex CLI `{}` (version codex-cli 0.121.0) does not accept af's invocation; missing: `codex app-server` answering JSON-RPC `initialize` over its default stdio transport; codex app-server was stopped by af after answering initialize with a line that is not a JSON-RPC message: {line}; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`",
                program.display()
            ),
            // Unavailable, not incompatible: the app-server serves the protocol, it only never
            // answered the request af wrote.
            Incompatible::IdentityPremature => {
                "fixture/review: codex app-server exited 0 after initialize without answering `account/read`".to_string()
            }
            Incompatible::IdentityMalformed => format!(
                "Task Provider admission refused before any Worker was dispatched: provider codex-personal: incompatible Provider CLI: the codex CLI `{}` (version codex-cli 0.121.0) does not accept af's invocation; missing: `codex app-server` answering JSON-RPC `initialize` over its default stdio transport; codex app-server was stopped by af after answering initialize, before answering `account/read`, with a line that is not a JSON-RPC message: {{\"id\":2,\"foo\":\"bar\"}}; fix: upgrade it with `npm install -g @openai/codex@latest`, then rerun `af provider status`",
                program.display()
            ),
        };
        for refused in [plan, run(&["review", "run", "--file", "review.json"])] {
            assert!(!refused.status.success());
            let stderr = String::from_utf8_lossy(&refused.stderr);
            assert!(stderr.contains(&refusal), "{stderr}");
            assert!(!stderr.contains("login"), "{stderr}");
            assert!(!stderr.contains("wrong@example.test"), "{stderr}");
        }
        // No Worker process ran: the fixture records every model invocation it receives.
        assert!(
            !calls.exists(),
            "an incompatible CLI reached model dispatch"
        );
        let listed = run(&["task", "list"]);
        let listed: Value = serde_json::from_slice(&listed.stdout).unwrap_or(Value::Null);
        for task in listed["tasks"].as_array().into_iter().flatten() {
            assert_eq!(task["chargeable_tokens"], "0", "{task}");
        }
        return;
    }
    assert!(
        plan.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(!calls.exists(), "Planning made a model call");
    let planned: Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(planned["attempts"], 0);
    assert!(planned["graph"]["nodes"]["root.providers.admit0"].is_object());
    assert!(
        planned["graph"]["nodes"]["root.providers.admit1"].is_null(),
        "Identical capabilities did not share admission"
    );
    std::fs::write(&email, "changed@example.test").unwrap();
    let changed = run(&["task", "run", "--execute", "review-cli"]);
    assert!(!changed.status.success());
    assert!(!calls.exists(), "Changed account reached model dispatch");
    std::fs::write(&email, "developer@example.test").unwrap();
    let output = run(&["task", "run", "--execute", "review-cli"]);
    // `af` ran its probes under the raised probe timeout `crate::common::af` sets; neither a
    // probe nor a Worker's model call received that setting.
    assert!(
        !home.join("probe-setting-leaked").exists(),
        "{}",
        std::fs::read_to_string(home.join("probe-setting-leaked")).unwrap_or_default()
    );
    if let Some(after) = switch_after {
        // The Provider answered as another account: `provider_failed/identity_changed`.
        assert_eq!(
            output.status.code(),
            Some(6),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["chargeable_tokens"], (12 * after).to_string());
        assert_ne!(result["result"]["domain_conclusion"], "pass");
        let sent = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(sent.lines().count(), after);
        assert!(
            sent.lines().all(|line| line == "developer@example.test"),
            "private input reached another account"
        );
        let records = result["execution_records"].as_array().unwrap();
        assert!(
            records
                .iter()
                .any(|r| r["record"]["kind"] == "settled" && r["record"]["charged_tokens"] == "0"),
            "pre-send refusal must be known zero: {records:?}"
        );
        let shown = run(&["task", "show", "review-cli"]);
        assert!(shown.status.success());
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(shown["chargeable_tokens"], result["chargeable_tokens"]);
        assert_eq!(shown["attempts"], result["attempts"]);
        assert_eq!(std::fs::read_to_string(&calls).unwrap(), sent);
        let cas = af_store::Cas::open(state.join("cas")).unwrap();
        let store = af_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let projection = store.task_projection(&cas, "review-cli").unwrap().unwrap();
        assert_eq!(projection.revision.limits.tokens, 10000);
        let execution = projection.execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), (12 * after) as u128);
        assert!(
            execution.outputs.contains_key("root.providers.admit0"),
            "succeeded admission was lost"
        );
        assert!(execution.budget.begun_attempts() <= 4);
        return;
    }
    if wide {
        let exact = if codex {
            2 * u128::from(u64::MAX) + 40
        } else {
            u128::from(u64::MAX) + 20
        };
        assert_eq!(
            output.status.code(),
            Some(4),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["schema"], "af/task-inspection@20");
        assert_eq!(result["chargeable_tokens"], exact.to_string());
        assert_eq!(result["result"]["domain_conclusion"], "incomplete");
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
        let shown = run(&["task", "show", "review-cli"]);
        assert!(shown.status.success());
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(shown["chargeable_tokens"], result["chargeable_tokens"]);
        let listed = run(&["task", "list"]);
        assert!(listed.status.success());
        let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
        assert_eq!(listed["schema"], "af/task-list@4");
        assert_eq!(listed["tasks"][0]["schema"], "af/task-list-entry@4");
        assert_eq!(listed["tasks"][0]["chargeable_tokens"], exact.to_string());
        let text = crate::common::af()
            .current_dir(&repo)
            .args(["task", "list", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(text.status.success());
        let text = String::from_utf8_lossy(&text.stdout).into_owned();
        let row = text
            .lines()
            .find(|line| line.starts_with("review-cli"))
            .unwrap();
        assert!(
            row.split_whitespace().any(|cell| cell == exact.to_string()),
            "{text}"
        );
        let cas = af_store::Cas::open(state.join("cas")).unwrap();
        let store = af_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let projection = store.task_projection(&cas, "review-cli").unwrap().unwrap();
        let execution = projection.execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), exact);
        assert_eq!(execution.budget.begun_attempts(), 1);
        assert_eq!(projection.revision.limits.tokens, 10_000);
        let walls = store
            .task_attempt_wall(&af_store::store::task::task_run_id("review-cli").unwrap())
            .unwrap();
        let usage = walls[0].usage.as_ref().unwrap();
        assert_eq!(usage.chargeable_tokens.get(), exact);
        if codex {
            assert_eq!(usage.input_tokens.unwrap().get(), u128::from(u64::MAX) + 20);
            assert_eq!(
                usage.output_tokens.unwrap().get(),
                u128::from(u64::MAX) + 30
            );
        }
        let settled = result["execution_records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["record"]["kind"] == "settled")
            .unwrap();
        let id = settled["record"]["usage_id"].as_str().unwrap();
        assert_eq!(
            cas.get_artifact(id).unwrap().artifact_type,
            af_contracts::task::usage::TASK_TOKEN_USAGE_V3
        );
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
        return;
    }
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["attempts"], 4);
    assert_eq!(result["chargeable_tokens"], "36");
    assert_eq!(result["result"]["domain_conclusion"], "pass");
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 3);
    if client_update != ClientUpdate::None {
        // The update happened after the admission call's start, and both Workers after it
        // still ran, with no failed Attempt: on the executable this process captured while that
        // file exists, and on the installed one when the update deleted it.
        let updated = std::fs::canonicalize(versions.join("2")).unwrap();
        assert_eq!(std::fs::canonicalize(bin.join(kind)).unwrap(), updated);
        let captured = updated.with_file_name("1");
        let later = if client_update == ClientUpdate::KeepsOldVersion {
            &captured
        } else {
            assert!(!captured.exists());
            &updated
        };
        let served = std::fs::read_to_string(home.join("served-by")).unwrap();
        let served: Vec<&Path> = served.lines().map(Path::new).collect();
        assert_eq!(served, [&captured, later, later]);
    }
    assert!(
        run(&["task", "run", "--execute", "review-cli"])
            .status
            .success()
    );
    assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 3);
}

fn af(repo: &Path, state: &Path, args: &[&str]) -> Value {
    let output = crate::common::af()
        .current_dir(repo)
        .args(["task"])
        .args(args)
        .arg("--state")
        .arg(state)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "af task {args:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn captured_task_kind_packages_keep_business_names_and_domain_acceptance() {
    for (fixture_name, file_name, profile, kind, root, exit) in [
        (
            "embedded-review",
            "ticket.json",
            "reviewed_implementation",
            "team/feature",
            "implementation",
            0,
        ),
        (
            "review",
            "review.json",
            "review",
            "team/security-review",
            "review",
            3,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = fixture_named(directory.path(), fixture_name);
        let package_name = "team/task-kind";
        let package = repo.join(".af/task-packages/team/task-kind");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("kind.toml"), toml::to_string(&serde_json::json!({"schema":"af.task-kind/3","name":package_name,"version":"1.0.0","kind":kind,"profile":profile})).unwrap()).unwrap();
        let file = repo.join(file_name);
        let mut ticket: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        ticket["kind"] = Value::String(kind.into());
        ticket.as_object_mut().unwrap().remove("verification");
        std::fs::write(&file, serde_json::to_vec(&ticket).unwrap()).unwrap();
        let pipeline_dir = repo.join(format!(".af/task-packages/fixture/{root}"));
        let mut pipeline = af_pipeline_config::task::parse_task_pipeline(
            &std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap(),
        )
        .unwrap();
        pipeline.accepts.kinds = std::collections::BTreeSet::from([kind.into()]);
        std::fs::write(
            pipeline_dir.join("pipeline.toml"),
            toml::to_string(&pipeline).unwrap(),
        )
        .unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        catalog["packages"].as_table_mut().unwrap().insert(package_name.into(), toml::Value::try_from(serde_json::json!({"version":"1.0.0","digest":af_pipeline_config::lock::package_digest(package_name,&package).unwrap(),"path":".af/task-packages/team/task-kind"})).unwrap());
        catalog["packages"][format!("fixture/{root}")]["digest"] = toml::Value::String(
            af_pipeline_config::lock::package_digest(&format!("fixture/{root}"), &pipeline_dir)
                .unwrap(),
        );
        catalog.as_table_mut().unwrap().insert(
            "kinds".into(),
            toml::Value::try_from(serde_json::json!({kind:package_name})).unwrap(),
        );
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "kind package"]] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let plan = af(&repo, &state, &["plan", "--file", file_name]);
        assert!(plan["plan"]["dependencies"][package_name].is_object());
        let task_id = ticket["task_id"].as_str().unwrap();
        std::fs::write(package.join("kind.toml"), "modified after capture").unwrap();
        let output = crate::common::af()
            .current_dir(&repo)
            .args(["task", "run", "--execute", task_id, "--json", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["result"]["acceptance"], "satisfied");
        assert_eq!(result["plan_id"], plan["plan_id"]);
    }
}

fn embedded_review_never_accepts(case: &str) {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "embedded-review");
    let packages = repo.join(".af/task-packages/fixture");
    match case {
        "finding" => {
            let reply = serde_json::json!({"schema":"af.worker-reply/1","outputs":{"result":[{"reports":[{"severity":"major","file":"pagination.py","line":1,"title":"Missing validation","body":"Offset must reject negative values","fix":"Validate offset","confidence":0.9}],"benchmark_demands":[],"dispositions":[]}]}});
            std::fs::write(
                packages.join("correctness/worker.py"),
                format!(
                    "import json,sys\njson.load(sys.stdin)\nprint({:?})\n",
                    reply.to_string()
                ),
            )
            .unwrap();
        }
        "missing" => std::fs::write(
            packages.join("bugs/worker.py"),
            "raise Exception('review unavailable')\n",
        )
        .unwrap(),
        "failed_checks" => {
            let path = packages.join("implementer/worker.py");
            let source = std::fs::read_to_string(&path)
                .unwrap()
                .replace("items[offset:offset+limit]", "items[:1]");
            std::fs::write(path, source).unwrap();
        }
        "missing_coverage" => {
            let path = packages.join("review/pipeline.toml");
            let mut pipeline: af_contracts::task::pipeline::PipelineDefinition =
                af_pipeline_config::task::parse_task_pipeline(
                    &std::fs::read_to_string(&path).unwrap(),
                )
                .unwrap();
            pipeline.coverage.clear();
            pipeline
                .contract
                .outputs
                .get_mut("review")
                .unwrap()
                .covers
                .clear();
            std::fs::write(path, toml::to_string(&pipeline).unwrap()).unwrap();
        }
        "stale_checks" => {
            let path = packages.join("implementation/pipeline.toml");
            let mut pipeline: af_contracts::task::pipeline::PipelineDefinition =
                af_pipeline_config::task::parse_task_pipeline(
                    &std::fs::read_to_string(&path).unwrap(),
                )
                .unwrap();
            pipeline.nodes.push(serde_json::from_value(serde_json::json!({"id":"old_checks","operator":{"op":"check","checks":["pagination"]},"inputs":{"source":{"kind":"input","port":"source"}}})).unwrap());
            pipeline
                .nodes
                .iter_mut()
                .find(|node| node.id == "accept")
                .unwrap()
                .inputs
                .insert(
                    "checks".into(),
                    serde_json::from_value(
                        serde_json::json!({"kind":"node","node":"old_checks","port":"result"}),
                    )
                    .unwrap(),
                );
            std::fs::write(path, toml::to_string(&pipeline).unwrap()).unwrap();
        }
        _ => unreachable!(),
    }
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    for name in [
        "bugs",
        "correctness",
        "review",
        "implementation",
        "implementer",
    ] {
        let name_key = format!("fixture/{name}");
        catalog["packages"][&name_key]["digest"] = toml::Value::String(
            af_pipeline_config::lock::package_digest(&name_key, &packages.join(name)).unwrap(),
        );
    }
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [["add", "-A"], ["commit", "-qm"]] {
        let mut command = Command::new("git");
        command.current_dir(&repo).args(args);
        if args[0] == "commit" {
            command.arg(case);
        }
        assert!(command.status().unwrap().success());
    }
    let output = crate::common::af()
        .current_dir(&repo)
        .args([
            "task",
            "start",
            "--execute",
            "--file",
            "ticket.json",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    // A missing reviewer is a failed Worker (`provider_failed`, 6), a failed check a failed gate
    // (`gate_failed`, 5), never `changes_requested`.
    let code = if matches!(case, "missing_coverage" | "stale_checks") {
        1
    } else if case == "missing" {
        6
    } else if case == "failed_checks" {
        5
    } else {
        3
    };
    assert_eq!(
        output.status.code(),
        Some(code),
        "{case}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if code != 1 {
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_ne!(result["result"]["acceptance"], "satisfied", "{case}");
        assert!(
            result["result"]["missing_obligations"]
                .as_array()
                .unwrap()
                .contains(&Value::String("verified".into()))
        );
        assert_eq!(
            result["attempts"],
            if case == "failed_checks" { 2 } else { 5 }
        );
    }
}

#[test]
fn embedded_review_never_accepts_a_finding() {
    embedded_review_never_accepts("finding");
}

#[test]
fn embedded_review_never_accepts_a_missing_reviewer() {
    embedded_review_never_accepts("missing");
}

#[test]
fn embedded_review_never_accepts_failed_checks() {
    embedded_review_never_accepts("failed_checks");
}

#[test]
fn embedded_review_never_accepts_missing_coverage() {
    embedded_review_never_accepts("missing_coverage");
}

#[test]
fn embedded_review_never_accepts_stale_checks() {
    embedded_review_never_accepts("stale_checks");
}

#[test]
fn implementation_embeds_the_same_locked_review_and_one_task_budget() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "embedded-review");
    let plan = af(&repo, &state, &["plan", "--file", "ticket.json"]);
    assert_eq!(
        plan["graph"]["calls"]["root.nodes.review"]["pipeline"],
        "fixture/review"
    );
    assert_eq!(plan["attempts"], 0);
    let result = af(&repo, &state, &["run", "--execute", "pagination-cli"]);
    assert_eq!(
        result["attempts"], 5,
        "Implementation, checks, two reviewers and goal evaluation share five Attempts"
    );
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(
        result["result"]["outputs"]["verification"]["artifact_type"],
        "af/ReviewedImplementation@1"
    );
    let delivered = directory.path().join("delivered");
    af(
        &repo,
        &state,
        &[
            "deliver",
            "pagination-cli",
            "--confirm",
            "pagination-cli",
            "--branch",
            "reviewed-pagination",
            "--worktree",
            delivered.to_str().unwrap(),
        ],
    );
    let standalone_state = directory.path().join("standalone-state");
    let standalone_plan = af(
        &delivered,
        &standalone_state,
        &["plan", "--file", "review.json", "--uncommitted"],
    );
    assert_eq!(
        standalone_plan["plan"]["pipeline_id"],
        plan["plan"]["dependencies"]["fixture/review"]["artifact_id"]
    );
    let standalone = af(
        &delivered,
        &standalone_state,
        &["run", "--execute", "standalone-cli"],
    );
    assert_eq!(standalone["attempts"], 3);
    assert_eq!(standalone["result"]["domain_conclusion"], "pass");
    let review = &result["review_rounds"][0];
    let alone = &standalone["review_rounds"][0];
    assert_eq!(review["outcome"], alone["outcome"]);
    assert_eq!(review["conclusion"], alone["conclusion"]);
    let captured = af_store::Cas::open_existing(state.join("cas")).unwrap();
    let separate = af_store::Cas::open_existing(standalone_state.join("cas")).unwrap();
    for reviewer in ["bugs", "correctness"] {
        let embedded_result = captured
            .get_json(review["selected_results"][reviewer].as_str().unwrap())
            .unwrap();
        let standalone_result = separate
            .get_json(alone["selected_results"][reviewer].as_str().unwrap())
            .unwrap();
        assert_eq!(embedded_result["payload"], standalone_result["payload"]);
        let revision = captured
            .get_json(result["revision_id"].as_str().unwrap())
            .unwrap();
        let requirement = &revision["payload"]["inputs"]["requirements"]["artifact_ids"][0];
        assert!(
            embedded_result["input_artifacts"]
                .as_array()
                .unwrap()
                .contains(requirement)
        );
        assert!(
            !standalone_result["input_artifacts"]
                .as_array()
                .unwrap()
                .contains(requirement)
        );
        let verified = captured
            .get_json(
                result["result"]["outputs"]["evaluation"]["artifact_ids"][0]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let evaluation = captured
            .get_json(verified["payload"]["evaluation_id"].as_str().unwrap())
            .unwrap();
        assert!(
            evaluation["input_artifacts"]
                .as_array()
                .unwrap()
                .contains(requirement)
        );
        assert_eq!(
            evaluation["subject_snapshot_id"],
            result["result"]["outputs"]["snapshot"]["snapshot_id"]
        );
    }
}

#[test]
fn two_developers_share_one_pipeline_with_captured_private_worker_bindings() {
    use af_pipeline_config::task::catalog::TaskWorkerManifest;
    let temp = tempfile::tempdir().unwrap();
    let (repo, _) = fixture(temp.path());
    let mut pipeline_ids = Vec::new();
    let mut worker_ids = Vec::new();
    for developer in ["alice", "bob"] {
        let local = temp.path().join(developer);
        let package = local.join("worker");
        copy_tree(
            &repo.join(".af/task-packages/fixture/implementer"),
            &package,
        );
        let mut worker: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(package.join("worker.toml")).unwrap()).unwrap();
        worker.name = format!("local/{developer}");
        worker.signature.attempt.as_mut().unwrap().wall_ms =
            if developer == "alice" { 4500 } else { 4800 };
        std::fs::write(
            package.join("worker.toml"),
            toml::to_string(&worker).unwrap(),
        )
        .unwrap();
        let digest = af_pipeline_config::lock::package_digest(&worker.name, &package).unwrap();
        let bindings = local.join("bindings.toml");
        std::fs::write(&bindings, toml::to_string(&serde_json::json!({
            "schema":"af.task-bindings/1", "packages":{worker.name.clone():{"version":"1.0.0", "digest":digest,"path":"worker"}},
            "slots":{"root.slots.implementer":worker.name}
        })).unwrap()).unwrap();
        let state = temp.path().join(format!("state-{developer}"));
        let plan = af(
            &repo,
            &state,
            &[
                "plan",
                "--file",
                "ticket.json",
                "--bindings",
                bindings.to_str().unwrap(),
            ],
        );
        pipeline_ids.push(plan["plan"]["pipeline_id"].clone());
        worker_ids
            .push(plan["plan"]["bindings"]["root.slots.implementer"]["package_digest"].clone());
        assert_eq!(plan["attempts"], 0);
        assert_eq!(
            plan["graph"]["slots"]["root.slots.implementer"]["worker"],
            format!("local/{developer}")
        );
        std::fs::write(
            package.join("worker.py"),
            "raise Exception('changed local package must never execute')",
        )
        .unwrap();
        std::fs::write(&bindings, "invalid after capture").unwrap();
        let run = af(&repo, &state, &["run", "--execute", "pagination-cli"]);
        assert_eq!(run["plan_id"], plan["plan_id"]);
        assert_eq!(run["result"]["acceptance"], "satisfied");
        assert_eq!(run["attempts"], 3);
    }
    assert_eq!(pipeline_ids[0], pipeline_ids[1]);
    assert_ne!(worker_ids[0], worker_ids[1]);
}

#[test]
fn plan_then_run_uses_captured_inputs_and_does_not_repeat_finished_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(temp.path());
    let planned = af(&repo, &state, &["plan", "--file", "ticket.json"]);
    assert_eq!(planned["schema"], "af/task-inspection@20");
    assert_eq!(planned["attempts"], 0);
    assert!(planned["plan"].is_object());
    assert!(planned["graph"].is_object());
    assert!(state.join("events.sqlite").is_file());

    std::fs::write(repo.join("pagination.py"), "live source changed\n").unwrap();
    std::fs::write(repo.join(".af/code-policy.toml"), "invalid after planning").unwrap();
    std::fs::write(
        repo.join(".af/task-packages/fixture/implementer/worker.py"),
        "raise Exception('live Worker must not run')",
    )
    .unwrap();
    let run = af(&repo, &state, &["run", "--execute", "pagination-cli"]);
    assert_eq!(run["attempts"], 3);
    assert_eq!(run["result"]["acceptance"], "satisfied");
    assert_eq!(run["plan_id"], planned["plan_id"]);
    assert_eq!(
        std::fs::read_to_string(repo.join("pagination.py")).unwrap(),
        "live source changed\n"
    );
    let resumed = af(&repo, &state, &["run", "--execute", "pagination-cli"]);
    assert_eq!(resumed, run);
    let explained = af(&repo, &state, &["explain", "pagination-cli"]);
    assert_eq!(explained["plan"], planned["plan"]);
    assert_eq!(explained["graph"], planned["graph"]);
    let shown = af(&repo, &state, &["show", "pagination-cli"]);
    assert_eq!(shown, run);
    let listed = af(&repo, &state, &["list"]);
    assert_eq!(listed["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(listed["tasks"][0]["task_id"], "pagination-cli");
}

#[test]
fn task_start_runs_a_task_file_and_requires_one() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(temp.path());
    // `--file` is the only way to describe a Task; the fixed-format flags are gone.
    for args in [
        "start --execute",
        "start --kind implement --goal pagination",
        "start --file ticket.json --pipeline .af/pipelines/implement.toml",
    ] {
        let output = crate::common::af()
            .current_dir(&repo)
            .arg("task")
            .args(args.split(' '))
            .arg("--state")
            .arg(&state)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "af task {args}");
    }
    assert!(!state.exists());
    let run = af(
        &repo,
        &state,
        &["start", "--execute", "--file", "ticket.json"],
    );
    assert_eq!(run["result"]["acceptance"], "satisfied");
    assert_eq!(run["attempts"], 3);
    let destination = temp.path().join("delivered");
    let deliver = [
        "deliver",
        "pagination-cli",
        "--branch",
        "task/pagination",
        "--worktree",
        destination.to_str().unwrap(),
        "--confirm",
        "pagination-cli",
    ];
    let receipt = af(&repo, &state, &deliver);
    assert_eq!(receipt["outcome"]["kind"], "delivered");
    assert_eq!(receipt["remote_actions"], serde_json::json!([]));
    assert_eq!(af(&repo, &state, &deliver), receipt);
    assert!(
        std::fs::read_to_string(destination.join("pagination.py"))
            .unwrap()
            .contains("offset")
    );
}

#[test]
fn current_review_catalog_uses_exact_assignments_and_reopens_without_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "review-v2");
    let invoke = |args: &[&str]| {
        let output = crate::common::af()
            .current_dir(&repo)
            .args(args)
            .args(["--json", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(3),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let result = invoke(&["review", "run", "--file", "review.json"]);
    assert_eq!(result["attempts"], 6);
    assert_eq!(result["result"]["execution"], "completed");
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(
        result["result"]["domain_conclusion"],
        "convergence_exhausted"
    );
    let explained = invoke(&["task", "explain", "review-cli"]);
    let graph = serde_json::to_string(&explained["graph"]).unwrap();
    assert!(graph.contains("af/TaskReviewSubject@3"));
    assert!(graph.contains("af/TaskReviewAssignment@1"));
    assert!(graph.contains("review.kernel/ReviewerResult@2"));
    assert_eq!(invoke(&["task", "run", "--execute", "review-cli"]), result);
}

/// Native Review exposes valid proposed repairs even when its complete verdict requests changes.
/// All subprocesses use the isolated CLI helper; these command Workers spend no model tokens.
#[test]
fn native_review_facade_exports_non_nominated_repairs_and_refuses_false_original_claims() {
    use af_contracts::task::native_review::TASK_REVIEW_PROPOSAL_V1;
    use af_contracts::task::pipeline::{PipelineDefinition, TaskNodeV1};
    for fault in [
        "none",
        "paths",
        "report",
        "result",
        "stale",
        "credential",
        "archive",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = fixture_named(directory.path(), "review");
        let source_before = std::fs::read(repo.join("lib.rs")).unwrap();
        let packages = repo.join(".af/task-packages/fixture");
        let author = packages.join("propose");
        std::fs::create_dir_all(author.join("outputs")).unwrap();
        let source = serde_json::json!({"artifact_type":"af/SourceTree@1","cardinality":"one","optional":false,"affinity":{"kind":"unbound"},"covers":[]});
        let same = |ty: &str| serde_json::json!({"artifact_type":ty,"cardinality":"one","optional":false,"affinity":{"kind":"same_as","input":"source"},"covers":[]});
        let manifest = serde_json::json!({"schema":"af.worker/1","name":"fixture/propose","version":"1.0.0",
            "signature":{"contract":{"inputs":{"source":source,"review":same("af/TaskReviewRound@1")},"outputs":{"candidate":same("af/CandidateTree@1"),"proposal":same(TASK_REVIEW_PROPOSAL_V1)}},
            "effects":["read-source","write-source"],"roles":["implement"],"evidence":{},"retains":{"proposal":["source","review"]},"unbound_allowed":[],
            "worker_input_type":"af/ProposalInput@1","worker_output_type":TASK_REVIEW_PROPOSAL_V1,"attempt":{"tokens":0,"wall_ms":5000}},
            "runner":{"kind":"command","command":{"program":"/usr/bin/python3","args":[{"value":"-B","provenance":"literal"},{"value":"@package/worker.py","provenance":"literal"}]}}});
        std::fs::write(
            author.join("worker.toml"),
            toml::to_string(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(author.join("input.schema.json"), r#"{"type":"object","required":["source","review"],"additionalProperties":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"object"}}}"#).unwrap();
        std::fs::write(author.join("outputs/proposal.schema.json"), r#"{"type":"object","additionalProperties":false,"required":["result_id","paths","report_indices","evidence_ids","auto_apply_nominated"],"properties":{"result_id":{"type":"string"},"paths":{"type":"array"},"report_indices":{"type":"array"},"evidence_ids":{"type":"array"},"auto_apply_nominated":{"type":"boolean"}}}"#).unwrap();
        std::fs::write(author.join("worker.py"), format!(r#"import json,sys,pathlib
r=json.load(sys.stdin)
pathlib.Path('lib.rs').write_text('pub fn repaired() {{}}\n')
p={{'result_id':r['inputs']['review'][0]['payload']['selected_results']['correctness'],'paths':['lib.rs'],'report_indices':[0],'evidence_ids':[],'auto_apply_nominated':False}}
if {fault:?}=='stale': p['auto_apply_nominated']=True
if {fault:?}=='paths': p['paths']=['other.rs']
if {fault:?}=='report': p['report_indices']=[999]
if {fault:?}=='result': p['result_id']='sha256:'+'0'*64
print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'proposal':[p]}}}}))
"#)).unwrap();
        let model =
            (fault == "credential").then(|| credential_proposal_author(directory.path(), &author));
        let path = packages.join("review/pipeline.toml");
        let mut pipeline: PipelineDefinition =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        pipeline.slots.insert("propose".into(), serde_json::from_value(serde_json::json!({"worker":"fixture/propose","role":"implement","input_type":"af/ProposalInput@1","output_type":TASK_REVIEW_PROPOSAL_V1,"min_attempts":1,"max_attempts":1,"allow_local_replacement":false,"independent_from":["bugs","correctness"]})).unwrap());
        let node: TaskNodeV1 = serde_json::from_value(serde_json::json!({"id":"propose","operator":{"op":"worker","slot":"propose"},"inputs":{"source":{"kind":"input","port":"source"},"review":{"kind":"node","node":"reduce","port":"result"}}})).unwrap();
        pipeline.nodes.push(node);
        // This new fixture has one additional mandatory Proposal author.
        pipeline.max_attempts += 1;
        if fault == "stale" {
            export_integration(&repo, &mut pipeline);
        }
        let review_path = repo.join("review.json");
        let mut request: Value =
            serde_json::from_slice(&std::fs::read(&review_path).unwrap()).unwrap();
        request["limits"]["max_attempts"] =
            serde_json::json!(request["limits"]["max_attempts"].as_u64().unwrap() + 1);
        if fault == "archive" {
            request["issue"] = serde_json::json!({"kind":"local","path":"issue.json"});
            std::fs::write(repo.join("issue.json"), serde_json::to_vec(&serde_json::json!({"schema":"af.issue-input/1","id":"42","key":"AF-42","revision":"v1","summary":"Review current source","description":"Check the declared behavior","acceptance":{}})).unwrap()).unwrap();
            pipeline.contract.inputs.insert("requirements".into(), serde_json::from_value(serde_json::json!({"artifact_type":"af/Requirements@1","cardinality":"one","optional":false,"affinity":{"kind":"unbound"},"covers":[]})).unwrap());
        }
        if fault == "credential" {
            request["limits"]["tokens"] = serde_json::json!(10000);
            // Four protected verification slots plus the author and its admission.
            request["limits"]["max_attempts"] = serde_json::json!(6);
            request["limits"]["wall_ms"] = serde_json::json!(300000);
            request["limits"]["verification"] =
                serde_json::json!({"tokens":4096,"attempts":4,"wall_ms":60000});
            pipeline.max_attempts = 6;
        }
        if fault == "stale" {
            request["limits"]["max_attempts"] = serde_json::json!(7);
            request["limits"]["verification"]["attempts"] = serde_json::json!(6);
            request["limits"]["verification"]["wall_ms"] = serde_json::json!(30000);
        }
        std::fs::write(review_path, serde_json::to_vec(&request).unwrap()).unwrap();
        std::fs::write(&path, toml::to_string(&pipeline).unwrap()).unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        if fault == "credential" {
            catalog.as_table_mut().unwrap().insert(
                "providers".into(),
                toml::Value::try_from(serde_json::json!({"fixture/propose":"proposal-author"}))
                    .unwrap(),
            );
        }
        if fault == "stale" {
            catalog["review"]["max_rounds"] = toml::Value::Integer(2);
        }
        for name in ["propose", "review", "bugs", "correctness"] {
            let package = format!("fixture/{name}");
            let version = if name == "review" { "1.0.1" } else { "1.0.0" };
            catalog["packages"].as_table_mut().unwrap().insert(package.clone(), toml::Value::try_from(serde_json::json!({"version":version,"digest":af_pipeline_config::lock::package_digest(&package,&packages.join(name)).unwrap(),"path":format!(".af/task-packages/fixture/{name}")})).unwrap());
        }
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        for args in [
            vec!["add", "-A"],
            vec!["commit", "-qm", "native Proposal authority"],
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
        let cli = |args: &[&str]| {
            let mut command = crate::common::af();
            command
                .current_dir(&repo)
                .args(args)
                .arg("--state")
                .arg(&state);
            if let Some((registry, path)) = &model {
                command
                    .env("AF_PROVIDERS_FILE", registry)
                    .env("PATH", path)
                    .env("USER", "fixture");
            }
            command.output().unwrap()
        };
        let result = cli(&["review", "run", "--file", "review.json", "--json"]);
        assert_eq!(
            result.status.code(),
            Some(3),
            "{fault}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let document: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            document["result"]["domain_conclusion"],
            if fault == "stale" {
                "convergence_exhausted"
            } else {
                "changes_requested"
            }
        );
        let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
        let store = af_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let task = store.task_projection(&cas, "review-cli").unwrap().unwrap();
        let execution = task.execution.unwrap();
        let proposal =
            &execution.outputs["root.nodes.propose"].1.outputs["proposal"].artifact_ids[0];
        let exported = cli(&["review", "export", "review-cli", proposal]);
        if fault == "stale" {
            assert_eq!(exported.status.code(), Some(1));
            assert!(
                String::from_utf8_lossy(&exported.stderr).contains("is stale"),
                "{exported:?}"
            );
            assert!(exported.stdout.is_empty());
            let deliberate = cli(&["review", "export", "review-cli", proposal, "--allow-stale"]);
            assert!(deliberate.status.success(), "{deliberate:?}");
            assert!(String::from_utf8_lossy(&deliberate.stdout).contains("+pub fn repaired() {}"));
            assert_eq!(std::fs::read(repo.join("lib.rs")).unwrap(), source_before);
            let reopened = cli(&["review", "report", "review-cli", "--json"]);
            assert!(reopened.status.success());
        } else if matches!(fault, "none" | "credential" | "archive") {
            assert!(
                exported.status.success(),
                "{}",
                String::from_utf8_lossy(&exported.stderr)
            );
            let plan_id = document["plan_id"].as_str().unwrap();
            assert!(
                cli(&[
                    "review",
                    "export",
                    "review-cli",
                    proposal,
                    "--plan",
                    plan_id
                ])
                .status
                .success()
            );
            let false_plan = cli(&[
                "review",
                "export",
                "review-cli",
                proposal,
                "--plan",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            ]);
            assert_eq!(false_plan.status.code(), Some(1));
            assert!(false_plan.stdout.is_empty());
            let patch = String::from_utf8(exported.stdout).unwrap();
            assert!(
                patch.contains("diff --git a/lib.rs b/lib.rs")
                    && patch.contains("+pub fn repaired() {}"),
                "{patch}"
            );
            let round_id = &document["result"]["outputs"]["review"]["artifact_ids"][0];
            let round = cas.get_artifact(round_id.as_str().unwrap()).unwrap();
            let set = cas
                .get_artifact(round.payload["finding_set_id"].as_str().unwrap())
                .unwrap();
            let finding = set.payload["findings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["source"] == "correctness")
                .unwrap()["finding_id"]
                .as_str()
                .unwrap();
            assert!(
                cli(&["review", "export", "review-cli", "--finding", finding])
                    .status
                    .success()
            );
            for args in [
                vec!["review", "inspect", "review-cli", "--json"],
                vec!["review", "ledger", "review-cli", "--json"],
                vec!["review", "report", "review-cli", "--json"],
                vec!["review", "resume", "review-cli", "--json"],
            ] {
                let shown = cli(&args);
                assert!(
                    matches!(shown.status.code(), Some(0 | 3)),
                    "{}",
                    String::from_utf8_lossy(&shown.stderr)
                );
                let _: Value = serde_json::from_slice(&shown.stdout).unwrap();
            }
            assert_eq!(
                cli(&[
                    "review",
                    "export",
                    "review-cli",
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                ])
                .status
                .code(),
                Some(1)
            );
            if fault == "archive" {
                let mut issue: Value =
                    serde_json::from_slice(&std::fs::read(repo.join("issue.json")).unwrap())
                        .unwrap();
                issue["revision"] = serde_json::json!("v2");
                issue["description"] = serde_json::json!("Also inspect error handling");
                std::fs::write(repo.join("issue.json"), serde_json::to_vec(&issue).unwrap())
                    .unwrap();
                let refresh = cli(&["task", "refresh", "review-cli", "--json"]);
                assert!(refresh.status.success(), "{refresh:?}");
                let shown = cli(&[
                    "review",
                    "inspect",
                    "review-cli",
                    "--plan",
                    plan_id,
                    "--json",
                ]);
                assert!(shown.status.success(), "{shown:?}");
                let archived: Value = serde_json::from_slice(&shown.stdout).unwrap();
                assert_eq!(archived["schema"], "af/task-plan-inspection@6");
                assert_eq!(archived["current_plan"], false);
                assert_eq!(archived["graph"]["schema"], "af.compiled-task/6");
                let exported = cli(&[
                    "review",
                    "export",
                    "review-cli",
                    proposal,
                    "--plan",
                    plan_id,
                ]);
                assert!(exported.status.success(), "{exported:?}");
                assert_eq!(String::from_utf8(exported.stdout).unwrap(), patch);
                assert!(
                    !cli(&["review", "export", "review-cli", proposal])
                        .status
                        .success()
                );
                let current = cli(&["review", "show", "review-cli", "--json"]);
                assert!(current.status.success(), "{current:?}");
                let current: Value = serde_json::from_slice(&current.stdout).unwrap();
                assert_eq!(current["state"]["phase"], "waiting");
                assert_eq!(current["phase"]["reason"], "needs_resources");
                assert_eq!(current["attempts"], document["attempts"]);
                assert_eq!(current["chargeable_tokens"], document["chargeable_tokens"]);
                assert_ne!(current["plan_id"], document["plan_id"]);
            }
            assert_eq!(std::fs::read(repo.join("lib.rs")).unwrap(), source_before);
        } else {
            assert_eq!(
                exported.status.code(),
                Some(1),
                "{fault}: {}",
                String::from_utf8_lossy(&exported.stdout)
            );
            assert!(exported.stdout.is_empty());
        }
    }
}

/// The guide's advisory exports are real native documents; neither planning nor execution
/// manufactures a successful empty-Diff Review. No checks or Workers run in these refusals.
#[test]
fn native_review_preflight_commands_inspect_real_task_documents_and_refuse_empty_diff() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = native_review_with_base(directory.path());
    let cli = |args: &[&str]| {
        crate::common::af()
            .current_dir(&repo)
            .args(args)
            .arg("--state")
            .arg(&state)
            .output()
            .unwrap()
    };
    for action in ["plan", "run"] {
        let output = cli(&[
            "review",
            action,
            "--file",
            "review.json",
            "--base",
            "HEAD",
            "--candidate",
            "HEAD",
            "--json",
        ]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.to_lowercase().contains("empty"), "{error}");
        assert!(
            error.contains("--candidate")
                && error.contains("--base")
                && error.contains("--uncommitted"),
            "actionable selector fix: {error}"
        );
        let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
        let store = af_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        assert!(
            store.task_projection(&cas, "review-cli").unwrap().is_none(),
            "empty Diff cannot admit a Task"
        );
    }
    let planned = cli(&["review", "plan", "--file", "review.json", "--json"]);
    assert!(planned.status.success(), "{planned:?}");
    let explained = cli(&["task", "explain", "review-cli", "--json"]);
    assert!(explained.status.success(), "{explained:?}");
    let value: Value = serde_json::from_slice(&explained.stdout).unwrap();
    assert_eq!(value["schema"], "af/task-inspection@20");
    assert!(value["plan"].is_object() && value["graph"].is_object());
    assert_eq!(value["attempts"], 0);
    let source = value["plan"]["inputs"]["source"]["snapshot_id"]
        .as_str()
        .unwrap();
    let diff = cli(&[
        "task",
        "diff",
        "review-cli",
        "--snapshot",
        source,
        "--base",
        source,
        "--json",
    ]);
    assert!(diff.status.success(), "{diff:?}");
    let diff: Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert_eq!(diff["schema"], "af/task-diff@1");
    crate::schemas::valid(&crate::schemas::validator("task-diff-v1.json"), &diff);
    assert!(diff["files"].as_array().unwrap().is_empty());
}

/// A real checked Integration followed by its exact successor makes the original Proposal
/// stale. This fixture adds precisely one check and two reviewer Attempts to the original Round.
fn export_integration(
    repo: &Path,
    pipeline: &mut af_contracts::task::pipeline::PipelineDefinition,
) {
    use af_contracts::task::pipeline::{PortAffinityV1, TaskNodeV1};
    let root = |port: &str| serde_json::json!({"kind":"input","port":port});
    let from = |node: &str, port: &str| serde_json::json!({"kind":"node","node":node,"port":port});
    let mut push = |id: &str, op: Value, inputs: Value, conditional: bool| {
        let mut value = serde_json::json!({"id":id,"operator":op,"inputs":inputs});
        if conditional {
            value["when"] = serde_json::json!({"node":"prepare","outcome":"passed"});
        }
        pipeline
            .nodes
            .push(serde_json::from_value::<TaskNodeV1>(value).unwrap());
    };
    push(
        "prepare",
        serde_json::json!({"op":"review_integrate","reviewers":["correctness"],"protected_paths":[]}),
        serde_json::json!({"source":root("source"),"review":from("reduce","result"),"candidate_correctness":from("propose","candidate"),"proposal_correctness":from("propose","proposal")}),
        false,
    );
    push(
        "unpromoted",
        serde_json::json!({"op":"review_integration_source"}),
        serde_json::json!({"source":root("source"),"integration":from("prepare","result")}),
        true,
    );
    push(
        "post_check",
        serde_json::json!({"op":"check","checks":["syntax"]}),
        serde_json::json!({"source":from("unpromoted","source")}),
        true,
    );
    push(
        "commit",
        serde_json::json!({"op":"review_commit"}),
        serde_json::json!({"source":from("unpromoted","source"),"integration":from("prepare","result"),"checks":from("post_check","result")}),
        true,
    );
    push(
        "integrated_bind",
        serde_json::json!({"op":"review_bind_integrated"}),
        serde_json::json!({"source":from("unpromoted","source"),"history":from("commit","history"),"integration":from("commit","result")}),
        true,
    );
    for name in ["bugs", "correctness"] {
        push(
            &format!("integrated_{name}"),
            serde_json::json!({"op":"verify","slot":name}),
            serde_json::json!({"source":from("unpromoted","source"),"subject":from("integrated_bind","subject"),"assignment":from("integrated_bind",name),"history":from("commit","history"),"checks":from("post_check","result"),"requirements":root("requirements")}),
            true,
        );
    }
    push(
        "integrated_reduce",
        serde_json::json!({"op":"review_reduce"}),
        serde_json::json!({"source":from("unpromoted","source"),"subject":from("integrated_bind","subject"),"history":from("commit","history"),"checks":from("post_check","result"),"bugs":from("integrated_bugs","result"),"correctness":from("integrated_correctness","result")}),
        true,
    );
    for (port, selected) in [("review", "result"), ("history", "history")] {
        let id = format!("select_{port}");
        push(
            &id,
            serde_json::json!({"op":"select"}),
            serde_json::json!({"condition":from("prepare","result"),"passed":from("integrated_reduce",selected),"failed":from("reduce",selected),"inconclusive":from("reduce",selected)}),
            false,
        );
    }
    for (port, selected) in [("review", "result"), ("history", "history")] {
        let _ = selected;
        pipeline.contract.outputs.get_mut(port).unwrap().affinity = PortAffinityV1::Unbound {};
        pipeline.outputs.insert(
            port.into(),
            serde_json::from_value(from(&format!("select_{port}"), "output")).unwrap(),
        );
    }
    pipeline.coverage.insert(
        "reviewed".into(),
        serde_json::from_value(from("select_review", "output")).unwrap(),
    );
    pipeline.max_attempts = 7;
    for name in ["bugs", "correctness"] {
        pipeline.slots.get_mut(name).unwrap().max_attempts = 2;
    }
    // Preserve the original claim and supply explicit dispositions on the integrated Subject.
    for name in ["bugs", "correctness"] {
        std::fs::write(repo.join(format!(".af/task-packages/fixture/{name}/worker.py")), r#"import json,sys
r=json.load(sys.stdin)
a=r['inputs']['assignment'][0]['payload']
s=r['inputs']['subject'][0]['payload']
stage={'reports':[{'severity':'major','file':'lib.rs','line':1,'title':'Missing behavior','body':'The source needs repair','fix':'Apply the repair','confidence':0.9}] if s['round']==1 else [],'benchmark_demands':[],'dispositions':[{'finding_id':f['finding_id'],'position':'not_reproduced','reason':'Checked the complete current Subject'} for f in a['findings']]}
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[stage]}}))
"#).unwrap();
    }
}

/// A credential-bearing model author, driven by a hermetic fake CLI. The real model adapter
/// captures Provider admission and seals its sandbox; the fake inference spends no model tokens.
fn credential_proposal_author(root: &Path, author: &Path) -> (PathBuf, std::ffi::OsString) {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.join("fake-provider-bin");
    let auth = root.join("fake-auth");
    crate::fake_models::install("codex", &bin, &auth);
    let executable = bin.join("codex");
    let mut script = std::fs::read_to_string(&executable).unwrap();
    let offset = script.rfind("sys.exit(64)").unwrap();
    script.truncate(offset);
    script.push_str(r#"request=sys.stdin.read()
if request==PROBE_INPUT:
 message='OK'
else:
 import pathlib
 r=json.loads(request)
 pathlib.Path('lib.rs').write_text('pub fn repaired() {}\n')
 p={'result_id':r['inputs']['review'][0]['payload']['selected_results']['correctness'],'paths':['lib.rs'],'report_indices':[0],'evidence_ids':[],'auto_apply_nominated':True}
 message=json.dumps({'schema':'af.worker-reply/1','outputs':{'proposal':[p]}})
if '-o' in sys.argv: open(sys.argv[sys.argv.index('-o')+1],'w').write(message)
print(json.dumps({'type':'thread.started','thread_id':'synthetic-proposal'}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':message}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':10,'cached_input_tokens':0,'output_tokens':2,'reasoning_output_tokens':0,'cache_write_input_tokens':0}}))
"#);
    std::fs::write(&executable, crate::provider_cli::with_probe(&script)).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry = root.join("model-providers.toml");
    std::fs::write(
        &registry,
        format!(
            "version=4\n[[providers]]\nid='proposal-author'\nkind='codex'\nauth_dir={}\n",
            toml::Value::String(auth.display().to_string())
        ),
    )
    .unwrap();
    let path = author.join("worker.toml");
    let mut worker: Value = serde_json::to_value(
        toml::from_str::<toml::Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(),
    )
    .unwrap();
    worker["runner"] = serde_json::json!({"kind":"model","provider_kind":"codex","model":"gpt-6.1-sol","effort":"high"});
    worker["signature"]["attempt"] = serde_json::json!({"tokens":1000,"wall_ms":120000});
    std::fs::write(path, toml::to_string(&worker).unwrap()).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    (registry, path)
}

fn native_review_with_base(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, state) = fixture_named(root, "review-v2");
    let package = repo.join(".af/task-packages/fixture/review");
    let path = package.join("pipeline.toml");
    let mut pipeline: af_contracts::task::pipeline::PipelineDefinition =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    pipeline.contract.inputs.insert("base".into(), serde_json::from_value(serde_json::json!({"artifact_type":"af/SourceTree@1","cardinality":"one","optional":true,"affinity":{"kind":"unbound"},"covers":[]})).unwrap());
    for node in &mut pipeline.nodes {
        if matches!(
            node.operator,
            af_contracts::task::pipeline::TaskOperatorV1::ReviewBind {}
        ) {
            node.inputs.insert(
                "base".into(),
                serde_json::from_value(serde_json::json!({"kind":"input","port":"base"})).unwrap(),
            );
        }
    }
    std::fs::write(&path, toml::to_string(&pipeline).unwrap()).unwrap();
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    catalog["packages"]["fixture/review"]["digest"] =
        af_pipeline_config::lock::package_digest("fixture/review", &package)
            .unwrap()
            .into();
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [["add", "-A"], ["commit", "-qm"]] {
        let mut command = Command::new("git");
        command.current_dir(&repo).args(args);
        if args[0] == "commit" {
            command.arg("declare native Base selection");
        }
        assert!(command.status().unwrap().success());
    }
    (repo, state)
}

#[test]
fn native_review_facade_pins_independent_authority_candidate_and_base() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = native_review_with_base(directory.path());
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let authority = git(&["rev-parse", "HEAD"]);
    let original = std::fs::read(repo.join("lib.rs")).unwrap();
    let candidate_bytes = format!(
        "pub fn candidate_selected() {{}}\n{}",
        "// readable source change\n".repeat(40000)
    );
    std::fs::write(repo.join("lib.rs"), &candidate_bytes).unwrap();
    // Candidate configuration cannot replace the separately captured committed authority.
    std::fs::write(
        repo.join(".af/task-catalog.toml"),
        "candidate_must_not_be_authority = [\n",
    )
    .unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "candidate with untrusted authority edits"]);
    let candidate = git(&["rev-parse", "HEAD"]);
    let planned = crate::common::af()
        .current_dir(&repo)
        .args([
            "review",
            "plan",
            "--file",
            "review.json",
            "--authority",
            &authority,
            "--candidate",
            &candidate,
            "--base",
            &authority,
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert!(planned.status.success(), "{planned:?}");
    let plan: Value = serde_json::from_slice(&planned.stdout).unwrap();
    let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
    let snapshot = |port: &str| {
        let id = plan["plan"]["inputs"][port]["snapshot_id"]
            .as_str()
            .unwrap();
        af_source_git::task::read_snapshot(&cas, id).unwrap()
    };
    let (source, manifest) = snapshot("source");
    assert_eq!(
        af_source_git::task::read_origin(&cas, &source.origin_id)
            .unwrap()
            .source_revision(),
        Some(candidate.as_str())
    );
    assert_eq!(
        cas.get(&manifest.get("lib.rs").unwrap().content).unwrap(),
        candidate_bytes.as_bytes()
    );
    let (base, manifest) = snapshot("base");
    assert_eq!(
        af_source_git::task::read_origin(&cas, &base.origin_id)
            .unwrap()
            .source_revision(),
        Some(authority.as_str())
    );
    assert_eq!(
        cas.get(&manifest.get("lib.rs").unwrap().content).unwrap(),
        original
    );
    assert_ne!(
        plan["plan"]["inputs"]["source"]["snapshot_id"],
        plan["plan"]["inputs"]["base"]["snapshot_id"]
    );
    let resumed = crate::common::af()
        .current_dir(&repo)
        .args([
            "review",
            "resume",
            "review-cli",
            "--confirm-plan",
            plan["plan_id"].as_str().unwrap(),
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(resumed.status.code(), Some(3), "{resumed:?}");
    let result: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(result["plan_id"], plan["plan_id"]);
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(
        std::fs::read(repo.join("lib.rs")).unwrap(),
        candidate_bytes.as_bytes()
    );
}
