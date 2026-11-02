//! Real CLI orchestration across existing Tasks and process restarts, with a pinned,
//! credential-free service adapter. No real scenarios, Ants, Providers or remotes are used.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn output(repo: &Path, state: &Path, args: &[&str]) -> Output {
    crate::common::af()
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
        .output()
        .unwrap()
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
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let state = root.path().join("scenario-state");
        let policy = root.path().join("policy.json");
        let bundle = root.path().join("bundle.json");
        let inputs = root.path().join("inputs.json");
        let owned = root.path().join("owned");
        std::fs::create_dir(&owned).unwrap();
        let owned = owned.canonicalize().unwrap();
        let machine = root.path().join("machine");
        std::fs::create_dir(&machine).unwrap();
        let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        write(
            &policy,
            &json!({"schema":"af/scenario-policy@1","adapters":{},"task_plans":[],"file_roots":[owned],"decision_keys":{"owner":key.pk.to_box().unwrap().into_string()}}),
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
            &json!({"schema":"af/scenario-bundle@1","root":"main","definitions":{"main":{"schema":"af/scenario@1","inputs":{},"outputs":{"ok":"boolean"},"steps":steps,"result":{"ok":literal("boolean",json!(true))},"completion":[literal("boolean",json!(true))],"authority":{"task_plans":[],"grants":[g],"decision_owners":["owner"]},"limits":{"max_tokens":2000,"max_wall_ms":604800000,"max_dispatches":64}}}}),
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
        valid("scenario-pin-v1.json", &pinned);
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
    valid("scenario-run-v1.json", &waiting);
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
    valid("scenario-run-v1.json", &done);
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
    valid("scenario-run-v1.json", &cancelled);
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
        &json!({"schema":"af/ant@1","id":"daily","scenario":bundle,"inputs":{},"schedule":{"timezone":"UTC","hour":12,"minute":0,"start_unix_ms":now},"owner":"owner","notification":"local:owner","execution":{"overlap":"skip","missed":"skip","lateness_ms":1000,"per_run":bundle["definitions"]["main"]["limits"],"rolling_tokens":2000,"rolling_window_ms":172800000}}),
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
        valid("ant-tick-v1.json", &tick);
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
    valid("ant-state-v1.json", &paused);
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
    valid("scenario-pin-v1.json", &pin);
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
        std::fs::set_permissions(&self.machine, std::fs::Permissions::from_mode(0o700)).unwrap();
        let examples = workspace().join("fixtures/scenarios/github");
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
        std::fs::copy(examples.join("bundle.json"), &self.bundle).unwrap();
        std::fs::copy(
            examples.join("mock-service.json"),
            self.machine.join("mock-service.json"),
        )
        .unwrap();
    }
    fn github_state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.machine.join("mock-service.json")).unwrap())
            .unwrap()
    }
}

#[test]
fn production_github_adapter_runs_exact_head_pr_sequence_and_recovers_remote_effect_before_reply() {
    let f = Fixture::bare();
    f.install_github();
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
    valid("scenario-run-v1.json", &done);
    let journal: Value = serde_json::from_slice(
        &std::fs::read(f.machine.join("github-receipts/state.json")).unwrap(),
    )
    .unwrap();
    valid("github-adapter-state-v1.json", &journal);
    assert_eq!(journal["records"].as_object().unwrap().len(), 3);
    assert_eq!(
        f.github_state()["effects"],
        json!(["publish_pr:1", "notify:1", "followup:2"])
    );
    assert_eq!(
        cli(&f.repo, &f.state, &["scenario", "resume", "github-pr"]),
        done
    );
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
