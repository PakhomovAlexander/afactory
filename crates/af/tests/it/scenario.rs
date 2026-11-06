//! Real CLI orchestration across ordinary Tasks and process restarts. Production adapters use
//! credential-free inert service transports; Git lifecycle tests also use real local remotes.
#[path = "scenario/workspace_lifecycle.rs"]
mod workspace_lifecycle;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Output;

const GITHUB_ENVIRONMENT_CHILD: &str = "AF_TEST_GITHUB_ENVIRONMENT_CHILD";
const GITHUB_ENVIRONMENT_HOMES: [(&str, &str); 6] = [
    ("HOME", "home"),
    ("XDG_CONFIG_HOME", "config"),
    ("XDG_STATE_HOME", "state"),
    ("XDG_CACHE_HOME", "cache"),
    ("XDG_DATA_HOME", "data"),
    ("XDG_BIN_HOME", "bin"),
];

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn output(repo: &Path, state: &Path, args: &[&str]) -> Output {
    let mut command = crate::common::af();
    command
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state);
    if let Some(root) = std::env::var_os(GITHUB_ENVIRONMENT_CHILD) {
        let root = PathBuf::from(root);
        // Only the isolated regression child forwards its asserted private homes. Apply
        // them after common::af's defaults, including for Fixture::start and approve.
        command.envs(GITHUB_ENVIRONMENT_HOMES.map(|(name, leaf)| (name, root.join(leaf))));
        for (name, leaf) in GITHUB_ENVIRONMENT_HOMES {
            let expected = root.join(leaf);
            assert_eq!(std::env::var_os(name).unwrap(), expected.as_os_str());
            assert_eq!(
                command.get_envs().find(|(key, _)| *key == name),
                Some((std::ffi::OsStr::new(name), Some(expected.as_os_str()))),
                "effective af environment for {name}"
            );
        }
    }
    command.output().unwrap()
}
fn value(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn cli(repo: &Path, state: &Path, args: &[&str]) -> Value {
    value(output(repo, state, args), 0)
}
fn valid(file: &str, v: &Value) {
    crate::schemas::valid(&crate::schemas::validator(file), v);
}
fn grant(operation: &str, destination: &Path) -> Value {
    json!({"adapter":"builtin","operation":operation,"destination":destination})
}
fn literal(kind: &str, data: Value) -> Value {
    json!({"source":"literal","value":{"type":kind,"value":data}})
}
fn step(id: &str, operation: Value, inputs: Value, port: &str) -> Value {
    json!({"id":id,"operation":operation,"inputs":inputs,"outputs":{"result":port},"when":null,"max_attempts":1,"timeout_ms":120000,"on_timeout":"fail"})
}
struct Fixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    policy: PathBuf,
    bundle: PathBuf,
    inputs: PathBuf,
    owned: PathBuf,
    machine: PathBuf,
    key: minisign::KeyPair,
}
impl Fixture {
    fn bare() -> Self {
        Self::bare_in(&std::env::temp_dir())
    }
    fn bare_in(parent: &Path) -> Self {
        // Machine authority requires canonical paths and safe ancestors. macOS temp roots
        // may use /var or /tmp aliases; tempfile's default directory mode also follows umask.
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(parent)
            .unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let repo = root_path.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let state = root_path.join("scenario-state");
        let policy = root_path.join("policy.json");
        let bundle = root_path.join("bundle.json");
        let inputs = root_path.join("inputs.json");
        let owned = root_path.join("owned");
        std::fs::create_dir(&owned).unwrap();
        let owned = owned.canonicalize().unwrap();
        let machine = root_path.join("machine");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&machine)
            .unwrap();
        let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        write(
            &policy,
            &json!({"schema":"af/scenario-policy@5","adapters":{},"task_plans":[],"task_templates":[],"file_roots":[owned],"decision_keys":{"owner":key.pk.to_box().unwrap().into_string()}}),
        );
        std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600)).unwrap();
        write(&inputs, &json!({}));
        let g = grant("write_owned_file", &owned);
        let steps = vec![
            step(
                "owner",
                json!({"kind":"decision","owner":"owner"}),
                json!({"subject":literal("text",json!("issue-1")),"revision":literal("text",json!("head-1"))}),
                "decision",
            ),
            step(
                "write",
                json!({"kind":"action","grant":g,"requires_decision":false,"required_condition":null}),
                json!({"file":literal("text",json!("note")),"content":literal("text",json!("owned content"))}),
                "owned_file",
            ),
        ];
        write(
            &bundle,
            &json!({"schema":"af/scenario-bundle@5","root":"main","definitions":{"main":{"schema":"af/scenario@5","inputs":{},"outputs":{"ok":"boolean"},"steps":steps,"result":{"ok":literal("boolean",json!(true))},"completion":[literal("boolean",json!(true))],"authority":{"task_plans":[],"task_templates":[],"grants":[g],"decision_owners":["owner"]},"limits":{"max_tokens":2000,"max_wall_ms":604800000,"max_dispatches":64}}}}),
        );
        Self {
            _root: root,
            repo,
            state,
            policy,
            bundle,
            inputs,
            owned,
            machine,
            key,
        }
    }
    fn start(&self, id: &str) -> Value {
        let pinned = cli(
            &self.repo,
            &self.state,
            &[
                "scenario",
                "validate",
                "--file",
                self.bundle.to_str().unwrap(),
                "--authority",
                self.policy.to_str().unwrap(),
            ],
        );
        valid("scenario-pin-v5.json", &pinned);
        cli(
            &self.repo,
            &self.state,
            &[
                "scenario",
                "start",
                id,
                "--file",
                self.bundle.to_str().unwrap(),
                "--authority",
                self.policy.to_str().unwrap(),
                "--inputs",
                self.inputs.to_str().unwrap(),
                "--confirm-pin",
                pinned["revision"].as_str().unwrap(),
                "--execute",
            ],
        )
    }
    fn approve(&self, id: &str) -> Value {
        let decision_files = tempfile::tempdir_in(&self.machine).unwrap();
        let payload = decision_files.path().join("payload");
        let request = cli(
            &self.repo,
            &self.state,
            &[
                "scenario",
                "decision-payload",
                id,
                "--out",
                payload.to_str().unwrap(),
            ],
        );
        valid("scenario-decision-v1.json", &request);
        let bytes = std::fs::read(&payload).unwrap();
        let signature = minisign::sign(
            Some(&self.key.pk),
            &self.key.sk,
            bytes.as_slice(),
            None,
            None,
        )
        .unwrap()
        .into_string();
        let envelope = json!({"schema":"af/scenario-signed-decision@1","request":request,"signature":signature});
        valid("scenario-signed-decision-v1.json", &envelope);
        let signed = decision_files.path().join("signed.json");
        write(&signed, &envelope);
        cli(
            &self.repo,
            &self.state,
            &["scenario", "decide", id, "--file", signed.to_str().unwrap()],
        )
    }
    fn install_service(&self) -> Value {
        let path = self.machine.join("service.py");
        std::fs::copy(workspace().join("fixtures/scenarios/service.py"), &path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let digest = format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(std::fs::read(&path).unwrap()))
        );
        let mut policy: Value =
            serde_json::from_slice(&std::fs::read(&self.policy).unwrap()).unwrap();
        policy["adapters"] = json!({"service":{"executable":path,"digest":digest,"operations":["observe","publish_pr"],"destinations":["fixture:repository"],"max_wall_ms":30000,"max_output_bytes":65536}});
        write(&self.policy, &policy);
        write(
            &self.machine.join("observation.json"),
            &json!({"revision":"head-1","checks_passed":true,"merged":false}),
        );
        policy
    }
}

#[test]
fn performance_scenario_native_task_submission_lost_response_wait_and_report_complete_without_models()
 {
    let f = Fixture::bare();
    std::fs::remove_dir(&f.repo).unwrap();
    let task_root = f._root.path().join("task-fixture");
    std::fs::create_dir(&task_root).unwrap();
    let (repo, task_state) = crate::task_cli::fixture_named(&task_root, "pagination");
    std::fs::rename(repo, &f.repo).unwrap();
    let plan = cli(
        &f.repo,
        &task_state,
        &["task", "plan", "--file", "ticket.json"],
    );
    let mut policy = f.install_service();
    policy["task_plans"] = json!([plan["plan_id"]]);
    policy["adapters"]["service"]["operations"] = json!(["observe", "experiment_submit_v1"]);
    write(&f.policy, &policy);
    write(
        &f.machine.join("observation.json"),
        &json!({"revision":"head-1","native_verified":true}),
    );
    let submit = json!({"adapter":"service","operation":"experiment_submit_v1","destination":"fixture:repository"});
    let observe =
        json!({"adapter":"service","operation":"observe","destination":"fixture:repository"});
    let candidate = json!({"source":"step","step":"candidate","port":"result"});
    let report = json!({"source":"step","step":"comparison","port":"result"});
    let bundle = json!({"schema":"af/scenario-bundle@5","root":"performance","definitions":{"performance":{
        "schema":"af/scenario@5","admission":null,"inputs":{},"outputs":{"candidate":"task","report":"observation"},
        "steps":[
            step("candidate", json!({"kind":"task","target":{"repo":f.repo,"state":task_state,
                "task_id":"pagination-cli","plan_id":plan["plan_id"],"revision_id":plan["revision_id"],"max_tokens":1000}}),json!({}),"task"),
            step("experiment",json!({"kind":"action","grant":submit,"requires_decision":false,"required_condition":null}),
                json!({"subject":literal("text",json!("declared-experiment")),"revision":literal("text",json!("head-1")),
                    "body":literal("text",json!("fake native adapter result; no measurement claim")),"candidate":candidate}),"action"),
            step("comparison",json!({"kind":"wait","grant":observe,"condition":"native_verified"}),
                json!({"subject":{"source":"project","value":{"source":"step","step":"experiment","port":"result"},
                    "projection":{"kind":"action_intent"}},"revision":literal("text",json!("head-1"))}),"observation")],
        "result":{"candidate":candidate,"report":report},"completion":[{"source":"project","value":report,
            "projection":{"kind":"observation_satisfied"}}],
        "authority":{"task_plans":[plan["plan_id"]],"task_templates":[],"grants":[submit,observe],"decision_owners":[]},
        "limits":{"max_tokens":1000,"max_wall_ms":3600000,"max_dispatches":8}}}});
    valid("scenario-bundle-v5.json", &bundle);
    write(&f.bundle, &bundle);
    std::fs::write(
        f.machine.join("lose-response"),
        b"lose one submission response",
    )
    .unwrap();
    let waiting = f.start("performance");
    assert_eq!(waiting["status"], "waiting");
    assert_eq!(waiting["waiting"]["kind"], "reconcile");
    let done = cli(&f.repo, &f.state, &["scenario", "resume", "performance"]);
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(
        done["frames"]["root"]["outputs"]["report"]["value"]["satisfied"],
        true
    );
    assert_eq!(
        std::fs::read_to_string(f.machine.join("effects"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        cli(&f.repo, &f.state, &["scenario", "resume", "performance"])["status"],
        "succeeded"
    );
}

#[test]
fn actual_task_child_decisions_actions_waits_and_restart_recovery_keep_final_evidence() {
    let f = Fixture::bare();
    // Reuse the existing captured command-Worker fixture rather than building a second Task
    // harness or permitting a real Provider CLI through PATH.
    std::fs::remove_dir(&f.repo).unwrap();
    let task_root = f._root.path().join("task-fixture");
    std::fs::create_dir(&task_root).unwrap();
    let (repo, task_state) = crate::task_cli::fixture_named(&task_root, "pagination");
    std::fs::rename(repo, &f.repo).unwrap();
    let plan = cli(
        &f.repo,
        &task_state,
        &["task", "plan", "--file", "ticket.json"],
    );
    let mut policy = f.install_service();
    policy["task_plans"] = json!([plan["plan_id"]]);
    write(&f.policy, &policy);
    let mut bundle: Value = serde_json::from_slice(
        &std::fs::read(workspace().join("fixtures/scenarios/issue-fix.json")).unwrap(),
    )
    .unwrap();
    for def in bundle["definitions"].as_object_mut().unwrap().values_mut() {
        for grant in def["authority"]["grants"].as_array_mut().unwrap() {
            if grant["adapter"] == "builtin" {
                grant["destination"] = json!(f.owned);
            }
        }
        for step in def["steps"].as_array_mut().unwrap() {
            if step["operation"]["grant"]["adapter"] == "builtin" {
                step["operation"]["grant"]["destination"] = json!(f.owned);
            }
            if step["operation"]["kind"] == "task" {
                step["operation"]["target"] = json!({"repo":f.repo,"state":task_state,"task_id":"pagination-cli","plan_id":plan["plan_id"],"revision_id":plan["revision_id"],"max_tokens":1000});
            }
        }
    }
    bundle["definitions"]["issue-fix"]["authority"]["task_plans"] = json!([plan["plan_id"]]);
    write(&f.bundle, &bundle);
    std::fs::copy(
        workspace().join("fixtures/scenarios/inputs.json"),
        &f.inputs,
    )
    .unwrap();
    let waiting = f.start("issue-fix");
    valid("scenario-run-v5.json", &waiting);
    assert_eq!(waiting["status"], "waiting");
    assert_eq!(waiting["waiting"]["kind"], "decision");
    assert_eq!(
        waiting["frames"]["root/child"]["steps"][0]["status"],
        "succeeded"
    );
    assert!(!waiting["frames"]["root/child"]["steps"][0]["skipped"].is_null());
    assert_eq!(
        waiting["frames"]["root"]["steps"][0]["outputs"]["result"]["value"]["result"]["acceptance"],
        "satisfied"
    );
    assert!(f.owned.join("candidate-note").is_file());
    assert!(!f.owned.join("unused").exists());
    let task = cli(&f.repo, &task_state, &["task", "show", "pagination-cli"]);
    let attempts = task["attempts"].clone();
    assert!(attempts.as_u64().unwrap() > 0);
    f.approve("issue-fix");
    std::fs::write(f.machine.join("lose-response"), b"lose one reply").unwrap();
    let uncertain = cli(&f.repo, &f.state, &["scenario", "resume", "issue-fix"]);
    assert_eq!(uncertain["status"], "waiting");
    assert_eq!(uncertain["waiting"]["kind"], "reconcile");
    let intent = uncertain["waiting"]["intent_id"].clone();
    // Definition edits have no authority over a recorded Run, including its child closure.
    std::fs::write(&f.bundle, b"not the pinned definition anymore").unwrap();
    let merged_wait = cli(&f.repo, &f.state, &["scenario", "resume", "issue-fix"]);
    assert_eq!(merged_wait["waiting"]["kind"], "condition");
    assert_eq!(
        merged_wait["frames"]["root"]["steps"][4]["intents"][0]["id"],
        intent
    );
    assert_eq!(
        std::fs::read_to_string(f.machine.join("effects"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    write(
        &f.machine.join("observation.json"),
        &json!({"revision":"head-1","checks_passed":true,"merged":true}),
    );
    let done = cli(&f.repo, &f.state, &["scenario", "resume", "issue-fix"]);
    valid("scenario-run-v5.json", &done);
    assert_eq!(done["status"], "succeeded");
    assert_eq!(
        done["frames"]["root"]["outputs"]["implementation"]["type"],
        "task"
    );
    assert_eq!(
        done["frames"]["root"]["outputs"]["cleanup"]["value"]["operation"],
        "cleanup_owned_file"
    );
    assert!(!f.owned.join("candidate-note").exists());
    let replay = cli(&f.repo, &f.state, &["scenario", "resume", "issue-fix"]);
    assert_eq!(done, replay);
    assert_eq!(
        cli(&f.repo, &task_state, &["task", "show", "pagination-cli"])["attempts"],
        attempts
    );
    assert_eq!(
        std::fs::read_to_string(f.machine.join("effects"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn cancellation_preserves_receipts_and_never_dispatches_later_effects() {
    let f = Fixture::bare();
    let run = f.start("cancelled");
    assert_eq!(run["status"], "waiting");
    let cancelled = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "cancel",
            "cancelled",
            "--reason",
            "operator cancelled",
        ],
    );
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["frames"], run["frames"]);
    assert_eq!(
        cli(&f.repo, &f.state, &["scenario", "resume", "cancelled"]),
        cancelled
    );
    assert!(!f.owned.join("note").exists());
    assert_eq!(cancelled["cancellation"]["reason"], "operator cancelled");
    valid("scenario-run-v5.json", &cancelled);
}

#[test]
fn cleanup_preserves_modified_content_and_unowned_destinations() {
    let f = Fixture::bare();
    let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    let main = &mut bundle["definitions"]["main"];
    let write_step = main["steps"][1].clone();
    let decision = main["steps"][0].clone();
    let cleanup = grant("cleanup_owned_file", &f.owned);
    main["authority"]["grants"]
        .as_array_mut()
        .unwrap()
        .push(cleanup.clone());
    main["steps"] = json!([
        write_step,
        decision,
        step(
            "cleanup",
            json!({"kind":"action","grant":cleanup,"requires_decision":false,"required_condition":null}),
            json!({"owned":{"source":"step","step":"write","port":"result"}}),
            "action"
        )
    ]);
    write(&f.bundle, &bundle);
    let waiting = f.start("modified");
    assert_eq!(waiting["status"], "waiting");
    std::fs::write(f.owned.join("note"), b"user's unexpected changes").unwrap();
    f.approve("modified");
    let failed = cli(&f.repo, &f.state, &["scenario", "resume", "modified"]);
    assert_eq!(failed["status"], "failed");
    assert!(failed["failure"].as_str().unwrap().contains("preserved"));
    assert_eq!(
        std::fs::read(f.owned.join("note")).unwrap(),
        b"user's unexpected changes"
    );
    // The next Run cannot adopt or overwrite that unowned destination.
    let refused = f.start("unowned");
    assert_eq!(refused["status"], "failed");
    assert_eq!(
        std::fs::read(f.owned.join("note")).unwrap(),
        b"user's unexpected changes"
    );
}

#[test]
fn cli_rejects_unauthorized_actions_stale_pins_and_corrupt_state() {
    let f = Fixture::bare();
    let pinned = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "validate",
            "--file",
            f.bundle.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
        ],
    );
    let error = value(
        output(
            &f.repo,
            &f.state,
            &[
                "scenario",
                "start",
                "bad",
                "--file",
                f.bundle.to_str().unwrap(),
                "--authority",
                f.policy.to_str().unwrap(),
                "--inputs",
                f.inputs.to_str().unwrap(),
                "--confirm-pin",
                "sha256:stale",
            ],
        ),
        1,
    );
    assert!(error["error"].as_str().unwrap().contains("confirmation"));
    assert!(!f.state.exists());
    let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    bundle["definitions"]["main"]["authority"]["grants"] = json!([]);
    write(&f.bundle, &bundle);
    let error = value(
        output(
            &f.repo,
            &f.state,
            &[
                "scenario",
                "validate",
                "--file",
                f.bundle.to_str().unwrap(),
                "--authority",
                f.policy.to_str().unwrap(),
            ],
        ),
        1,
    );
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("Unauthorized action")
    );
    write(&f.bundle, &pinned["bundle"]);
    let run = f.start("corrupt");
    assert_eq!(run["status"], "waiting");
    let c = rusqlite::Connection::open(f.state.join("runs/scenarios.sqlite")).unwrap();
    c.execute("UPDATE journal SET digest='damaged'", [])
        .unwrap();
    let error = value(
        output(&f.repo, &f.state, &["scenario", "resume", "corrupt"]),
        1,
    );
    assert!(error["error"].as_str().unwrap().contains("checksum"));
    assert!(!f.owned.join("note").exists());
}

#[test]
fn stale_observed_head_and_changed_adapter_bytes_fail_without_publication() {
    let f = Fixture::bare();
    f.install_service();
    let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    let grant =
        json!({"adapter":"service","operation":"observe","destination":"fixture:repository"});
    bundle["definitions"]["main"]["authority"]["grants"]
        .as_array_mut()
        .unwrap()
        .push(grant.clone());
    bundle["definitions"]["main"]["steps"].as_array_mut().unwrap().insert(0,step("checks",json!({"kind":"wait","grant":grant,"condition":"checks_passed"}),json!({"subject":literal("text",json!("issue-1")),"revision":literal("text",json!("head-1"))}),"observation"));
    write(&f.bundle, &bundle);
    write(
        &f.machine.join("observation.json"),
        &json!({"revision":"head-2","checks_passed":true}),
    );
    let stale = f.start("stale");
    assert_eq!(stale["status"], "failed");
    assert!(stale["failure"].as_str().unwrap().contains("stale"));
    write(
        &f.machine.join("observation.json"),
        &json!({"revision":"head-1","checks_passed":false}),
    );
    let waiting = f.start("changed");
    assert_eq!(waiting["status"], "waiting");
    std::fs::write(f.machine.join("service.py"), b"changed installed adapter").unwrap();
    let changed = cli(&f.repo, &f.state, &["scenario", "resume", "changed"]);
    assert_eq!(changed["status"], "failed");
    assert!(
        changed["failure"]
            .as_str()
            .unwrap()
            .contains("executable changed")
    );
    assert!(!f.machine.join("effects").exists());
}

#[test]
fn ant_cli_ticks_once_concurrently_and_pause_retains_a_waiting_run() {
    let f = Fixture::bare();
    let bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    let now = 1_800_000_000_000_u64 / 86_400_000 * 86_400_000;
    let due = now + 12 * 3_600_000;
    let ant = f._root.path().join("ant.json");
    write(
        &ant,
        &json!({"schema":"af/ant@5","id":"daily","scenario":bundle,"inputs":{},"schedule":{"timezone":"UTC","hour":12,"minute":0,"start_unix_ms":now},"owner":"owner","notification":"local:owner","execution":{"overlap":"skip","missed":"skip","lateness_ms":1000,"per_run":bundle["definitions"]["main"]["limits"],"rolling_tokens":2000,"rolling_window_ms":172800000}}),
    );
    let pin = cli(
        &f.repo,
        &f.state,
        &[
            "ant",
            "validate",
            "--file",
            ant.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
        ],
    );
    let added = cli(
        &f.repo,
        &f.state,
        &[
            "ant",
            "add",
            "--file",
            ant.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
            "--confirm-pin",
            pin["revision"].as_str().unwrap(),
            "--now",
            &now.to_string(),
        ],
    );
    assert_eq!(added["state"], "disabled");
    cli(
        &f.repo,
        &f.state,
        &["ant", "enable", "daily", "--now", &now.to_string()],
    );
    let mut commands = Vec::new();
    for _ in 0..2 {
        commands.push(
            crate::common::af()
                .current_dir(&f.repo)
                .args([
                    "ant",
                    "tick",
                    "--json",
                    "--now",
                    &due.to_string(),
                    "--state",
                ])
                .arg(&f.state)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let ticks: Vec<Value> = commands
        .into_iter()
        .map(|c| value(c.wait_with_output().unwrap(), 0))
        .collect();
    assert_eq!(
        ticks
            .iter()
            .map(|t| t["started"].as_array().unwrap().len())
            .sum::<usize>(),
        1
    );
    for tick in ticks {
        valid("ant-tick-v5.json", &tick);
    }
    let shown = cli(&f.repo, &f.state, &["ant", "show", "daily"]);
    assert_eq!(shown["occurrences"].as_array().unwrap().len(), 1);
    let run_id = shown["occurrences"][0]["outcome"]["run_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        cli(&f.repo, &f.state, &["scenario", "show", run_id])["status"],
        "waiting"
    );
    cli(
        &f.repo,
        &f.state,
        &["ant", "pause", "daily", "--now", &due.to_string()],
    );
    assert_eq!(
        cli(&f.repo, &f.state, &["scenario", "show", run_id])["status"],
        "waiting"
    );
    cli(
        &f.repo,
        &f.state,
        &["ant", "tick", "--now", &(due + 86_400_000).to_string()],
    );
    let paused = cli(&f.repo, &f.state, &["ant", "show", "daily"]);
    assert_eq!(
        paused["occurrences"][1]["outcome"]["kind"],
        "skipped_paused"
    );
    let expired = cli(&f.repo, &f.state, &["scenario", "show", run_id]);
    assert_eq!(expired["status"], "failed");
    assert!(expired["failure"].as_str().unwrap().contains("deadline"));
    valid("ant-state-v5.json", &paused);
}

#[test]
fn cancellation_requests_active_task_stop_and_retains_task_attempt_receipts() {
    use std::io::{BufRead, BufReader};
    let f = Fixture::bare();
    std::fs::remove_dir(&f.repo).unwrap();
    let task_root = f._root.path().join("task-fixture");
    std::fs::create_dir(&task_root).unwrap();
    let (repo, task_state) = crate::task_cli::fixture_named(&task_root, "pagination");
    std::fs::rename(repo, &f.repo).unwrap();
    let ready = f.machine.join("ready");
    let hold = f.machine.join("hold");
    nix::unistd::mkfifo(&ready, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    nix::unistd::mkfifo(&hold, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    let worker = f
        .repo
        .join(".af/task-packages/fixture/implementer/worker.py");
    let original = std::fs::read_to_string(&worker).unwrap();
    let rendezvous = format!(
        "import os\nwith open({}, 'w') as signal:\n signal.write(str(os.getpid())+'\\n')\n signal.flush()\nwith open({}, 'r') as hold:\n hold.read(1)\n",
        serde_json::to_string(&ready).unwrap(),
        serde_json::to_string(&hold).unwrap()
    );
    std::fs::write(&worker, format!("{rendezvous}{original}")).unwrap();
    crate::task_interrupt::commit_fixture(&f.repo);
    let plan = cli(
        &f.repo,
        &task_state,
        &["task", "plan", "--file", "ticket.json"],
    );
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
    policy["task_plans"] = json!([plan["plan_id"]]);
    write(&f.policy, &policy);
    let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    let task = step(
        "task",
        json!({"kind":"task","target":{"repo":f.repo,"state":task_state,"task_id":"pagination-cli","plan_id":plan["plan_id"],"revision_id":plan["revision_id"],"max_tokens":1000}}),
        json!({}),
        "task",
    );
    let later = bundle["definitions"]["main"]["steps"][1].clone();
    bundle["definitions"]["main"]["steps"] = json!([task, later]);
    bundle["definitions"]["main"]["authority"]["task_plans"] = json!([plan["plan_id"]]);
    write(&f.bundle, &bundle);
    let pin = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "validate",
            "--file",
            f.bundle.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
        ],
    );
    cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "start",
            "active",
            "--file",
            f.bundle.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
            "--inputs",
            f.inputs.to_str().unwrap(),
            "--confirm-pin",
            pin["revision"].as_str().unwrap(),
        ],
    );
    struct Driver(Option<std::process::Child>);
    impl Drop for Driver {
        fn drop(&mut self) {
            if let Some(child) = &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut driver = Driver(Some(
        crate::common::af()
            .current_dir(&f.repo)
            .args(["scenario", "resume", "active", "--json", "--state"])
            .arg(&f.state)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    // A FIFO rendezvous proves the Worker is dispatched; there is no sleep or timing assertion.
    let mut pid = String::new();
    BufReader::new(std::fs::File::open(&ready).unwrap())
        .read_line(&mut pid)
        .unwrap();
    assert!(pid.trim().parse::<u32>().unwrap() > 0);
    let key = format!(
        "sha256-{}-cancel.lock",
        review_core::hex::encode(&Sha256::digest(b"active"))
    );
    let handoff = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(f.state.join("runs").join(key))
        .unwrap();
    fs2::FileExt::lock_exclusive(&handoff).unwrap();
    let recorded = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "cancel",
            "active",
            "--reason",
            "stop active Task",
        ],
    );
    assert_eq!(recorded["status"], "cancelled");
    assert_eq!(
        recorded["cancellation"]["child_requests"]["root/task"]["state"],
        "requested"
    );
    assert_eq!(
        recorded["cancellation"]["child_requests"]["root/task"]["run"],
        1
    );
    drop(handoff);
    let cancelled = cli(&f.repo, &f.state, &["scenario", "resume", "active"]);
    assert_eq!(
        cancelled["cancellation"]["child_requests"]["root/task"]["state"],
        "confirmed"
    );
    assert_eq!(cancelled["status"], "cancelled");
    assert!(!cancelled["cancellation"]["child_requests"]["root/task"].is_null());
    let settled = value(driver.0.take().unwrap().wait_with_output().unwrap(), 0);
    assert_eq!(settled["status"], "cancelled");
    assert!(!f.owned.join("note").exists());
    let task = cli(&f.repo, &task_state, &["task", "show", "pagination-cli"]);
    assert_eq!(task["state"]["outcome"], "cancelled");
    assert!(task["attempts"].as_u64().unwrap() > 0);
    let replay = cli(&f.repo, &f.state, &["scenario", "resume", "active"]);
    assert_eq!(replay["status"], "cancelled");
    assert!(!f.owned.join("note").exists());
    assert_eq!(
        cli(&f.repo, &task_state, &["task", "show", "pagination-cli"])["attempts"],
        task["attempts"]
    );
}

#[test]
fn toml_authoring_runs_and_pins_the_same_contract_as_its_json_wire_form() {
    let mut f = Fixture::bare();
    let toml = f._root.path().join("approval.toml");
    std::fs::copy(
        workspace().join("fixtures/scenarios/local-decision.toml"),
        &toml,
    )
    .unwrap();
    let policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
    let policy_toml = f._root.path().join("policy.toml");
    std::fs::write(&policy_toml, toml::to_string_pretty(&policy).unwrap()).unwrap();
    std::fs::set_permissions(&policy_toml, std::fs::Permissions::from_mode(0o600)).unwrap();
    f.bundle = toml;
    f.policy = policy_toml;
    write(
        &f.inputs,
        &json!({"subject":{"type":"text","value":"issue-1"},"revision":{"type":"text","value":"head-1"}}),
    );
    let pin = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "validate",
            "--file",
            f.bundle.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
        ],
    );
    valid("scenario-pin-v5.json", &pin);
    let json_wire = f._root.path().join("wire.json");
    write(&json_wire, &pin["bundle"]);
    let replay = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "validate",
            "--file",
            json_wire.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
        ],
    );
    assert_eq!(pin, replay);
    assert_eq!(f.start("approval")["status"], "waiting");
    f.approve("approval");
    let done = cli(&f.repo, &f.state, &["scenario", "resume", "approval"]);
    assert_eq!(done["status"], "succeeded");
    assert_eq!(
        done["frames"]["root"]["outputs"]["approved"]["type"],
        "decision"
    );
}

impl Fixture {
    fn install_github(&self) {
        let examples = workspace().join("fixtures/scenarios/github");
        self.install_github_from(&examples);
    }
    fn install_github_from(&self, examples: &Path) {
        let mut config: Value =
            serde_json::from_slice(&std::fs::read(examples.join("config.example.json")).unwrap())
                .unwrap();
        let token = self.machine.join("token");
        std::fs::write(&token, b"github_pat_TEST_CREDENTIAL_ONLY\n").unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        config["token_file"] = json!(token);
        config["state_dir"] = json!(self.machine.join("github-receipts"));
        let config_path = self.machine.join("github-config.json");
        write(&config_path, &config);
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        valid("github-adapter-config-v1.json", &config);
        for name in ["publication", "notification", "followup"] {
            let payload: Value = serde_json::from_slice(
                &std::fs::read(examples.join(format!("{name}.json"))).unwrap(),
            )
            .unwrap();
            let schema = match name {
                "publication" => "github-publish-pr-v1.json",
                "notification" => "github-notify-v1.json",
                _ => "github-followup-v1.json",
            };
            valid(schema, &payload);
        }
        let executable = self.machine.join("github-adapter");
        let installed = std::process::Command::new("/usr/bin/python3")
            .arg("-IS")
            .arg(workspace().join("scripts/install-scenario-github-adapter.py"))
            .args(["--config", config_path.to_str().unwrap()])
            .args(["--output", executable.to_str().unwrap()])
            .env_clear()
            .env("HOME", &self.machine)
            .env("XDG_CONFIG_HOME", &self.machine)
            .output()
            .unwrap();
        assert!(
            installed.status.success(),
            "{}",
            String::from_utf8_lossy(&installed.stderr)
        );
        let production = std::fs::read(&executable).unwrap();
        let installed_digest = format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(&production))
        );
        assert_eq!(
            String::from_utf8(installed.stdout).unwrap().trim(),
            installed_digest
        );
        // All production application bytes remain in this one pinned executable. The
        // credential-free fixture replaces only stdlib HTTPS transport, before main runs;
        // production has no mock URL, import hook or environment-controlled test mode.
        let mut bytes = b"#!/usr/bin/python3 -IS\n".to_vec();
        bytes.extend(
            std::fs::read(workspace().join("fixtures/scenarios/github-transport.py")).unwrap(),
        );
        bytes.push(b'\n');
        bytes.extend(&production);
        std::fs::write(&executable, &bytes).unwrap();
        let digest = format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(&bytes))
        );
        let mut policy: Value =
            serde_json::from_slice(&std::fs::read(&self.policy).unwrap()).unwrap();
        policy["adapters"] = json!({"github":{"executable":executable,"digest":digest,"operations":["publish_pr","notify","followup","observe"],"destinations":["github:octo/project"],"max_wall_ms":30000,"max_output_bytes":65536}});
        write(&self.policy, &policy);
        // Copy fixture bytes, not sealed-source permissions: the bundle remains editable
        // and both the test and installed mock transport update private service state.
        std::fs::write(
            &self.bundle,
            std::fs::read(examples.join("bundle.json")).unwrap(),
        )
        .unwrap();
        let mut service = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.machine.join("mock-service.json"))
            .unwrap();
        service
            .write_all(&std::fs::read(examples.join("mock-service.json")).unwrap())
            .unwrap();
    }
    fn github_state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.machine.join("mock-service.json")).unwrap())
            .unwrap()
    }
}

#[test]
fn production_github_adapter_installs_from_symlinked_private_temp_root() {
    let parent = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let alias = parent.path().join("alias");
    std::os::unix::fs::symlink(parent.path().canonicalize().unwrap(), &alias).unwrap();
    let f = Fixture::bare_in(&alias);
    assert_ne!(f._root.path(), f.machine.parent().unwrap());
    assert_eq!(f.machine, f.machine.canonicalize().unwrap());
    for path in [f.machine.parent().unwrap(), &f.machine] {
        assert_eq!(
            path.metadata().unwrap().permissions().mode() & 0o7777,
            0o700
        );
    }
    // Exercise the real installer: it still checks ownership, ancestors and canonical paths.
    f.install_github();
    assert_eq!(f.github_state()["effects"], json!([]));
}

#[test]
fn production_github_adapter_runs_exact_head_pr_sequence_and_recovers_remote_effect_before_reply() {
    github_read_only_source_sequence(Fixture::bare());
}

#[test]
fn production_github_adapter_full_read_only_sequence_under_aliased_tmpdir_and_permissive_umask() {
    const TEST: &str = "scenario::production_github_adapter_full_read_only_sequence_under_aliased_tmpdir_and_permissive_umask";
    if let Some(root) = std::env::var_os(GITHUB_ENVIRONMENT_CHILD) {
        // Only the exact regression is relaunched. Never change the test runner's umask
        // or environment: other integration tests may run concurrently in that process.
        let root = PathBuf::from(root);
        assert_eq!(root, root.canonicalize().unwrap());
        for (name, leaf) in GITHUB_ENVIRONMENT_HOMES {
            let path = PathBuf::from(std::env::var_os(name).unwrap());
            assert_eq!(path, root.join(leaf));
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }
        let tmpdir = PathBuf::from(std::env::var_os("TMPDIR").unwrap());
        assert_eq!(tmpdir, root.join("tmp-alias"));
        assert!(tmpdir.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::env::temp_dir(), tmpdir);
        assert_eq!(tmpdir.canonicalize().unwrap(), root.join("tmp"));
        assert_ne!(tmpdir, tmpdir.canonicalize().unwrap());
        // A fresh 0777 directory proves every inherited umask bit is zero without
        // mutating it. Its private ancestor still prevents access by other users.
        let probe = root.join("umask-probe");
        std::fs::DirBuilder::new()
            .mode(0o777)
            .create(&probe)
            .unwrap();
        assert_eq!(
            probe.metadata().unwrap().permissions().mode() & 0o777,
            0o777
        );
        std::fs::remove_dir(probe).unwrap();
        let f = Fixture::bare();
        assert!(f._root.path().starts_with(&tmpdir));
        assert_eq!(f.machine, f.machine.canonicalize().unwrap());
        for path in [&root, f.machine.parent().unwrap(), &f.machine] {
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }
        github_read_only_source_sequence(f);
        return;
    }

    let parent = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = parent.path().canonicalize().unwrap();
    for leaf in GITHUB_ENVIRONMENT_HOMES
        .map(|(_, leaf)| leaf)
        .into_iter()
        .chain(["tmp"])
    {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join(leaf))
            .unwrap();
    }
    let alias = root.join("tmp-alias");
    std::os::unix::fs::symlink(root.join("tmp"), &alias).unwrap();
    // Positional arguments keep paths out of shell syntax. exec preserves the child
    // identity; the marker takes the child branch instead of launching recursively.
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "umask 000; exec \"$1\" --exact \"$2\" --nocapture --test-threads=1",
            "github-environment-regression",
        ])
        .arg(std::env::current_exe().unwrap())
        .arg(TEST)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("AF_WORKSPACE_ROOT", workspace())
        .env(GITHUB_ENVIRONMENT_CHILD, &root)
        .env("TMPDIR", &alias)
        .envs(GITHUB_ENVIRONMENT_HOMES.map(|(name, leaf)| (name, root.join(leaf))))
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed; 0 failed"));
}

fn github_read_only_source_sequence(f: Fixture) {
    // Reproduce sealed gate sources without changing permissions in the source Snapshot.
    let sources = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&f.repo)
        .unwrap();
    assert_eq!(
        sources.path().metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    let examples = workspace().join("fixtures/scenarios/github");
    let mut originals = Vec::new();
    for name in [
        "config.example.json",
        "publication.json",
        "notification.json",
        "followup.json",
        "bundle.json",
        "mock-service.json",
    ] {
        let bytes = std::fs::read(examples.join(name)).unwrap();
        let path = sources.path().join(name);
        std::fs::write(&path, &bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        originals.push((path, bytes));
    }
    f.install_github_from(sources.path());
    let service_path = f.machine.join("mock-service.json");
    assert_eq!(
        service_path.metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_ne!(f.bundle.metadata().unwrap().permissions().mode() & 0o200, 0);
    let waiting = f.start("github-pr");
    assert_eq!(waiting["status"], "waiting");
    assert_eq!(waiting["waiting"]["kind"], "decision");
    let pin = waiting["pin"].clone();
    f.approve("github-pr");
    let mut remote = f.github_state();
    remote["crash_after_effect"] = json!(true);
    write(&f.machine.join("mock-service.json"), &remote);
    let uncertain = cli(&f.repo, &f.state, &["scenario", "resume", "github-pr"]);
    assert_eq!(uncertain["status"], "waiting");
    assert_eq!(uncertain["waiting"]["kind"], "reconcile");
    let intent = uncertain["waiting"]["intent_id"].clone();
    let review = cli(&f.repo, &f.state, &["scenario", "resume", "github-pr"]);
    assert_eq!(review["status"], "waiting");
    assert_eq!(review["waiting"]["kind"], "decision");
    assert_eq!(
        review["frames"]["root"]["steps"][1]["intents"][0]["id"],
        intent
    );
    assert_eq!(review["frames"]["root"]["steps"][1]["status"], "succeeded");
    assert_eq!(review["frames"]["root"]["steps"][2]["status"], "succeeded");
    assert_eq!(f.github_state()["effects"], json!(["publish_pr:1"]));
    f.approve("github-pr");
    let merge_wait = cli(&f.repo, &f.state, &["scenario", "resume", "github-pr"]);
    assert_eq!(merge_wait["waiting"]["kind"], "condition");
    assert_eq!(
        f.github_state()["effects"],
        json!(["publish_pr:1", "notify:1"])
    );
    let mut remote = f.github_state();
    remote["prs"]["1"]["state"] = json!("closed");
    remote["prs"]["1"]["merged"] = json!(true);
    remote["prs"]["1"]["merged_at"] = json!("2026-11-02T00:00:00Z");
    remote["prs"]["1"]["merge_commit_sha"] = json!("2222222222222222222222222222222222222222");
    write(&f.machine.join("mock-service.json"), &remote);
    let followup = cli(&f.repo, &f.state, &["scenario", "resume", "github-pr"]);
    assert_eq!(followup["waiting"]["kind"], "decision");
    f.approve("github-pr");
    let done = cli(&f.repo, &f.state, &["scenario", "resume", "github-pr"]);
    assert_eq!(done["status"], "succeeded", "{done:#}");
    assert_eq!(done["pin"], pin);
    valid("scenario-run-v5.json", &done);
    let journal: Value = serde_json::from_slice(
        &std::fs::read(f.machine.join("github-receipts/state.json")).unwrap(),
    )
    .unwrap();
    valid("github-adapter-state-v4.json", &journal);
    assert_eq!(journal["records"].as_object().unwrap().len(), 3);
    assert_eq!(
        f.github_state()["effects"],
        json!(["publish_pr:1", "notify:1", "followup:2"])
    );
    assert_eq!(
        cli(&f.repo, &f.state, &["scenario", "resume", "github-pr"]),
        done
    );
    assert_eq!(
        service_path.metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    for (path, bytes) in originals {
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o444);
    }
}

#[test]
fn production_github_adapter_changed_executable_digest_refuses_dispatch() {
    let f = Fixture::bare();
    f.install_github();
    assert_eq!(f.start("github-digest")["status"], "waiting");
    f.approve("github-digest");
    std::fs::write(
        f.machine.join("github-adapter"),
        b"changed executable bytes",
    )
    .unwrap();
    let result = cli(&f.repo, &f.state, &["scenario", "resume", "github-digest"]);
    assert_eq!(result["status"], "waiting");
    assert_eq!(result["waiting"]["kind"], "reconcile");
    assert_eq!(f.github_state()["effects"], json!([]));
}

#[test]
fn roadmap_cli_restart_revision_confirmation_and_untrusted_progress_refusal() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let state = root.path().join("state");
    let file = root.path().join("roadmap.json");
    let mut proposal: Value = serde_json::from_slice(
        &std::fs::read(workspace().join("fixtures/scenarios/performance-roadmap.json")).unwrap(),
    )
    .unwrap();
    write(&file, &proposal);
    let args = ["--file", file.to_str().unwrap()];
    let pin = cli(
        &repo,
        &state,
        &[&["scenario", "roadmap", "validate"][..], &args].concat(),
    );
    valid("scenario-roadmap-revision-v2.json", &pin);
    assert!(
        !state.exists(),
        "validation creates no Store or execution state"
    );
    assert!(
        !output(
            &repo,
            &state,
            &[
                &["scenario", "roadmap", "create"][..],
                &args,
                &["--confirm-revision", "wrong"]
            ]
            .concat()
        )
        .status
        .success()
    );
    assert!(!state.exists(), "confirmation precedes creation");
    let create = [
        &["scenario", "roadmap", "create"][..],
        &args,
        &["--confirm-revision", pin["revision"].as_str().unwrap()],
    ]
    .concat();
    let first = cli(&repo, &state, &create);
    valid("scenario-roadmap-v2.json", &first);
    assert_eq!(first["progress"]["store-replay"]["state"], "pending");
    assert_eq!(first["progress"]["store-replay"]["actual_tokens"], 0);
    assert_eq!(first["progress"]["store-replay"]["reserved_tokens"], 0);
    assert_eq!(
        first["progress"]["store-replay"]["task_receipts"],
        json!([])
    );
    assert_eq!(
        cli(
            &repo,
            &state,
            &["scenario", "roadmap", "show", "performance"]
        ),
        first
    );
    assert!(
        !output(&repo, &state, &create).status.success(),
        "create cannot reset history"
    );
    let other = cli(&repo, &root.path().join("other-store"), &create);
    assert_ne!(
        first["identity"], other["identity"],
        "equal labels in distinct Stores have separate stable goal namespaces"
    );
    proposal["goals"][0]["estimated_tokens"] = json!(3000);
    write(&file, &proposal);
    let planned = cli(
        &repo,
        &state,
        &[
            &["scenario", "roadmap", "validate"][..],
            &args,
            &["--roadmap", "performance"],
        ]
        .concat(),
    );
    assert_eq!(planned["parent"], pin["revision"]);
    let revise = [
        &["scenario", "roadmap", "revise", "performance"][..],
        &args,
        &["--confirm-revision", planned["revision"].as_str().unwrap()],
    ]
    .concat();
    let revised = cli(&repo, &state, &revise);
    assert_eq!(revised["identity"], first["identity"]);
    assert_eq!(revised["progress"], first["progress"]);
    assert_eq!(revised["revisions"][0], first["revisions"][0]);
    assert_eq!(revised["revisions"].as_array().unwrap().len(), 2);
    assert!(
        !output(&repo, &state, &revise).status.success(),
        "stale revision confirmation fails after restart"
    );
    let list = cli(&repo, &state, &["scenario", "roadmap", "list"]);
    valid("scenario-roadmap-list-v2.json", &list);
    assert_eq!(list["roadmaps"]["performance"], revised);
    for (key, value) in [
        ("actual_tokens", json!(2000)),
        ("state", json!("completed")),
        ("completed_work", json!(["claimed experiment"])),
    ] {
        let mut forged = proposal.clone();
        forged["goals"][0][key] = value;
        write(&file, &forged);
        assert!(
            !output(
                &repo,
                &state,
                &[&["scenario", "roadmap", "validate"][..], &args].concat()
            )
            .status
            .success()
        );
        assert!(!output(&repo, &state, &revise).status.success());
    }
    assert_eq!(
        cli(
            &repo,
            &state,
            &["scenario", "roadmap", "show", "performance"]
        ),
        revised
    );
    let runs = cli(&repo, &state, &["scenario", "list"]);
    let ants = cli(&repo, &state, &["ant", "list"]);
    assert_eq!(runs["runs"], json!({}));
    assert_eq!(ants["ants"], json!({}));
    // Native command catalog and generated completions see the nested public API.
    let catalog = crate::common::af()
        .args(["help", "scenario", "roadmap", "create", "--json"])
        .output()
        .unwrap();
    let catalog = value(catalog, 0);
    assert_eq!(catalog["scope"], "af scenario roadmap create");
    let completion = crate::common::af()
        .env("COMPLETE", "fish")
        .args(["--", "af", "scenario", "roadmap", ""])
        .output()
        .unwrap();
    assert!(completion.status.success());
    let candidates = String::from_utf8_lossy(&completion.stdout);
    for command in ["validate", "create", "revise", "show", "list"] {
        assert!(candidates.contains(command), "{candidates}");
    }
}

#[test]
fn roadmap_cli_refuses_checkout_state_symlinked_journal_and_missing_ownership() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(repo.join(".git")).unwrap();
    let file = workspace().join("fixtures/scenarios/performance-roadmap.json");
    let state = root.path().join("state");
    let pin = cli(
        &repo,
        &state,
        &[
            "scenario",
            "roadmap",
            "validate",
            "--file",
            file.to_str().unwrap(),
        ],
    );
    let args = [
        "scenario",
        "roadmap",
        "create",
        "--file",
        file.to_str().unwrap(),
        "--confirm-revision",
        pin["revision"].as_str().unwrap(),
    ];
    assert!(
        !output(&repo, &repo.join("private-state"), &args)
            .status
            .success()
    );
    assert!(!repo.join("private-state").exists());
    let initial = cli(&repo, &state, &args);
    use std::os::unix::{fs::MetadataExt, fs::symlink};
    for path in [
        &state,
        &state.join("runs"),
        &state.join("runs/scenarios.sqlite"),
    ] {
        let meta = path.metadata().unwrap();
        assert_eq!(meta.uid(), nix::unistd::geteuid().as_raw());
        assert_eq!(meta.mode() & 0o077, 0, "private state: {}", path.display());
    }
    let db = state.join("runs/scenarios.sqlite");
    let backup = root.path().join("journal");
    std::fs::rename(&db, &backup).unwrap();
    symlink(&backup, &db).unwrap();
    assert!(
        !output(
            &repo,
            &state,
            &["scenario", "roadmap", "show", "performance"]
        )
        .status
        .success()
    );
    std::fs::remove_file(&db).unwrap();
    std::fs::rename(&backup, &db).unwrap();
    assert_eq!(
        cli(
            &repo,
            &state,
            &["scenario", "roadmap", "show", "performance"]
        ),
        initial
    );
    std::fs::remove_file(state.join("runs/scenario-owner")).unwrap();
    assert!(
        !output(&repo, &state, &args).status.success(),
        "create cannot recreate missing ownership over existing history"
    );
}

#[test]
fn fresh_tasks_resume_once_bind_accepted_sources_capture_experiments_and_refuse_replayed_authority()
{
    let f = Fixture::bare();
    std::fs::remove_dir(&f.repo).unwrap();
    let task_root = f._root.path().join("task-fixture");
    std::fs::create_dir(&task_root).unwrap();
    let (repo, task_state) = crate::task_cli::fixture_named(&task_root, "pagination");
    std::fs::rename(repo, &f.repo).unwrap();
    let commit = std::process::Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(commit.status.success());
    let commit = String::from_utf8(commit.stdout).unwrap();
    let file = f.machine.join("template.json");
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ticket.json")).unwrap()).unwrap();
    document["verification"] = json!("evaluation");
    write(&file, &document);
    let pinned = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "template-pin",
            "--file",
            file.to_str().unwrap(),
            "--repo",
            f.repo.to_str().unwrap(),
            "--task-state",
            task_state.to_str().unwrap(),
            "--authority-commit",
            commit.trim(),
        ],
    );
    valid("scenario-template-pin-v1.json", &pinned);
    valid("scenario-task-template-v1.json", &pinned["template"]);
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
    policy["task_templates"] = json!([pinned["template_id"]]);
    write(&f.policy, &policy);
    let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    let root = &mut bundle["definitions"]["main"];
    root["authority"]["task_templates"] = json!([pinned["template_id"]]);
    root["steps"] = json!([
        step(
            "first",
            json!({"kind":"fresh_task","template":pinned["template"],"bindings":{}}),
            json!({}),
            "task"
        ),
        step(
            "second",
            json!({"kind":"fresh_task","template":pinned["template"],"bindings":{"source":{"port":"snapshot","artifact_type":"af/SourceTree@1"}}}),
            json!({"source":{"source":"step","step":"first","port":"result"}}),
            "task"
        )
    ]);
    root["limits"]["max_tokens"] = json!(2000);
    let mut proposal: Value = serde_json::from_slice(
        &std::fs::read(workspace().join("fixtures/scenarios/performance-roadmap.json")).unwrap(),
    )
    .unwrap();
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    proposal["horizon"] = json!({"start_unix_ms":now,"end_unix_ms":now+86400000});
    proposal["goals"][0]["loop_tokens"] = json!(2000);
    let roadmap_file = f.machine.join("roadmap.json");
    write(&roadmap_file, &proposal);
    let revision = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "roadmap",
            "validate",
            "--file",
            roadmap_file.to_str().unwrap(),
        ],
    );
    let roadmap = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "roadmap",
            "create",
            "--file",
            roadmap_file.to_str().unwrap(),
            "--confirm-revision",
            revision["revision"].as_str().unwrap(),
        ],
    );
    root["admission"] = json!({"roadmap":"performance","identity":roadmap["identity"],"revision":revision["revision"]});
    write(&f.bundle, &bundle);
    // Editing the authoring file after pinning cannot change an admitted template's bytes.
    document["goal"] = json!("Changed unchecked future text");
    write(&file, &document);
    let refused = f.start("command-roadmap-refused");
    assert_eq!(refused["status"], "failed", "{refused}");
    assert!(refused["failure"].as_str().unwrap().contains("Performance"));
    assert!(refused["fresh_tasks"].as_object().unwrap().is_empty());
    bundle["definitions"]["main"]["admission"] = Value::Null;
    write(&f.bundle, &bundle);
    let done = f.start("fresh-chain");
    assert_eq!(done["status"], "succeeded", "{done}");
    valid("scenario-run-v5.json", &done);
    let roadmap = cli(
        &f.repo,
        &f.state,
        &["scenario", "roadmap", "show", "performance"],
    );
    assert_eq!(
        roadmap["progress"]["store-replay"]["state"], "deferred",
        "{roadmap}"
    );
    assert_eq!(
        roadmap["progress"]["store-replay"]["task_receipts"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(roadmap["progress"]["store-replay"]["reserved_tokens"], 2000);
    let plans = done["fresh_tasks"].as_object().unwrap();
    assert_eq!(plans.len(), 2);
    let first = done["frames"]["root"]["steps"][0]["outputs"]["result"]["value"].clone();
    let second = done["frames"]["root"]["steps"][1]["outputs"]["result"]["value"].clone();
    assert_ne!(first["task_id"], second["task_id"]);
    // The native handoff preserves both original Snapshots and the exact accepted result.
    // It writes only private Git objects and verifies the same capture on restart.
    let source_diff = cli(
        &f.repo,
        &task_state,
        &["task", "diff", first["task_id"].as_str().unwrap()],
    );
    let branch = std::process::Command::new("git")
        .current_dir(&f.repo)
        .args(["symbolic-ref", "HEAD"])
        .output()
        .unwrap();
    assert!(branch.status.success());
    let branch = String::from_utf8(branch.stdout).unwrap();
    let request = json!({"schema":"af/scenario-experiment-source-request@1", "receipt":first,
        "baseline_snapshot_id":source_diff["base_snapshot_id"], "candidate_snapshot_id":source_diff["snapshot_id"],
        "control_sha":commit.trim(), "trusted_ref":branch.trim()});
    valid("scenario-experiment-source-request-v1.json", &request);
    let request_file = f.machine.join("experiment-source.json");
    write(&request_file, &request);
    let capture = f.machine.join("capture");
    let source_args = [
        "scenario",
        "experiment-source",
        "--file",
        request_file.to_str().unwrap(),
        "--repo",
        f.repo.to_str().unwrap(),
        "--task-state",
        task_state.to_str().unwrap(),
        "--into",
        capture.to_str().unwrap(),
    ];
    let captured = cli(&f.repo, &f.state, &source_args);
    valid("scenario-experiment-source-v1.json", &captured);
    assert_eq!(
        captured["baseline_snapshot_id"],
        source_diff["base_snapshot_id"]
    );
    assert_eq!(
        captured["candidate_snapshot_id"],
        source_diff["snapshot_id"]
    );
    assert_ne!(captured["baseline_sha"], captured["candidate_sha"]);
    assert_eq!(captured, cli(&f.repo, &f.state, &source_args));
    let mut stale = request.clone();
    stale["candidate_snapshot_id"] = json!(format!("sha256:{}", "f".repeat(64)));
    write(&request_file, &stale);
    assert!(!output(&f.repo, &f.state, &source_args).status.success());
    write(&request_file, &request);
    std::fs::write(capture.join("source.json"), b"{}").unwrap();
    assert!(!output(&f.repo, &f.state, &source_args).status.success());
    let inspection = cli(
        &f.repo,
        &task_state,
        &["task", "show", second["task_id"].as_str().unwrap()],
    );
    let shown = serde_json::to_string(&inspection).unwrap();
    assert!(
        shown.contains(first["task_id"].as_str().unwrap()),
        "{inspection}"
    );
    for _ in 0..2 {
        let resumed = cli(&f.repo, &f.state, &["scenario", "resume", "fresh-chain"]);
        assert_eq!(resumed, done);
    }
    let listed = cli(&f.repo, &task_state, &["task", "list"]);
    assert_eq!(listed["tasks"].as_array().unwrap().len(), 3, "{listed}");
    let listed = serde_json::to_string(&listed).unwrap();
    assert!(
        !listed.contains("pagination-cli"),
        "Template placeholder must never become a Task: {listed}"
    );
    // Invalid dependent outputs fail before planning another native Task. Reuse the already
    // accepted exact-plan predecessor, so these refusal cases dispatch no new Worker.
    bundle["definitions"]["main"]["admission"] = Value::Null;
    bundle["definitions"]["main"]["steps"][0]["operation"] = json!({"kind":"task","target":{"repo":f.repo,"state":task_state,"task_id":first["task_id"],"plan_id":first["plan_id"],"revision_id":first["revision_id"],"max_tokens":1000}});
    bundle["definitions"]["main"]["authority"]["task_plans"] = json!([first["plan_id"]]);
    policy["task_plans"] = json!([first["plan_id"]]);
    write(&f.policy, &policy);
    for (label, port, kind, diagnostic) in [
        ("missing-output", "absent", "af/SourceTree@1", "missing"),
        ("mistyped-output", "snapshot", "af/Report@1", "wrong type"),
    ] {
        bundle["definitions"]["main"]["steps"][1]["operation"]["bindings"]["source"] =
            json!({"port":port,"artifact_type":kind});
        write(&f.bundle, &bundle);
        let failed = f.start(label);
        assert_eq!(failed["status"], "failed", "{failed}");
        assert!(
            failed["failure"].as_str().unwrap().contains(diagnostic),
            "{failed}"
        );
        assert!(failed["fresh_tasks"].as_object().unwrap().is_empty());
    }
    let listed = cli(&f.repo, &task_state, &["task", "list"]);
    assert_eq!(listed["tasks"].as_array().unwrap().len(), 3, "{listed}");
    // Restore a fresh first step before checking its immutable template grant.
    bundle["definitions"]["main"]["steps"][0]["operation"] =
        json!({"kind":"fresh_task","template":pinned["template"],"bindings":{}});
    // Occupying a stable fresh Task ID with the same file bytes under other authority must
    // not turn native same-file reconciliation into Scenario execution authority.
    bundle["definitions"]["main"]["steps"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    write(&f.bundle, &bundle);
    let pin = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "validate",
            "--file",
            f.bundle.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
        ],
    );
    let held = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "start",
            "occupied-native",
            "--file",
            f.bundle.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
            "--inputs",
            f.inputs.to_str().unwrap(),
            "--confirm-pin",
            pin["revision"].as_str().unwrap(),
        ],
    );
    let identity = serde_json::to_vec(&json!([
        held["execution_id"],
        held["pin"]["revision"],
        "root/first",
        1
    ]))
    .unwrap();
    let fresh_id = format!(
        "scenario-{}",
        review_core::hex::encode(&Sha256::digest(identity))
    );
    let mut occupied = pinned["template"]["document"].clone();
    occupied["task_id"] = json!(fresh_id);
    let occupied_file = f.machine.join("occupied-task.json");
    std::fs::write(&occupied_file, serde_json::to_vec(&occupied).unwrap()).unwrap();
    // A native plan can be committed while its response is lost. Simulate exactly that gap
    // through the native CLI, before the Scenario has recorded its fresh Task/plan mapping.
    let lost = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "start",
            "lost-planning",
            "--file",
            f.bundle.to_str().unwrap(),
            "--authority",
            f.policy.to_str().unwrap(),
            "--inputs",
            f.inputs.to_str().unwrap(),
            "--confirm-pin",
            pin["revision"].as_str().unwrap(),
        ],
    );
    let identity = serde_json::to_vec(&json!([
        lost["execution_id"],
        lost["pin"]["revision"],
        "root/first",
        1
    ]))
    .unwrap();
    let lost_id = format!(
        "scenario-{}",
        review_core::hex::encode(&Sha256::digest(identity))
    );
    let mut lost_document = pinned["template"]["document"].clone();
    lost_document["task_id"] = json!(lost_id);
    let lost_file = f.machine.join("lost-task.json");
    std::fs::write(&lost_file, serde_json::to_vec(&lost_document).unwrap()).unwrap();
    let native_plan = cli(
        &f.repo,
        &task_state,
        &["task", "plan", "--file", lost_file.to_str().unwrap()],
    );
    let recovered = cli(&f.repo, &f.state, &["scenario", "resume", "lost-planning"]);
    assert_eq!(recovered["status"], "succeeded", "{recovered}");
    let target = recovered["fresh_tasks"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(target["task_id"], lost_id);
    assert_eq!(target["plan_id"], native_plan["plan_id"]);
    let listed = cli(&f.repo, &task_state, &["task", "list"]);
    assert_eq!(
        listed["tasks"].as_array().unwrap().len(),
        4,
        "Native planning recovery duplicated work: {listed}"
    );
    let code_policy = f.repo.join(".af/code-policy.toml");
    let policy_text = std::fs::read_to_string(&code_policy)
        .unwrap()
        .replace("check_wall_ms = 5000", "check_wall_ms = 4000");
    std::fs::write(&code_policy, policy_text).unwrap();
    for args in [
        vec!["add", ".af/code-policy.toml"],
        vec!["commit", "-qm", "different captured authority"],
    ] {
        let out = std::process::Command::new("git")
            .current_dir(&f.repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    cli(
        &f.repo,
        &task_state,
        &["task", "plan", "--file", occupied_file.to_str().unwrap()],
    );
    let refused = cli(
        &f.repo,
        &f.state,
        &["scenario", "resume", "occupied-native"],
    );
    assert_eq!(refused["status"], "failed", "{refused}");
    assert!(
        refused["failure"]
            .as_str()
            .unwrap()
            .contains("pinned template authority"),
        "{refused}"
    );
    assert!(refused["fresh_tasks"].as_object().unwrap().is_empty());
    // A changed template requires its own machine grant and pin before it can dispatch.
    bundle["definitions"]["main"]["steps"][0]["operation"]["template"]["document"]["goal"] =
        json!("Widened unchecked text");
    write(&f.bundle, &bundle);
    let error = value(
        output(
            &f.repo,
            &f.state,
            &[
                "scenario",
                "validate",
                "--file",
                f.bundle.to_str().unwrap(),
                "--authority",
                f.policy.to_str().unwrap(),
            ],
        ),
        1,
    );
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("template exceeds"),
        "{error}"
    );
}

#[test]
fn fresh_read_only_review_template_uses_native_acceptance_without_an_implementation_verification_field()
 {
    let f = Fixture::bare();
    std::fs::remove_dir(&f.repo).unwrap();
    let task_root = f._root.path().join("read-only-fixture");
    std::fs::create_dir(&task_root).unwrap();
    let (repo, task_state) = crate::task_cli::fixture_named(&task_root, "review");
    std::fs::rename(repo, &f.repo).unwrap();
    let commit = std::process::Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(commit.status.success());
    let commit = String::from_utf8(commit.stdout).unwrap();
    let pin = cli(
        &f.repo,
        &f.state,
        &[
            "scenario",
            "template-pin",
            "--file",
            f.repo.join("review.json").to_str().unwrap(),
            "--repo",
            f.repo.to_str().unwrap(),
            "--task-state",
            task_state.to_str().unwrap(),
            "--authority-commit",
            commit.trim(),
        ],
    );
    assert!(pin["template"]["document"].get("verification").is_none());
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
    policy["task_templates"] = json!([pin["template_id"]]);
    write(&f.policy, &policy);
    f.install_service();
    write(
        &f.machine.join("observation.json"),
        &json!({"revision":"head-1","bounded_evidence":true}),
    );
    let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    let root = &mut bundle["definitions"]["main"];
    root["authority"]["task_templates"] = json!([pin["template_id"]]);
    let observe =
        json!({"adapter":"service","operation":"observe","destination":"fixture:repository"});
    root["authority"]["grants"] = json!([observe]);
    root["steps"] = json!([
        step(
            "history",
            json!({"kind":"wait","grant":observe,"condition":"bounded_evidence"}),
            json!({"subject":literal("text",json!("selected-history")),"revision":literal("text",json!("head-1"))}),
            "observation"
        ),
        step(
            "review",
            json!({"kind":"fresh_task","template":pin["template"],"bindings":{}}),
            json!({"scenario_evidence":{"source":"step","step":"history","port":"result"}}),
            "task"
        )
    ]);
    write(&f.bundle, &bundle);
    let done = f.start("read-only");
    assert_eq!(done["status"], "succeeded", "{done}");
    let receipt = &done["frames"]["root"]["steps"][1]["outputs"]["result"]["value"];
    assert_eq!(receipt["result"]["acceptance"], "satisfied");
    let cas = review_store::Cas::open_existing(task_state.join("cas")).unwrap();
    let revision = cas
        .get_artifact(receipt["revision_id"].as_str().unwrap())
        .unwrap();
    let requirements_id = revision.payload["inputs"]["requirements"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let requirements = cas.get_artifact(requirements_id).unwrap();
    assert_eq!(
        requirements.payload["specification"]["scenario_evidence"]["condition"],
        "bounded_evidence"
    );
    assert_eq!(
        requirements.payload["specification"]["scenario_evidence"]["evidence"],
        "service revision head-1"
    );
    let plan = cas
        .get_artifact(receipt["plan_id"].as_str().unwrap())
        .unwrap();
    assert!(
        plan.payload["bindings"]
            .as_object()
            .unwrap()
            .keys()
            .all(|slot| !slot.ends_with(".implementer"))
    );
    let status = std::process::Command::new("git")
        .current_dir(&f.repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(
        status.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[path = "scenario_performance.rs"]
mod performance;
#[test]
fn ordinary_scenario_routes_completed_checks_into_one_fresh_repair_and_signed_same_pr() {
    for mode in ["ready", "repairable", "pending", "stale"] {
        let f = Fixture::bare();
        std::fs::remove_dir(&f.repo).unwrap();
        let task_root = f._root.path().join("task-fixture");
        std::fs::create_dir(&task_root).unwrap();
        let (repo, task_state) = crate::task_cli::fixture_named(&task_root, "pagination");
        std::fs::rename(repo, &f.repo).unwrap();
        let commit = std::process::Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(commit.status.success());
        let commit = String::from_utf8(commit.stdout).unwrap();
        let file = f.machine.join("template.json");
        let mut document: Value =
            serde_json::from_slice(&std::fs::read(f.repo.join("ticket.json")).unwrap()).unwrap();
        document["verification"] = json!("evaluation");
        write(&file, &document);
        let pinned = cli(
            &f.repo,
            &f.state,
            &[
                "scenario",
                "template-pin",
                "--file",
                file.to_str().unwrap(),
                "--repo",
                f.repo.to_str().unwrap(),
                "--task-state",
                task_state.to_str().unwrap(),
                "--authority-commit",
                commit.trim(),
            ],
        );
        let adapter = f.machine.join("outcome-service.py");
        std::fs::copy(
            workspace().join("fixtures/scenarios/workspace/outcome-service.py"),
            &adapter,
        )
        .unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(f.machine.join("mode"), mode).unwrap();
        let digest = format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(std::fs::read(&adapter).unwrap()))
        );
        let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
        policy["task_templates"] = json!([pinned["template_id"]]);
        policy["adapters"] = json!({"workspace":{"executable":adapter,"digest":digest,
            "operations":["prepare_candidate_v1","publish_pr","observe"],"destinations":["github:octo/project"],
            "max_wall_ms":30000,"max_output_bytes":65536}});
        write(&f.policy, &policy);
        let binding = |id: &str| json!({"source":"step","step":id,"port":"result"});
        let project = |id: &str, kind: &str| json!({"source":"project","value":binding(id),"projection":{"kind":kind}});
        let grant = |op: &str| json!({"adapter":"workspace","operation":op,"destination":"github:octo/project"});
        let action = |op: &str, signed: bool| json!({"kind":"action","grant":grant(op),"requires_decision":signed,"required_condition":null});
        let prepare = |id: &str, candidate: &str| {
            step(
                id,
                action("prepare_candidate_v1", false),
                json!({"candidate":binding(candidate),"subject":project(candidate,"task_result_id"),
            "revision":{"source":"project","value":binding(candidate),"projection":{"kind":"task_output_snapshot","port":"snapshot"}},
            "body":literal("text",json!("{\"schema\":\"af/git-workspace-content@1\",\"title\":\"Documentation\",\"body\":\"Exact accepted candidate\"}"))}),
                "action",
            )
        };
        let decision = |id: &str, prepared: &str| {
            step(
                id,
                json!({"kind":"decision","owner":"owner"}),
                json!({"subject":project(prepared,"workspace_subject"),"revision":project(prepared,"workspace_revision")}),
                "decision",
            )
        };
        let publish = |id: &str, prepared: &str, approval: &str| {
            step(
                id,
                action("publish_pr", true),
                json!({"subject":project(prepared,"workspace_subject"),"revision":project(prepared,"workspace_revision"),
            "body":project(prepared,"workspace_publication"),"decision":binding(approval)}),
                "action",
            )
        };
        let repair_if = json!({"source":"equals","left":project("checks","checks_state"),"right":literal("text",json!("repairable"))});
        let mut repair = step(
            "repair",
            json!({"kind":"fresh_task","template":pinned["template"],
            "bindings":{"source":{"port":"snapshot","artifact_type":"af/SourceTree@1"}}}),
            json!({"source":binding("edit"),"scenario_evidence":binding("checks"),"publication":binding("prepare")}),
            "task",
        );
        repair["when"] = repair_if.clone();
        let mut repaired = prepare("repaired", "repair");
        repaired["inputs"]["previous"] = binding("prepare");
        repaired["when"] = repair_if.clone();
        let mut approve_repair = decision("approve-repair", "repaired");
        approve_repair["when"] = repair_if.clone();
        let mut publish_repair = publish("publish-repair", "repaired", "approve-repair");
        publish_repair["when"] = repair_if.clone();
        let selected = json!({"source":"select","condition":repair_if,"when_true":binding("repaired"),"when_false":binding("prepare")});
        let bundle = json!({"schema":"af/scenario-bundle@5","root":"documentation-maintenance","definitions":{"documentation-maintenance":{
            "schema":"af/scenario@5","admission":null,"inputs":{},"outputs":{"selected":"action"},
            "steps":[step("edit",json!({"kind":"fresh_task","template":pinned["template"],"bindings":{}}),json!({}),"task"),
                prepare("prepare","edit"),decision("approve","prepare"),publish("publish","prepare","approve"),
                step("checks",json!({"kind":"wait","grant":grant("observe"),"condition":"checks_completed"}),
                    json!({"subject":project("prepare","workspace_subject"),"revision":project("prepare","workspace_revision")}),"observation"),
                repair,repaired,approve_repair,publish_repair],
            "result":{"selected":selected},"completion":[literal("boolean",json!(true))],
            "authority":{"task_plans":[],"task_templates":[pinned["template_id"]],
                "grants":[grant("prepare_candidate_v1"),grant("publish_pr"),grant("observe")],"decision_owners":["owner"]},
            "limits":{"max_tokens":2000,"max_wall_ms":3600000,"max_dispatches":16}}}});
        valid("scenario-bundle-v5.json", &bundle);
        write(&f.bundle, &bundle);
        assert_eq!(f.start("documentation")["status"], "waiting");
        f.approve("documentation");
        let mut run = cli(&f.repo, &f.state, &["scenario", "resume", "documentation"]);
        if mode == "stale" {
            assert_eq!(run["status"], "failed", "{run}");
            assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), 1);
        } else if mode == "pending" {
            assert_eq!(run["status"], "waiting", "{run}");
            assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), 1);
        } else {
            if mode == "repairable" {
                assert_eq!(run["status"], "waiting", "{run}");
                assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), 2);
                f.approve("documentation");
                run = cli(&f.repo, &f.state, &["scenario", "resume", "documentation"]);
            }
            assert_eq!(run["status"], "succeeded", "{run}");
            let prepared: Value = serde_json::from_str(
                run["frames"]["root"]["outputs"]["selected"]["value"]["evidence"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                prepared["commit"],
                if mode == "ready" {
                    "b".repeat(40)
                } else {
                    "c".repeat(40)
                }
            );
        }
        let effects: Value =
            serde_json::from_slice(&std::fs::read(f.machine.join("effects.json")).unwrap())
                .unwrap();
        assert_eq!(effects["posts"], 1);
        assert_eq!(
            effects["publications"].as_array().unwrap().len(),
            if mode == "repairable" { 2 } else { 1 }
        );
    }
}

#[test]
fn generated_impl_scenario_runs_zero_one_two_repairs_then_actual_merge_and_owned_cleanup() {
    for (repairs, failures) in [(0, 0), (1, 1), (2, 2), (2, 3)] {
        let mode = format!("repairs-{failures}");
        let f = Fixture::bare();
        std::fs::remove_dir(&f.repo).unwrap();
        let task_root = f._root.path().join("task-fixture");
        std::fs::create_dir(&task_root).unwrap();
        let (repo, task_state) = crate::task_cli::fixture_named(&task_root, "continue");
        std::fs::rename(repo, &f.repo).unwrap();
        let af = crate::common::af();
        let generated = std::process::Command::new("/usr/bin/python3")
            .args(["-B", "-I"])
            .arg(workspace().join("fixtures/scenarios/impl/generate.py"))
            .arg(workspace())
            .arg(&f.repo)
            .arg(af.get_program())
            .args(["consumer", "command"])
            .envs(af.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))))
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        std::fs::write(f.repo.join("NOTES.md"), "paginate documentation").unwrap();
        for args in [
            ["add", "-A"].as_slice(),
            ["commit", "-qm", "generated consuming packages"].as_slice(),
        ] {
            assert!(
                std::process::Command::new("git")
                    .current_dir(&f.repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let commit = std::process::Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(commit.status.success());
        let commit = String::from_utf8(commit.stdout).unwrap();
        let file = f.machine.join("template.json");
        let mut document: Value =
            serde_json::from_slice(&std::fs::read(f.repo.join("first.json")).unwrap()).unwrap();
        document["pipeline"]["name"] = json!("consumer/implementation");
        document["facts"]["standard"] = json!(true);
        document["limits"]["max_attempts"] = json!(16);
        document["limits"]["verification"]["attempts"] = json!(10);
        document["limits"]["verification"]["wall_ms"] = json!(35000);
        write(&file, &document);
        let pinned = cli(
            &f.repo,
            &f.state,
            &[
                "scenario",
                "template-pin",
                "--file",
                file.to_str().unwrap(),
                "--repo",
                f.repo.to_str().unwrap(),
                "--task-state",
                task_state.to_str().unwrap(),
                "--authority-commit",
                commit.trim(),
            ],
        );
        let adapter = f.machine.join("outcome-service.py");
        std::fs::copy(
            workspace().join("fixtures/scenarios/workspace/outcome-service.py"),
            &adapter,
        )
        .unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(f.machine.join("mode"), &mode).unwrap();
        let digest = format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(std::fs::read(&adapter).unwrap()))
        );
        let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
        policy["task_templates"] = json!([pinned["template_id"]]);
        policy["adapters"] = json!({"workspace":{"executable":adapter,"digest":digest,
            "operations":["prepare_candidate_v1","publish_pr","observe","cleanup_workspace_v1"],"destinations":["github:octo/project"],
            "max_wall_ms":30000,"max_output_bytes":65536}});
        write(&f.policy, &policy);
        write(&f.machine.join("pin.json"), &pinned);
        std::fs::write(f.machine.join("body.md"), "Exact accepted candidate").unwrap();
        std::fs::write(f.machine.join("human-state"), "open").unwrap();
        let generated = std::process::Command::new("/usr/bin/python3")
            .args(["-B", "-I"])
            .arg(workspace().join("scripts/scenario-impl.py"))
            .args(["bundle", "--pin"])
            .arg(f.machine.join("pin.json"))
            .args([
                "--destination",
                "github:octo/project",
                "--title",
                "Documentation",
                "--body-file",
            ])
            .arg(f.machine.join("body.md"))
            .args(["--repairs", &repairs.to_string(), "--output"])
            .arg(f.bundle.with_extension("generated.json"))
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let bundle: Value = serde_json::from_slice(
            &std::fs::read(f.bundle.with_extension("generated.json")).unwrap(),
        )
        .unwrap();
        valid("scenario-bundle-v5.json", &bundle);
        let mut tampered = bundle.clone();
        tampered["definitions"]["impl"]["steps"][0]["operation"]["template"]["document"]["goal"] =
            json!("unapproved goal");
        write(&f.bundle, &tampered);
        let refused = value(
            output(
                &f.repo,
                &f.state,
                &[
                    "scenario",
                    "validate",
                    "--file",
                    f.bundle.to_str().unwrap(),
                    "--authority",
                    f.policy.to_str().unwrap(),
                ],
            ),
            1,
        );
        assert!(
            refused["error"].as_str().unwrap().contains("template"),
            "{refused}"
        );
        write(&f.bundle, &bundle);
        let started = f.start("impl-run");
        assert_eq!(started["status"], "waiting", "{started}");
        let mut run = Value::Null;
        for _ in 0..=repairs {
            f.approve("impl-run");
            run = cli(&f.repo, &f.state, &["scenario", "resume", "impl-run"]);
            assert_eq!(run["status"], "waiting", "{run}");
        }
        assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), repairs + 1);
        let effects = || -> Value {
            serde_json::from_slice(&std::fs::read(f.machine.join("effects.json")).unwrap()).unwrap()
        };
        assert_eq!(effects()["posts"], 1);
        assert_eq!(
            effects()["publications"].as_array().unwrap().len(),
            repairs + 1
        );
        assert!(effects().get("cleaned").is_none());
        if failures > repairs {
            // The last failed head cannot dispatch a third repair or reach merge/cleanup.
            let capped = cli(&f.repo, &f.state, &["scenario", "resume", "impl-run"]);
            assert_eq!(capped["status"], "waiting", "{capped}");
            assert_eq!(
                capped["fresh_tasks"].as_object().unwrap().len(),
                repairs + 1
            );
            assert!(effects().get("cleaned").is_none());
            continue;
        }
        // Closed without a merge still cannot release owned objects.
        std::fs::write(f.machine.join("human-state"), "closed-unmerged").unwrap();
        assert_eq!(
            cli(&f.repo, &f.state, &["scenario", "resume", "impl-run"])["status"],
            "waiting"
        );
        assert!(effects().get("cleaned").is_none());
        std::fs::write(f.machine.join("human-state"), "merged").unwrap();
        let done = cli(&f.repo, &f.state, &["scenario", "resume", "impl-run"]);
        assert_eq!(done["status"], "succeeded", "{done}");
        assert_eq!(
            effects()["cleaned"],
            (char::from(b'b' + repairs as u8).to_string()).repeat(40)
        );
        // Every repair has native captured completed-CI evidence and the predecessor source.
        let cas = review_store::Cas::open_existing(task_state.join("cas")).unwrap();
        let mut evidence_count = 0;
        for task in done["fresh_tasks"].as_object().unwrap().values() {
            let revision = cas
                .get_artifact(task["revision_id"].as_str().unwrap())
                .unwrap();
            let req = cas
                .get_artifact(
                    revision.payload["inputs"]["requirements"]["artifact_ids"][0]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
            if let Some(evidence) = req.payload["specification"].get("scenario_evidence") {
                assert_eq!(evidence["condition"], "checks_completed");
                let packet: Value =
                    serde_json::from_str(evidence["evidence"].as_str().unwrap()).unwrap();
                assert_eq!(packet["state"], "repairable");
                evidence_count += 1;
                let receipt = done["frames"]["root"]["steps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|step| &step["outputs"]["result"]["value"])
                    .find(|receipt| receipt["task_id"] == task["task_id"])
                    .unwrap();
                let source_id = receipt["result"]["outputs"]["snapshot"]["artifact_ids"][0]
                    .as_str()
                    .unwrap();
                let source = cas.get_artifact(source_id).unwrap();
                let (_, manifest) = review_source_git::task::read_snapshot(
                    &cas,
                    source.payload["snapshot_id"].as_str().unwrap(),
                )
                .unwrap();
                let seen: Value = serde_json::from_slice(
                    &cas.get(&manifest.get("seen-ci.json").unwrap().content)
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    seen, packet,
                    "the repair implementer read exact admitted CI evidence"
                );
            }
        }
        assert_eq!(evidence_count, repairs);
    }
}
