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
            &json!({"schema":"af/scenario-bundle@6","root":"main","definitions":{"main":{"schema":"af/scenario@6","inputs":{},"outputs":{"ok":"boolean"},"steps":steps,"result":{"ok":literal("boolean",json!(true))},"completion":[literal("boolean",json!(true))],"authority":{"task_plans":[],"task_templates":[],"grants":[g],"decision_owners":["owner"]},"limits":{"max_tokens":2000,"max_wall_ms":604800000,"max_dispatches":64}}}}),
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
        valid("scenario-pin-v6.json", &pinned);
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
            af_contracts::hex::encode(&Sha256::digest(std::fs::read(&path).unwrap()))
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
    let bundle = json!({"schema":"af/scenario-bundle@6","root":"performance","definitions":{"performance":{
        "schema":"af/scenario@6","admission":null,"inputs":{},"outputs":{"candidate":"task","report":"observation"},
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
    valid("scenario-bundle-v6.json", &bundle);
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
    valid("scenario-run-v6.json", &waiting);
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
    valid("scenario-run-v6.json", &done);
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
    valid("scenario-run-v6.json", &cancelled);
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
        &json!({"schema":"af/ant@7","id":"daily","scenario":bundle,"inputs":{},"schedule":{"kind":"daily","timezone":"UTC","hour":12,"minute":0,"start_unix_ms":now},"owner":"owner","notification":"local:owner","execution":{"overlap":"skip","missed":"skip","lateness_ms":1000,"per_run":bundle["definitions"]["main"]["limits"],"rolling_tokens":2000,"rolling_window_ms":172800000}}),
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
        valid("ant-tick-v7.json", &tick);
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
    valid("ant-inspection-v3.json", &paused);
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
        af_contracts::hex::encode(&Sha256::digest(b"active"))
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
    valid("scenario-pin-v6.json", &pin);
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
            af_contracts::hex::encode(&Sha256::digest(&production))
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
            af_contracts::hex::encode(&Sha256::digest(&bytes))
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
    valid("scenario-run-v6.json", &done);
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
    valid("scenario-roadmap-v3.json", &first);
    assert_eq!(first["progress"]["store-replay"]["state"], "pending");
    assert_eq!(first["progress"]["store-replay"]["actual_tokens"], "0");
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
    valid("scenario-roadmap-list-v3.json", &list);
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
    valid("scenario-template-pin-v2.json", &pinned);
    valid("scenario-task-template-v2.json", &pinned["template"]);
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
    valid("scenario-run-v6.json", &done);
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
        af_contracts::hex::encode(&Sha256::digest(identity))
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
        af_contracts::hex::encode(&Sha256::digest(identity))
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
    let cas = af_store::Cas::open_existing(task_state.join("cas")).unwrap();
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
            af_contracts::hex::encode(&Sha256::digest(std::fs::read(&adapter).unwrap()))
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
        let bundle = json!({"schema":"af/scenario-bundle@6","root":"documentation-maintenance","definitions":{"documentation-maintenance":{
            "schema":"af/scenario@6","admission":null,"inputs":{},"outputs":{"selected":"action"},
            "steps":[step("edit",json!({"kind":"fresh_task","template":pinned["template"],"bindings":{}}),json!({}),"task"),
                prepare("prepare","edit"),decision("approve","prepare"),publish("publish","prepare","approve"),
                step("checks",json!({"kind":"wait","grant":grant("observe"),"condition":"checks_completed"}),
                    json!({"subject":project("prepare","workspace_subject"),"revision":project("prepare","workspace_revision")}),"observation"),
                repair,repaired,approve_repair,publish_repair],
            "result":{"selected":selected},"completion":[literal("boolean",json!(true))],
            "authority":{"task_plans":[],"task_templates":[pinned["template_id"]],
                "grants":[grant("prepare_candidate_v1"),grant("publish_pr"),grant("observe")],"decision_owners":["owner"]},
            "limits":{"max_tokens":2000,"max_wall_ms":3600000,"max_dispatches":16}}}});
        valid("scenario-bundle-v6.json", &bundle);
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
            af_contracts::hex::encode(&Sha256::digest(std::fs::read(&adapter).unwrap()))
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
        valid("scenario-bundle-v6.json", &bundle);
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
        let cas = af_store::Cas::open_existing(task_state.join("cas")).unwrap();
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
                let (_, manifest) = af_source_git::task::read_snapshot(
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

#[test]
fn interval_scheduler_executes_real_one_second_occurrences_and_generic_results() {
    let f = Fixture::bare();
    for label in ["weather", "library"] {
        let mut bundle: Value = serde_json::from_slice(
            &std::fs::read(workspace().join(format!("fixtures/scenarios/results/{label}.json")))
                .unwrap(),
        )
        .unwrap();
        let grant = grant("write_owned_file", &f.owned);
        bundle["definitions"][label]["authority"]["grants"] = json!([grant]);
        bundle["definitions"][label]["steps"][0]["operation"]["grant"] = grant;
        write(&f.bundle, &bundle);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let ant = f._root.path().join(format!("{label}-ant.json"));
        write(
            &ant,
            &json!({"schema":"af/ant@7","id":label,"scenario":bundle,"inputs":{},"schedule":{"kind":"interval","seconds":1,"start_unix_ms":now},"owner":"owner","notification":"local:fixture","execution":{"overlap":"skip","missed":"skip","lateness_ms":999,"per_run":bundle["definitions"][label]["limits"],"rolling_tokens":0,"rolling_window_ms":10000}}),
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
        valid("ant-pin-v7.json", &pin);
        cli(
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
            ],
        );
        cli(&f.repo, &f.state, &["ant", "enable", label]);
    }
    let scheduler = crate::common::af()
        .current_dir(&f.repo)
        .args(["ant", "scheduler", "run", "--for-seconds", "4", "--state"])
        .arg(&f.state)
        .output()
        .unwrap();
    assert!(
        scheduler.status.success(),
        "{}",
        String::from_utf8_lossy(&scheduler.stderr)
    );
    for label in ["weather", "library"] {
        let shown = cli(&f.repo, &f.state, &["ant", "show", label]);
        valid("ant-inspection-v3.json", &shown);
        let occurrences = shown["occurrences"].as_array().unwrap();
        let started: Vec<_> = occurrences
            .iter()
            .filter(|o| o["outcome"]["kind"] == "started")
            .collect();
        assert!(started.len() >= 3, "{shown}");
        let unique: std::collections::BTreeSet<_> =
            started.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(unique.len(), started.len());
        for pair in started.windows(2) {
            assert!(
                pair[1]["due_unix_ms"].as_u64().unwrap()
                    >= pair[0]["due_unix_ms"].as_u64().unwrap() + 1000
            );
        }
        for occurrence in started {
            assert!(
                occurrence["admitted_unix_ms"].as_u64().unwrap()
                    >= occurrence["due_unix_ms"].as_u64().unwrap()
            );
            let run = cli(
                &f.repo,
                &f.state,
                &[
                    "scenario",
                    "show",
                    occurrence["outcome"]["run_id"].as_str().unwrap(),
                ],
            );
            assert_eq!(run["status"], "succeeded");
            assert_eq!(run["dispatches"], 0);
        }
        let doc = crate::common::af()
            .current_dir(&f.repo)
            .args(["ant", "output", label, "--name", "report", "--state"])
            .arg(&f.state)
            .output()
            .unwrap();
        assert!(
            doc.status.success(),
            "{}",
            String::from_utf8_lossy(&doc.stderr)
        );
        let text = String::from_utf8(doc.stdout).unwrap();
        assert!(text.contains("State: final"));
        assert!(text.contains("freshness: stale"));
        assert!(!text.contains('\u{1b}'));
        assert!(text.contains(if label == "weather" {
            "| Station | Temperature C | Condition |"
        } else {
            "| Title | Copies | Tags |"
        }));
        let empty = crate::common::af()
            .current_dir(&f.repo)
            .args(["ant", "output", label, "--name", "empty", "--state"])
            .arg(&f.state)
            .output()
            .unwrap();
        assert!(empty.status.success());
        assert!(
            String::from_utf8(empty.stdout)
                .unwrap()
                .contains("final_empty")
        );
    }
    let status = cli(&f.repo, &f.state, &["ant", "scheduler", "status"]);
    assert_eq!(status["running"], false);
    valid("ant-scheduler-v1.json", &status);
    assert!(f.owned.read_dir().unwrap().next().is_none());
}

// Keep every fixture observer/driver owned even when an assertion unwinds.
struct ResultProcess(Option<std::process::Child>);
impl ResultProcess {
    fn id(&self) -> u32 {
        self.0.as_ref().unwrap().id()
    }
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let result = self.0.as_mut().unwrap().wait();
        if result.is_ok() {
            self.0.take();
        }
        result
    }
}
impl Drop for ResultProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn result_file_until(path: &Path, needle: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Ok(document) = std::fs::read_to_string(path) {
            assert!(document.starts_with("<!-- af-result-snapshot begin"));
            assert!(document.ends_with("-->\n"));
            assert!(!document.contains('\u{1b}'));
            if document.contains(needle) {
                return document;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("watch never published {needle}");
}

#[test]
fn markdown_watch_interrupt_is_read_only_atomic_and_retains_active_run() {
    let f = Fixture::bare();
    let started = f.start("watch-wait");
    assert_eq!(started["status"], "waiting");
    let file = f._root.path().join("latest.md");
    let mut watch = ResultProcess(Some(
        crate::common::af()
            .current_dir(&f.repo)
            .args([
                "scenario",
                "output",
                "watch-wait",
                "--name",
                "ok",
                "--watch",
                "--interval-seconds",
                "1",
                "--output",
            ])
            .arg(&file)
            .arg("--state")
            .arg(&f.state)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    result_file_until(&file, "unavailable");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(watch.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let status = watch.wait().unwrap();
    assert!(!status.success());
    let mut driver = ResultProcess(Some(
        crate::common::af()
            .current_dir(&f.repo)
            .args(["ant", "scheduler", "run", "--state"])
            .arg(&f.state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while cli(&f.repo, &f.state, &["ant", "scheduler", "status"])["running"] != true {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let duplicate = output(
        &f.repo,
        &f.state,
        &["ant", "scheduler", "run", "--for-seconds", "1"],
    );
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("Scheduler already running"));
    cli(&f.repo, &f.state, &["ant", "scheduler", "stop"]);
    assert!(driver.wait().unwrap().success());
    assert_eq!(
        cli(&f.repo, &f.state, &["ant", "scheduler", "status"])["running"],
        false
    );
    let after = cli(&f.repo, &f.state, &["scenario", "show", "watch-wait"]);
    assert_eq!(
        after, started,
        "watch must not mutate Run or clear human hold"
    );
}

#[test]
fn fleet_result_example_projects_native_goal_state_clock_and_charged_usage() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = crate::task_cli::fixture_named(root.path(), "pagination");
    let plan = cli(&repo, &state, &["task", "plan", "--file", "ticket.json"]);
    let mut command = std::process::Command::new("python3");
    command
        .args(["-I"])
        .arg(workspace().join("fixtures/scenarios/results/fleet-document.py"))
        .args(["--af"])
        .arg(crate::common::af_binary())
        .args(["--environment", "private-fixture", "--repo"])
        .arg(&repo)
        .arg("--task-state")
        .arg(&state)
        .args(["--task", "pagination-cli"]);
    command.envs(crate::common::private_home_env());
    command.env("AF_SELF_OFFLINE", "1");
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    valid("result-document-v1.json", &document);
    let row = &document["blocks"][1]["rows"][0];
    assert_eq!(row[0], "private-fixture");
    assert_eq!(row[1], json!(state.canonicalize().unwrap()));
    assert_eq!(row[2], "pagination-cli");
    assert!(!row[3].as_str().unwrap().is_empty());
    assert_eq!(row[4]["phase"], "planned");
    assert_eq!(row[6], "0");
    assert_eq!(row[7], 0);
    assert_eq!(row[8]["plan"], plan["plan_id"]);
    let typed: af_scenario::presentation::Document = serde_json::from_value(document).unwrap();
    typed.validate().unwrap();
}

#[test]
fn recorded_observation_results_are_partial_then_final_and_new_occurrences_change_revision() {
    let f = Fixture::bare();
    f.install_service();
    let observe =
        json!({"adapter":"service","operation":"observe","destination":"fixture:repository"});
    let result = json!({"source":"step","step":"sample","port":"result"});
    let inputs = json!({"subject":literal("text",json!("sensor")),"revision":literal("text",json!("head-1"))});
    let bundle = json!({"schema":"af/scenario-bundle@6","root":"sensor","definitions":{"sensor":{
        "schema":"af/scenario@6","inputs":{},"outputs":{"report":"observation"},
        "steps":[step("sample",json!({"kind":"wait","grant":observe,"condition":"sampled"}),inputs.clone(),"observation"),step("finish",json!({"kind":"wait","grant":observe,"condition":"finished"}),inputs,"observation")],
        "result":{"report":result},"completion":[literal("boolean",json!(true))],
        "authority":{"task_plans":[],"task_templates":[],"grants":[observe],"decision_owners":[]},
        "limits":{"max_tokens":0,"max_wall_ms":300000,"max_dispatches":8}}}});
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let document = |value: u64| json!({"schema":"af/result-document@1","observed_unix_ms":now,"stale_after_ms":10000,"blocks":[{"kind":"table","title":"Sensor","columns":["Count"],"rows":[[value]]}]});
    let observation = |value: u64, finished: bool| json!({"revision":"head-1","sampled":true,"finished":finished,"evidence":document(value).to_string()});
    write(&f.machine.join("observation.json"), &observation(1, false));
    let ant = f._root.path().join("sensor-ant.json");
    write(
        &ant,
        &json!({"schema":"af/ant@7","id":"sensor","scenario":bundle,"inputs":{},"schedule":{"kind":"interval","seconds":1,"start_unix_ms":now},"owner":"owner","notification":"local:fixture","execution":{"overlap":"skip","missed":"skip","lateness_ms":999,"per_run":bundle["definitions"]["sensor"]["limits"],"rolling_tokens":0,"rolling_window_ms":10000}}),
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
    cli(
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
    cli(
        &f.repo,
        &f.state,
        &["ant", "enable", "sensor", "--now", &now.to_string()],
    );
    cli(
        &f.repo,
        &f.state,
        &["ant", "tick", "--now", &now.to_string()],
    );
    let read = || {
        let output = crate::common::af()
            .current_dir(&f.repo)
            .args(["ant", "output", "sensor", "--state"])
            .arg(&f.state)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let partial = read();
    assert!(partial.contains("State: partial"));
    assert!(partial.contains("| 1 |"));
    let file = f._root.path().join("sensor.md");
    let mut watch = ResultProcess(Some(
        crate::common::af()
            .current_dir(&f.repo)
            .args(["ant", "output", "sensor", "--watch", "--output"])
            .arg(&file)
            .arg("--state")
            .arg(&f.state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    result_file_until(&file, "| 1 |");

    write(&f.machine.join("observation.json"), &observation(1, true));
    cli(
        &f.repo,
        &f.state,
        &["ant", "tick", "--now", &(now + 1).to_string()],
    );
    let final_view = read();
    assert!(final_view.contains("State: final"));
    assert!(final_view.contains("| 1 |"));
    write(&f.machine.join("observation.json"), &observation(2, false));
    cli(
        &f.repo,
        &f.state,
        &["ant", "tick", "--now", &(now + 1000).to_string()],
    );
    let changed = read();
    assert!(changed.contains("State: partial"));
    assert!(changed.contains("| 2 |"));
    let revision = |s: &str| {
        s.lines()
            .find(|s| s.starts_with("Revision:"))
            .unwrap()
            .to_string()
    };
    assert_ne!(revision(&changed), revision(&partial));
    let refreshed = result_file_until(&file, "| 2 |");
    assert_eq!(revision(&refreshed), revision(&changed));
    // A damaged/missing derived view is a refresh failure, never permission to display the
    // previous document as current. The authoritative Run remains untouched.
    let native_before = cli(&f.repo, &f.state, &["ant", "show", "sensor"]);
    let db = rusqlite::Connection::open(f.state.join("runs/scenarios.sqlite")).unwrap();
    db.execute(
        "DELETE FROM result_views WHERE kind='ant' AND label='sensor'",
        [],
    )
    .unwrap();
    drop(db);
    let failed = result_file_until(&file, "Error:");
    assert!(failed.contains("unavailable"));
    assert!(!failed.contains("| 2 |"));
    assert!(failed.contains("Ant: sensor"));
    assert!(failed.contains("# Output report"));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(watch.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    assert!(!watch.wait().unwrap().success());
    assert_eq!(
        cli(&f.repo, &f.state, &["ant", "show", "sensor"]),
        native_before
    );
}

#[test]
fn scheduler_fatal_journal_failure_stops_and_keeps_run_and_diagnostic() {
    let f = Fixture::bare();
    let before = f.start("journal-wait");
    assert_eq!(before["status"], "waiting");
    let db = rusqlite::Connection::open(f.state.join("runs/scenarios.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER fixture_refuse_journal BEFORE UPDATE ON journal BEGIN SELECT RAISE(ABORT,'fixture journal refusal'); END;").unwrap();
    drop(db);
    let mut driver = ResultProcess(Some(
        crate::common::af()
            .current_dir(&f.repo)
            .args(["ant", "scheduler", "run", "--state"])
            .arg(&f.state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = driver.0.as_mut().unwrap().try_wait().unwrap() {
            driver.0.take();
            assert_eq!(status.code(), Some(1));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fatal driver error kept looping"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let status = cli(&f.repo, &f.state, &["ant", "scheduler", "status"]);
    assert_eq!(status["running"], false);
    assert!(
        status["error"]
            .as_str()
            .unwrap()
            .contains("fixture journal refusal")
    );
    assert_eq!(
        cli(&f.repo, &f.state, &["scenario", "show", "journal-wait"]),
        before
    );
}

#[test]
fn native_authored_markdown_watch_uses_recorded_observation_and_never_drives_work() {
    let f = Fixture::bare();
    let mut bundle: Value = serde_json::from_slice(
        &std::fs::read(workspace().join("fixtures/scenarios/results/authored-markdown.json"))
            .unwrap(),
    )
    .unwrap();
    let g = grant("write_owned_file", &f.owned);
    let definition = &mut bundle["definitions"]["library"];
    definition["authority"]["grants"] = json!([g]);
    definition["steps"][0]["operation"]["grant"] = g;
    // Keep a genuine native Run waiting for a human while observing literal recorded
    // outputs. The watcher cannot clear this hold or dispatch the later file Action.
    let baseline: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    definition["steps"] = baseline["definitions"]["main"]["steps"].clone();
    definition["limits"] = baseline["definitions"]["main"]["limits"].clone();
    definition["authority"]["decision_owners"] = json!(["owner"]);
    let observed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    definition["result"]["observed"]["value"]["value"] = json!(observed);
    write(&f.bundle, &bundle);
    let started = f.start("authored");
    assert_eq!(started["status"], "waiting");
    let raw = std::fs::read_to_string(workspace().join("fixtures/scenarios/results/authored.md"))
        .unwrap();
    assert_eq!(
        started["pin"]["bundle"]["definitions"]["library"]["result"]["report"]["value"]["value"],
        raw
    );
    let file = f._root.path().join("authored.md");
    let mut watch = ResultProcess(Some(
        crate::common::af()
            .current_dir(&f.repo)
            .args([
                "scenario",
                "output",
                "authored",
                "--name",
                "report",
                "--raw-markdown",
                "--observed-output",
                "observed",
                "--stale-after-ms",
                "1",
                "--watch",
                "--interval-seconds",
                "1",
                "--output",
            ])
            .arg(&file)
            .arg("--state")
            .arg(&f.state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let initial = result_file_until(&file, "| Rust | 2 |");
    assert!(initial.contains(&raw));
    assert!(initial.contains("State: partial"));
    assert!(initial.contains("freshness: stale"));
    assert!(initial.contains(&format!("Observed Unix ms: {observed};")));
    // A later timestamp proves an actual poll occurred; it must not recollect evidence.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let refreshed = result_file_until(&file, "| Rust | 2 |");
        if refreshed != initial {
            assert!(refreshed.contains(&raw));
            assert!(refreshed.contains(&format!("Observed Unix ms: {observed};")));
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(watch.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    assert!(!watch.wait().unwrap().success());
    assert_eq!(
        cli(&f.repo, &f.state, &["scenario", "show", "authored"]),
        started
    );
    assert!(f.owned.read_dir().unwrap().next().is_none());
    // Without explicit selection it remains a structured-document error, never a
    // format guess. An absent observation port likewise fails without invented time.
    let unsupported = crate::common::af()
        .current_dir(&f.repo)
        .args([
            "scenario", "output", "authored", "--name", "report", "--state",
        ])
        .arg(&f.state)
        .output()
        .unwrap();
    assert!(!unsupported.status.success());
    assert!(
        String::from_utf8(unsupported.stdout)
            .unwrap()
            .contains("Unsupported presentation shape")
    );
}

#[test]
fn template_local_workers_are_captured_once_retained_and_restored_after_deletion() {
    use af_pipeline_config::task::catalog::TaskWorkerManifest;
    let f = Fixture::bare();
    std::fs::remove_dir(&f.repo).unwrap();
    let root = f._root.path().join("task-fixture");
    std::fs::create_dir(&root).unwrap();
    let (repo, task_state) = crate::task_cli::fixture_named(&root, "pagination");
    std::fs::rename(repo, &f.repo).unwrap();
    let commit = std::process::Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(commit.status.success());
    let commit = String::from_utf8(commit.stdout).unwrap();
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ticket.json")).unwrap()).unwrap();
    document["verification"] = json!("evaluation");
    let file = f.machine.join("template.json");
    write(&file, &document);
    // Initialize the shared Store with a different template. The concurrently created
    // local-template streams below are still empty; schema bootstrap is a separate boundary.
    cli(
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
    let mut pins = Vec::new();
    let mut workers = Vec::new();
    let mut unrelated_authority = None;
    for developer in ["alice", "bob"] {
        let local = f.machine.join(developer);
        let package = local.join("worker");
        crate::task_cli::copy_tree(
            &f.repo.join(".af/task-packages/fixture/implementer"),
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
        let definition = json!({"schema":"af.task-bindings/1", "packages":{worker.name.clone():{"version":"1.0.0","digest":digest,"path":"worker"}},"slots":{"root.slots.implementer":worker.name}});
        std::fs::write(&bindings, toml::to_string(&definition).unwrap()).unwrap();
        let args = [
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
            "--bindings",
            bindings.to_str().unwrap(),
        ];
        // Independent CLI processes start together, then all acknowledge one durable pin.
        let barrier = std::sync::Barrier::new(4);
        let pinned = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        cli(&f.repo, &f.state, &args)
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            assert!(results.iter().all(|r| r == &results[0]));
            results[0].clone()
        });
        let store = af_store::EventStore::open_read_only(task_state.join("events.sqlite")).unwrap();
        let run = format!(
            "scenario-template-{}",
            pinned["template_id"]
                .as_str()
                .unwrap()
                .trim_start_matches("sha256:")
        );
        assert_eq!(store.replay(&run).unwrap().len(), 1);
        valid("scenario-template-pin-v2.json", &pinned);
        assert_eq!(pinned, cli(&f.repo, &f.state, &args));
        for (field, bad) in [
            ("path", json!("../worker")),
            ("digest", json!(format!("sha256:{}", "0".repeat(64)))),
            ("version", json!("9.9.9")),
        ] {
            let mut wrong = definition.clone();
            wrong["packages"][&worker.name][field] = bad;
            std::fs::write(&bindings, toml::to_string(&wrong).unwrap()).unwrap();
            assert!(
                !output(&f.repo, &f.state, &args).status.success(),
                "{field}"
            );
        }
        let mut shadow = definition.clone();
        shadow["packages"] = json!({"fixture/implementer":definition["packages"][&worker.name]});
        std::fs::write(&bindings, toml::to_string(&shadow).unwrap()).unwrap();
        assert!(!output(&f.repo, &f.state, &args).status.success());
        std::fs::write(&bindings, toml::to_string(&definition).unwrap()).unwrap();
        let oversized = package.join("oversized");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len(16 * 1024 * 1024 + 1)
            .unwrap();
        assert!(!output(&f.repo, &f.state, &args).status.success());
        std::fs::remove_file(&oversized).unwrap();
        if developer == "alice" {
            let pipeline = local.join("pipeline");
            crate::task_cli::copy_tree(
                &f.repo.join(".af/task-packages/fixture/implementation"),
                &pipeline,
            );
            let manifest_path = pipeline.join("pipeline.toml");
            let mut manifest: toml::Value =
                toml::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
            manifest["name"] = toml::Value::String("local/pipeline".into());
            std::fs::write(&manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
            let digest =
                af_pipeline_config::lock::package_digest("local/pipeline", &pipeline).unwrap();
            let mut wrong = definition.clone();
            wrong["packages"]["local/pipeline"] = json!({"path":"pipeline","version":manifest["version"].as_str().unwrap(),"digest":digest});
            std::fs::write(&bindings, toml::to_string(&wrong).unwrap()).unwrap();
            let refused = output(&f.repo, &f.state, &args);
            assert!(!refused.status.success());
            assert!(
                String::from_utf8_lossy(&refused.stdout).contains("Worker packages only"),
                "{}",
                String::from_utf8_lossy(&refused.stdout)
            );
        }
        std::fs::write(&bindings, toml::to_string(&definition).unwrap()).unwrap();
        let moved = local.join("moved");
        std::fs::rename(&package, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &package).unwrap();
        assert!(!output(&f.repo, &f.state, &args).status.success());
        std::fs::remove_file(&package).unwrap();
        // No fresh Task has been planned: retention must start at pinning, not dispatch.
        cli(
            &f.repo,
            &task_state,
            &["task", "gc", "--older-than", "0", "--keep", "0", "--apply"],
        );
        let cas = af_store::Cas::open_existing(task_state.join("cas")).unwrap();
        let authority = cas
            .get_json(pinned["template"]["authority_id"].as_str().unwrap())
            .unwrap();
        assert!(
            authority["packages"]["fixture/implementer"].is_object(),
            "constraining default retained"
        );
        assert_eq!(
            authority["slot_workers"]["root.slots.implementer"],
            worker.name
        );
        let captured = authority["packages"][&worker.name]["artifact_id"]
            .as_str()
            .unwrap()
            .to_owned();
        cas.get_json(&captured).unwrap();
        if developer == "alice" {
            std::fs::rename(&moved, &package).unwrap();
            // Seed an unrooted closure with old mtimes, then repin using exactly those CAS
            // objects. Timestamp protection alone must not make this test pass.
            let db = rusqlite::Connection::open(task_state.join("events.sqlite")).unwrap();
            db.execute("DELETE FROM events WHERE run_id=?1", [&run])
                .unwrap();
            let digest = captured.strip_prefix("sha256:").unwrap();
            let object = task_state
                .join("cas/objects")
                .join(&digest[..2])
                .join(&digest[2..]);
            let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
            std::fs::File::open(&object)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
            let barriers = f.machine.join("pin-barriers");
            for phase in ["template-captured-1", "template-verifying-1"] {
                std::fs::create_dir_all(barriers.join(phase)).unwrap();
            }
            let planned = cli(
                &f.repo,
                &task_state,
                &["task", "plan", "--file", "ticket.json"],
            );
            let mut active = store.reopen(std::time::Duration::from_secs(1)).unwrap();
            let lease = active
                .take_task_lease(
                    &cas,
                    planned["task_id"].as_str().unwrap(),
                    "active-writer",
                    6_000,
                )
                .unwrap();
            let child = crate::common::af()
                .current_dir(&f.repo)
                .args(args)
                .args(["--json", "--state"])
                .arg(&f.state)
                .env("AF_TEST_WAIT_OBSERVATION_BARRIER", &barriers)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut collector = store.reopen(std::time::Duration::ZERO).unwrap();
            for phase in ["template-captured-1", "template-verifying-1"] {
                let barrier = barriers.join(phase);
                let initial_expiry = active.task_lease_state(&lease).unwrap();
                let started = std::time::Instant::now();
                while !barrier.join("ready").exists() {
                    assert!(started.elapsed() < std::time::Duration::from_secs(30));
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                assert_eq!(std::fs::metadata(&object).unwrap().modified().unwrap(), old);
                assert!(store.is_empty(&run).unwrap());
                let refused = collector
                    .apply_task_collection(&cas, 0, 0, false)
                    .unwrap_err();
                assert!(
                    refused
                        .to_string()
                        .contains("CAS capture/collection reservation"),
                    "{refused}"
                );
                // Exercise the real heartbeat during capture and final artifact verification.
                // Holding its shared handle forces renewal through the bounded-timeout connection.
                let shared = std::sync::Mutex::new(&mut active);
                let cancelled = std::sync::atomic::AtomicBool::new(false);
                let lost = std::sync::atomic::AtomicBool::new(false);
                af_runtime::task::lease::with_heartbeat_supervised(
                    &shared,
                    &cas,
                    &lease,
                    af_runtime::task::lease::HeartbeatControl {
                        cancellation: Some(&cancelled),
                        writer_lost: Some(&lost),
                        timing: af_runtime::task::lease::LeaseTiming::for_lease(6_000),
                    },
                    || {
                        let _busy = shared.lock().unwrap();
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(20);
                        while store.task_lease_state(&lease).unwrap() < initial_expiry + 3_000 {
                            assert!(!cancelled.load(std::sync::atomic::Ordering::Acquire));
                            assert!(
                                std::time::Instant::now() < deadline,
                                "heartbeat did not renew"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        assert!(!barrier.join("proceed").exists());
                        Ok(())
                    },
                )
                .unwrap();
                assert!(!lost.load(std::sync::atomic::Ordering::Acquire));
                assert!(!cancelled.load(std::sync::atomic::Ordering::Acquire));
                // The capture reservation must still exclude collection until publication.
                let refused = collector
                    .apply_task_collection(&cas, 0, 0, false)
                    .unwrap_err();
                assert!(
                    refused
                        .to_string()
                        .contains("CAS capture/collection reservation"),
                    "{refused}"
                );
                assert_eq!(std::fs::metadata(&object).unwrap().modified().unwrap(), old);
                std::fs::write(barrier.join("proceed"), []).unwrap();
            }
            active.release_task_lease(&cas, &lease).unwrap();
            assert_eq!(value(child.wait_with_output().unwrap(), 0), pinned);
            collector.apply_task_collection(&cas, 0, 0, false).unwrap();
            cas.get_json(&captured).unwrap();
            // A killed capture has no durable authority and leaves no reservation to expire.
            // Recovery is ordinary collection, followed by an explicit new pin from local bytes.
            db.execute("DELETE FROM events WHERE run_id=?1", [&run])
                .unwrap();
            let barriers = f.machine.join("killed-pin-barriers");
            let barrier = barriers.join("template-captured-1");
            std::fs::create_dir_all(&barrier).unwrap();
            let mut killed = crate::common::af()
                .current_dir(&f.repo)
                .args(args)
                .args(["--json", "--state"])
                .arg(&f.state)
                .env("AF_TEST_WAIT_OBSERVATION_BARRIER", &barriers)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let started = std::time::Instant::now();
            while !barrier.join("ready").exists() {
                assert!(started.elapsed() < std::time::Duration::from_secs(30));
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            killed.kill().unwrap();
            assert!(!killed.wait().unwrap().success());
            assert!(store.is_empty(&run).unwrap());
            collector.apply_task_collection(&cas, 0, 0, false).unwrap();
            assert!(cas.get_json(&captured).is_err());
            assert_eq!(cli(&f.repo, &f.state, &args), pinned);
            // Capture a real, valid authority with identical local bindings but a different
            // committed policy. The forged template below will be explicitly machine-admitted.
            std::fs::write(&bindings, toml::to_string(&definition).unwrap()).unwrap();
            let policy_path = f.repo.join(".af/code-policy.toml");
            let original = std::fs::read_to_string(&policy_path).unwrap();
            let changed = original.replace("check_wall_ms = 5000", "check_wall_ms = 4000");
            assert_ne!(changed, original);
            std::fs::write(&policy_path, changed).unwrap();
            for command in [
                vec!["add", ".af/code-policy.toml"],
                vec!["commit", "-qm", "other template authority"],
            ] {
                assert!(
                    std::process::Command::new("git")
                        .current_dir(&f.repo)
                        .args(command)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let other_commit = std::process::Command::new("git")
                .current_dir(&f.repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            assert!(other_commit.status.success());
            let other_commit = String::from_utf8(other_commit.stdout).unwrap();
            let mut other_args = args;
            other_args[9] = other_commit.trim();
            let other = cli(&f.repo, &f.state, &other_args);
            assert_eq!(
                other["template"]["bindings"],
                pinned["template"]["bindings"]
            );
            assert_ne!(
                other["template"]["authority_id"],
                pinned["template"]["authority_id"]
            );
            unrelated_authority = Some(other["template"]["authority_id"].clone());
            // Restore the fixture's source for the successful immutable-reuse cases.
            assert!(
                std::process::Command::new("git")
                    .current_dir(&f.repo)
                    .args(["reset", "--hard", commit.trim()])
                    .status()
                    .unwrap()
                    .success()
            );
            std::fs::rename(&package, &moved).unwrap();
        }

        std::fs::write(
            moved.join("worker.py"),
            "raise Exception('mutable bytes must not execute')",
        )
        .unwrap();
        std::fs::write(&bindings, "invalid after capture").unwrap();
        std::fs::remove_dir_all(&local).unwrap();
        pins.push(pinned);
        workers.push(captured);
    }
    assert_ne!(pins[0]["template_id"], pins[1]["template_id"]);
    assert_ne!(workers[0], workers[1]);
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
    policy["task_templates"] = json!(
        pins.iter()
            .map(|p| p["template_id"].clone())
            .collect::<Vec<_>>()
    );
    write(&f.policy, &policy);
    let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    let main = &mut bundle["definitions"]["main"];
    main["authority"]["task_templates"] = policy["task_templates"].clone();
    main["steps"] = json!(
        pins.iter()
            .enumerate()
            .map(|(i, p)| step(
                &format!("customer-{i}"),
                json!({"kind":"fresh_task","template":p["template"],"bindings":{}}),
                json!({}),
                "task"
            ))
            .collect::<Vec<_>>()
    );
    write(&f.bundle, &bundle);
    let done = f.start("captured-customers");
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["fresh_tasks"].as_object().unwrap().len(), 2);
    for (index, pin) in pins.iter().enumerate() {
        let receipt = &done["frames"]["root"]["steps"][index]["outputs"]["result"]["value"];
        let cas = af_store::Cas::open_existing(task_state.join("cas")).unwrap();
        let revision = cas
            .get_artifact(receipt["revision_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            revision.payload["authority"]["policy_id"],
            pin["template"]["authority_id"]
        );
    }
    assert_eq!(
        cli(
            &f.repo,
            &f.state,
            &["scenario", "resume", "captured-customers"]
        ),
        done
    );
    cli(
        &f.repo,
        &task_state,
        &["task", "gc", "--older-than", "0", "--keep", "0", "--apply"],
    );
    let again = f.start("fresh-after-collection");
    assert_eq!(again["status"], "succeeded", "{again}");
    // Corrupt and missing captured package bytes both fail without consulting deleted paths.
    let digest = workers[0].strip_prefix("sha256:").unwrap();
    let object = task_state
        .join("cas/objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    let bytes = std::fs::read(&object).unwrap();
    std::fs::write(&object, b"corrupt captured package").unwrap();
    for run in ["corrupt-package", "missing-package"] {
        let failed = f.start(run);
        assert_eq!(failed["status"], "failed", "{failed}");
        assert!(failed["fresh_tasks"].as_object().unwrap().is_empty());
        std::fs::remove_file(&object).ok();
    }
    std::fs::write(&object, bytes).unwrap();
    // Even a newly authorized template cannot substitute missing native authority,
    // a different committed source, or Provider mappings absent from its sealed closure.
    for (case, (field, replacement)) in [
        ("authority_id", json!(format!("sha256:{}", "f".repeat(64)))),
        ("authority_id", unrelated_authority.unwrap()),
        (
            "source_content_digest",
            json!(format!("sha256:{}", "f".repeat(64))),
        ),
        ("repository_id", json!("another-repository")),
        ("authority_commit", json!("0".repeat(40))),
        (
            "bindings",
            json!({"schema":"af.task-bindings/1","packages":{},"slots":{},"providers":{"local/alice":"substituted"}}),
        ),
    ].into_iter().enumerate() {
        let before = cli(&f.repo, &task_state, &["task", "list"]);
        let mut template = pins[0]["template"].clone();
        template[field] = replacement;
        let id = af_scenario::contract::hash(&template).unwrap();
        policy["task_templates"] = json!([id]);
        write(&f.policy, &policy);
        bundle["definitions"]["main"]["authority"]["task_templates"] = json!([id]);
        bundle["definitions"]["main"]["steps"] = json!([step(
            "changed",
            json!({"kind":"fresh_task","template":template,"bindings":{}}),
            json!({}),
            "task"
        )]);
        write(&f.bundle, &bundle);
        let failed = f.start(&format!("changed-{field}-{case}"));
        assert_eq!(failed["status"], "failed", "{field}: {failed}");
        assert!(failed["fresh_tasks"].as_object().unwrap().is_empty());
        assert_eq!(before, cli(&f.repo, &task_state, &["task", "list"]));
    }
    let mut old = pins[0]["template"].clone();
    old["schema"] = json!("af/scenario-task-template@1");
    assert!(!crate::schemas::validator("scenario-task-template-v2.json").is_valid(&old));
}

#[test]
fn template_sealed_restore_refuses_incompatible_workers_and_same_model_verification() {
    for case in ["typed-contract", "widened-effects", "same-model"] {
        let f = Fixture::bare();
        std::fs::remove_dir(&f.repo).unwrap();
        let root = f._root.path().join("task-fixture");
        std::fs::create_dir(&root).unwrap();
        let (repo, task_state) = crate::task_cli::fixture_named(&root, "pagination");
        std::fs::rename(repo, &f.repo).unwrap();
        let providers = crate::planning_models::Providers::new(&f.machine);
        if case == "same-model" {
            let path = f.repo.join(".af/task-catalog.toml");
            let mut catalog: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            catalog["independence"]["distinct_principals"] = toml::Value::Boolean(false);
            catalog["independence"]["distinct_models"] = toml::Value::Boolean(true);
            // Author the disposable fixture's model variant with one explicit native
            // Provider admission Attempt, alongside its three ordinary Attempts.
            let pipeline_path = f.repo.join(".af/task-packages/fixture/implementation");
            let manifest_path = pipeline_path.join("pipeline.toml");
            let mut pipeline: toml::Value =
                toml::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
            pipeline["max_attempts"] = toml::Value::Integer(4);
            std::fs::write(&manifest_path, toml::to_string(&pipeline).unwrap()).unwrap();
            catalog["packages"]["fixture/implementation"]["digest"] = toml::Value::String(
                af_pipeline_config::lock::package_digest("fixture/implementation", &pipeline_path)
                    .unwrap(),
            );
            std::fs::write(&path, toml::to_string(&catalog).unwrap()).unwrap();
            for args in [
                ["add", ".af"].as_slice(),
                ["commit", "-qm", "require distinct models"].as_slice(),
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
        }
        let commit = std::process::Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(commit.status.success());
        let commit = String::from_utf8(commit.stdout).unwrap();
        let local = f.machine.join("local");
        let mut packages = serde_json::Map::new();
        let mut slots = serde_json::Map::new();
        let mut aliases = serde_json::Map::new();
        for role in ["implementer", "evaluator"] {
            let name = format!("local/{role}");
            let package = local.join(role);
            crate::task_cli::copy_tree(
                &f.repo.join(".af/task-packages/fixture").join(role),
                &package,
            );
            let path = package.join("worker.toml");
            let mut worker: Value = serde_json::to_value(
                toml::from_str::<toml::Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(),
            )
            .unwrap();
            worker["name"] = json!(name);
            if role == "implementer" {
                match case {
                    "typed-contract" => {
                        worker["signature"]["contract"]["inputs"]["requirements"]["artifact_type"] =
                            json!("af/Document@1")
                    }
                    "widened-effects" => {
                        worker["signature"]["effects"] = json!(["write-source", "execute-checks"])
                    }
                    _ => {}
                }
            }
            if case == "same-model" {
                worker["runner"] = json!({"kind":"model","provider_kind":"codex","model":"gpt-6.1-sol","effort":"medium"});
                worker["signature"]["attempt"]["tokens"] = json!(100);
                // Explicit Provider aliases cannot establish model independence.
                aliases.insert(name.clone(), json!("fixture-codex"));
            }
            std::fs::write(&path, toml::to_string(&worker).unwrap()).unwrap();
            packages.insert(name.clone(), json!({"version":"1.0.0","path":role,"digest":af_pipeline_config::lock::package_digest(&name,&package).unwrap()}));
            slots.insert(format!("root.slots.{role}"), json!(name));
        }
        let bindings = local.join("bindings.toml");
        std::fs::write(&bindings, toml::to_string(&json!({"schema":"af.task-bindings/1","packages":packages,"slots":slots,"providers":aliases})).unwrap()).unwrap();
        let mut document: Value =
            serde_json::from_slice(&std::fs::read(f.repo.join("ticket.json")).unwrap()).unwrap();
        document["verification"] = json!("evaluation");
        document["limits"]["tokens"] = json!(200000);
        document["limits"]["wall_ms"] = json!(1000000);
        document["limits"]["verification"]["tokens"] = json!(100000);
        document["limits"]["verification"]["wall_ms"] = json!(500000);
        if case == "same-model" {
            document["limits"]["max_attempts"] = json!(4);
            document["limits"]["verification"]["attempts"] = json!(3);
        }

        let file = f.machine.join("template.json");
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
                "--bindings",
                bindings.to_str().unwrap(),
            ],
        );
        // Capture admits valid package bytes without compiling the selected Pipeline.
        // Delete them before planning to exercise only the sealed restore path.
        std::fs::remove_dir_all(&local).unwrap();
        let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
        policy["task_templates"] = json!([pinned["template_id"]]);
        write(&f.policy, &policy);
        let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
        let main = &mut bundle["definitions"]["main"];
        main["authority"]["task_templates"] = policy["task_templates"].clone();
        main["limits"]["max_tokens"] = json!(200000);
        main["steps"] = json!([step(
            "refused",
            json!({"kind":"fresh_task","template":pinned["template"],"bindings":{}}),
            json!({}),
            "task"
        )]);
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
        let result = providers
            .command(&f.repo)
            .args([
                "scenario",
                "start",
                case,
                "--file",
                f.bundle.to_str().unwrap(),
                "--authority",
                f.policy.to_str().unwrap(),
                "--inputs",
                f.inputs.to_str().unwrap(),
                "--confirm-pin",
                pin["revision"].as_str().unwrap(),
                "--execute",
                "--json",
                "--state",
                f.state.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        let failed = value(result, 0);
        assert_eq!(failed["status"], "failed", "{case}: {failed}");
        assert!(
            failed["fresh_tasks"].as_object().unwrap().is_empty(),
            "{failed}"
        );
        let expected = if case == "same-model" {
            "Trusted policy requires distinct models"
        } else {
            "public contract or authority"
        };
        assert!(
            failed["failure"].as_str().unwrap().contains(expected),
            "{case}: {failed}"
        );
        // Silent Scenario planning refuses before recording a Task or executable plan.
        let listed = cli(&f.repo, &task_state, &["task", "list"]);
        assert!(listed["tasks"].as_array().unwrap().is_empty(), "{listed}");
        providers.assert_no_dispatch();
    }
}

#[test]
fn template_repin_refuses_conflicting_corrupt_and_duplicate_durable_records() {
    let f = Fixture::bare();
    std::fs::remove_dir(&f.repo).unwrap();
    let root = f._root.path().join("task-fixture");
    std::fs::create_dir(&root).unwrap();
    let (repo, task_state) = crate::task_cli::fixture_named(&root, "pagination");
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
    let args = [
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
    ];
    let pinned = cli(&f.repo, &f.state, &args);
    let run = format!(
        "scenario-template-{}",
        pinned["template_id"]
            .as_str()
            .unwrap()
            .trim_start_matches("sha256:")
    );
    let db = rusqlite::Connection::open(task_state.join("events.sqlite")).unwrap();
    let original: (String, String, String) = db
        .query_row(
            "SELECT type,payload,artifact_refs FROM events WHERE run_id=?1",
            [&run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let cas = af_store::Cas::open_existing(task_state.join("cas")).unwrap();
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&f.policy).unwrap()).unwrap();
    policy["task_templates"] = json!([pinned["template_id"]]);
    write(&f.policy, &policy);
    let mut bundle: Value = serde_json::from_slice(&std::fs::read(&f.bundle).unwrap()).unwrap();
    bundle["definitions"]["main"]["authority"]["task_templates"] = policy["task_templates"].clone();
    bundle["definitions"]["main"]["steps"] = json!([step(
        "fresh",
        json!({"kind":"fresh_task","template":pinned["template"],"bindings":{}}),
        json!({}),
        "task"
    )]);
    write(&f.bundle, &bundle);
    assert_eq!(f.start("canonical-pin")["status"], "succeeded");
    let snapshot = || {
        db.query_row(
            "SELECT json_array(run_id,sequence,event_id,type,occurred_at,node_id,attempt_id,causation_id,correlation_id,artifact_refs,payload) FROM events WHERE run_id=?1",
            [&run],
            |r| r.get::<_, String>(0),
        ).unwrap()
    };
    for field in [
        "node_id",
        "attempt_id",
        "causation_id",
        "correlation_id",
        "occurred_at",
    ] {
        let original_row = snapshot();
        let original_value: Option<String> = db
            .query_row(
                &format!("SELECT {field} FROM events WHERE run_id=?1"),
                [&run],
                |r| r.get(0),
            )
            .unwrap();
        db.execute(
            &format!("UPDATE events SET {field}=?1 WHERE run_id=?2"),
            rusqlite::params!["2026-11-13T00:00:00Z", run],
        )
        .unwrap();
        let before = snapshot();
        assert!(
            !output(&f.repo, &f.state, &args).status.success(),
            "{field}"
        );
        assert_eq!(snapshot(), before, "repin mutated {field}");
        let failed = f.start(&format!("corrupt-{field}"));
        assert_eq!(failed["status"], "failed", "{field}: {failed}");
        assert!(failed["fresh_tasks"].as_object().unwrap().is_empty());
        assert!(
            failed.to_string().contains("durable native pin"),
            "{failed}"
        );
        assert_eq!(snapshot(), before, "planning mutated {field}");
        db.execute(
            &format!("UPDATE events SET {field}=?1 WHERE run_id=?2"),
            rusqlite::params![original_value, run],
        )
        .unwrap();
        assert_eq!(snapshot(), original_row);
        assert_eq!(cli(&f.repo, &f.state, &args), pinned);
    }
    let conflicting = cas.put_json(&json!({"not":"the pinned template"})).unwrap();
    let missing = format!("sha256:{}", "f".repeat(64));
    for artifact in [&conflicting, &missing] {
        let payload = json!({"artifact_id":artifact}).to_string();
        let refs = json!([artifact]).to_string();
        db.execute(
            "UPDATE events SET payload=?1,artifact_refs=?2 WHERE run_id=?3",
            rusqlite::params![payload, refs, run],
        )
        .unwrap();
        let refused = output(&f.repo, &f.state, &args);
        assert!(
            !refused.status.success(),
            "{}",
            crate::task_report::text(&refused)
        );
        let after: (String, String, String) = db
            .query_row(
                "SELECT type,payload,artifact_refs FROM events WHERE run_id=?1",
                [&run],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            after,
            (original.0.clone(), payload, refs),
            "refusal must not rewrite the record"
        );
    }
    db.execute(
        "UPDATE events SET payload=?1,artifact_refs=?2 WHERE run_id=?3",
        rusqlite::params![original.1, original.2, run],
    )
    .unwrap();
    assert_eq!(cli(&f.repo, &f.state, &args), pinned);
    // Pin admission must examine the entire stream, including rows replay used to hide.
    for sequences in [vec![-1], vec![-9], vec![-1, 0], vec![1], vec![0, 2]] {
        db.execute("DELETE FROM events WHERE run_id=?1", [&run])
            .unwrap();
        for sequence in &sequences {
            db.execute(
                "INSERT INTO events (run_id,sequence,event_id,type,occurred_at,artifact_refs,payload)
                 VALUES (?1,?2,?3,?4,'2026-11-13T00:00:00Z',?5,?6)",
                rusqlite::params![run, sequence, format!("corrupt-{sequence}"), original.0, original.2, original.1],
            ).unwrap();
        }
        let snapshot = || {
            db.prepare("SELECT sequence,event_id,type,payload,artifact_refs FROM events WHERE run_id=?1 ORDER BY sequence")
                .unwrap().query_map([&run], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?)))
                .unwrap().collect::<Result<Vec<_>, _>>().unwrap()
        };
        let before = snapshot();
        assert!(
            !output(&f.repo, &f.state, &args).status.success(),
            "{sequences:?}"
        );
        assert_eq!(snapshot(), before, "refusal must preserve every row");
    }
    db.execute("DELETE FROM events WHERE run_id=?1", [&run])
        .unwrap();
    assert_eq!(cli(&f.repo, &f.state, &args), pinned);
    let mut store = af_store::EventStore::open(task_state.join("events.sqlite")).unwrap();
    store
        .append(
            &run,
            &cas,
            af_store::NewEvent::new(
                af_contracts::EventType::ScenarioTemplatePinnedV1,
                serde_json::from_str(&original.1).unwrap(),
            )
            .referencing(serde_json::from_str(&original.2).unwrap()),
        )
        .unwrap();
    let before = store.replay(&run).unwrap();
    assert_eq!(before.len(), 2);
    assert!(!output(&f.repo, &f.state, &args).status.success());
    assert_eq!(store.replay(&run).unwrap(), before);
    // A corrupt but individually valid sequential history must never be replayed while
    // repinning holds SQLite's writer lock. Keep a real unrelated Task heartbeat active.
    let duplicate = af_store::NewEvent::new(
        af_contracts::EventType::ScenarioTemplatePinnedV1,
        serde_json::from_str(&original.1).unwrap(),
    )
    .referencing(serde_json::from_str(&original.2).unwrap());
    store
        .append_batch(&run, &cas, &vec![duplicate; 4095])
        .unwrap();
    let before = store.replay(&run).unwrap();
    let planned = cli(
        &f.repo,
        &task_state,
        &["task", "plan", "--file", "ticket.json"],
    );
    let mut active = store.reopen(std::time::Duration::from_secs(1)).unwrap();
    let lease = active
        .take_task_lease(
            &cas,
            planned["task_id"].as_str().unwrap(),
            "pin-history-heartbeat",
            6_000,
        )
        .unwrap();
    let expiry = active.task_lease_state(&lease).unwrap();
    let shared = std::sync::Mutex::new(&mut active);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let lost = std::sync::atomic::AtomicBool::new(false);
    af_runtime::task::lease::with_heartbeat_supervised(
        &shared,
        &cas,
        &lease,
        af_runtime::task::lease::HeartbeatControl {
            cancellation: Some(&cancelled),
            writer_lost: Some(&lost),
            timing: af_runtime::task::lease::LeaseTiming::for_lease(6_000),
        },
        || {
            let _busy = shared.lock().unwrap();
            assert!(!output(&f.repo, &f.state, &args).status.success());
            assert_eq!(f.start("large-duplicate-pin")["status"], "failed");
            assert_eq!(store.replay(&run).unwrap(), before);
            db.execute("DELETE FROM events WHERE run_id=?1 AND sequence<>0", [&run])
                .unwrap();
            for field in [
                "event_id",
                "type",
                "occurred_at",
                "node_id",
                "attempt_id",
                "causation_id",
                "correlation_id",
                "payload",
                "artifact_refs",
            ] {
                let original: Option<String> = db
                    .query_row(
                        &format!("SELECT {field} FROM events WHERE run_id=?1"),
                        [&run],
                        |r| r.get(0),
                    )
                    .unwrap();
                db.execute(
                    &format!("UPDATE events SET {field}=?1 WHERE run_id=?2"),
                    rusqlite::params!["é".repeat(129), run],
                )
                .unwrap();
                let before = snapshot();
                assert!(
                    !output(&f.repo, &f.state, &args).status.success(),
                    "{field}"
                );
                let failed = f.start(&format!("oversized-{field}"));
                assert_eq!(failed["status"], "failed", "{field}: {failed}");
                assert!(failed["fresh_tasks"].as_object().unwrap().is_empty());
                assert_eq!(snapshot(), before, "refusal mutated {field}");
                db.execute(
                    &format!("UPDATE events SET {field}=?1 WHERE run_id=?2"),
                    rusqlite::params![original, run],
                )
                .unwrap();
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while store.task_lease_state(&lease).unwrap() < expiry + 3_000 {
                assert!(!cancelled.load(std::sync::atomic::Ordering::Acquire));
                assert!(
                    std::time::Instant::now() < deadline,
                    "heartbeat did not renew"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(())
        },
    )
    .unwrap();
    assert!(!lost.load(std::sync::atomic::Ordering::Acquire));
    assert!(!cancelled.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(cli(&f.repo, &f.state, &args), pinned);
}
