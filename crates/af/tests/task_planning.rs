//! Real CLI planning, compiler repair, exact approval and restart on one Task ledger.
use review_core::task::pipeline::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
#[path = "support/task_cli.rs"]
mod task_cli;

/// Wall budget every Task in this file is started with, replacing the fixtures' own 60s.
///
/// These sequences chain many real subprocesses — `af` invocations, Git commits, catalog
/// operations, signing and Python Workers — between Task creation and the resumed run, while a
/// Task deadline is absolute wall-clock measured once at creation. The fixture budget therefore
/// left the last `prepare` racing the still-required verification reserve: under `make check`'s
/// four test threads those subprocesses stretch far enough that a valid generated-plan resume
/// was refused with `Task deadline protects still-required verification`.
///
/// Ten minutes is far above the slowest observed sequence, so the deadline stops being the
/// load-sensitive threshold. It is scheduling margin only: the per-Attempt wall (5s in every
/// fixture Worker manifest and in the fixture code policy), the Attempt count, the token budget
/// and the verification reserve are untouched and still bind exactly as before, and the
/// deadline's own fail-closed refusal keeps its coverage in
/// `crates/review-attempt/tests/task_budget.rs`. Do not collapse it back toward the elapsed time
/// of a fast local run (ADR-0113).
const PLANNING_WALL_MS: u64 = 600_000;
/// Every fixture Worker manifest and the fixture code policy bound one Attempt at five seconds.
const FIXTURE_ATTEMPT_WALL_MS: u64 = 5_000;
/// Slack the budget keeps beyond every Attempt a Task may start plus its whole reserve.
const PLANNING_SCHEDULING_MARGIN_MS: u64 = 300_000;

fn run(repo: &Path, state: &Path, args: &[&str], code: i32) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state)
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
fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn commit(repo: &Path) {
    for args in [["add", "-A"], ["commit", "-qm"]] {
        let mut cmd = Command::new("git");
        cmd.current_dir(repo).args(args);
        if args[0] == "commit" {
            cmd.arg("planning authority");
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
fn package(repo: &Path, catalog: &mut toml::Value, name: &str) {
    let path = format!(".af/task-packages/{name}");
    let digest = review_config::lock::package_digest(name, &repo.join(&path)).unwrap();
    catalog["packages"].as_table_mut().unwrap().insert(
        name.into(),
        toml::Value::try_from(json!({"version":"1.0.0","path":path,"digest":digest})).unwrap(),
    );
}
fn setup(root: &Path, repair: bool, nested: bool) -> (PathBuf, PathBuf, minisign::KeyPair) {
    setup_variant(root, repair, nested, "pagination")
}
fn setup_variant(
    root: &Path,
    repair: bool,
    nested: bool,
    fixture: &str,
) -> (PathBuf, PathBuf, minisign::KeyPair) {
    let (repo, state) = task_cli::fixture_named(root, fixture);
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    let pipeline_path = repo.join(".af/task-packages/fixture/implementation/pipeline.toml");
    let mut existing: PipelineDefinitionV1 =
        toml::from_str(&std::fs::read_to_string(&pipeline_path).unwrap()).unwrap();
    let mut generated = existing.clone();
    generated.name = "generated/implementation".into();
    // The execution Worker intentionally reuses the Planner's qualified node name. Old
    // selected outputs and compiler feedback must remain scoped to the bootstrap plan.
    generated.nodes[0].id = "plan".into();
    for node in &mut generated.nodes {
        for input in node.inputs.values_mut() {
            if let ValueRefV1::Node { node, .. } = input
                && node == "implement"
            {
                *node = "plan".into();
            }
        }
    }
    existing
        .accepts
        .required_facts
        .insert("small".into(), serde_json::from_value(json!(true)).unwrap());
    std::fs::write(pipeline_path, toml::to_string(&existing).unwrap()).unwrap();
    package(&repo, &mut catalog, "fixture/implementation");
    let mut definitions =
        BTreeMap::from([(generated.name.clone(), toml::to_string(&generated).unwrap())]);
    if nested {
        generated.name = "generated/root".into();
        generated.slots.clear();
        generated.nodes = vec![TaskNodeV1 {
            id: "implementation".into(),
            operator: TaskOperatorV1::Call {
                pipeline: "generated/implementation".into(),
                bindings: BTreeMap::new(),
            },
            inputs: generated
                .contract
                .inputs
                .keys()
                .map(|port| (port.clone(), ValueRefV1::Input { port: port.clone() }))
                .collect(),
            when: None,
        }];
        generated.outputs = generated
            .contract
            .outputs
            .keys()
            .map(|port| {
                (
                    port.clone(),
                    ValueRefV1::Node {
                        node: "implementation".into(),
                        port: port.clone(),
                    },
                )
            })
            .collect();
        generated.coverage = BTreeMap::from([(
            "verified".into(),
            ValueRefV1::Node {
                node: "implementation".into(),
                port: "verification".into(),
            },
        )]);
        definitions.insert(generated.name.clone(), toml::to_string(&generated).unwrap());
    }
    let proposal =
        json!({"schema":"af.pipeline-proposal/1","root":generated.name,"definitions":definitions});
    let worker_dir = repo.join(".af/task-packages/fixture/planner");
    std::fs::create_dir_all(worker_dir.join("outputs")).unwrap();
    write_json(&worker_dir.join("proposal.json"), &proposal);
    let mut bad = proposal.clone();
    bad["definitions"][generated.name.clone()] =
        json!("schema = 'af.pipeline/1'\nname = 'generated/implementation'\n");
    write_json(&worker_dir.join("bad.json"), &bad);
    std::fs::write(worker_dir.join("worker.py"), format!(r#"import json, sys
from pathlib import Path
request = json.load(sys.stdin)
payload = request['inputs']['request'][0]['payload']
assert payload['schema'] == 'af.planning-request/1'
assert set(payload) == {{'schema','task','operators','workers','pipelines','bounds'}}
assert not Path('pagination.py').exists()
assert all('instructions' not in worker and 'runner' not in worker for worker in payload['workers'].values())
assert 'operator/planning-context' not in payload['operators']
repair = {repair}
if request['feedback']:
    prior = request['feedback'][0]['payload']
    assert prior['code'] == 'compiler_rejected'
    assert prior['compiler']['diagnostics']
    assert prior['compiler']['proposal'] == json.loads(Path(__file__).with_name('bad.json').read_text())
name = 'bad.json' if repair and not request['feedback'] else 'proposal.json'
proposal = json.loads(Path(__file__).with_name(name).read_text())
print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'proposal':[proposal]}}}}))
"#, repair=if repair {"True"} else {"False"})).unwrap();
    let port = |kind| json!({"artifact_type":kind,"cardinality":"one","optional":false,"affinity":{"kind":"unbound"},"covers":[]});
    let manifest: review_config::task::catalog::TaskWorkerManifest = serde_json::from_value(json!({
        "schema":"af.worker/1","name":"fixture/planner","version":"1.0.0",
        "signature":{"contract":{"inputs":{"request":port("af/PlanningRequest@1")},"outputs":{"proposal":port("af/PipelineProposal@1")}},"effects":[],"roles":["plan"],"evidence":{},"retains":{"proposal":["request"]},"worker_input_type":"af/PlannerInput@1","worker_output_type":"af/PipelineProposal@1","attempt":{"tokens":0,"wall_ms":5000}},
        "runner":{"kind":"command","command":{"program":"/usr/bin/python3","args":[{"value":"-B","provenance":"literal"},{"value":"@package/worker.py","provenance":"literal"}]}}
    })).unwrap();
    std::fs::write(
        worker_dir.join("worker.toml"),
        toml::to_string(&manifest).unwrap(),
    )
    .unwrap();
    write_json(
        &worker_dir.join("input.schema.json"),
        &json!({"type":"object","additionalProperties":false,"required":["request"],"properties":{"request":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"object"}}}}),
    );
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../schemas/pipeline-proposal-v1.json")).unwrap();
    schema.as_object_mut().unwrap().remove("$id");
    schema.as_object_mut().unwrap().remove("$schema");
    write_json(&worker_dir.join("outputs/proposal.schema.json"), &schema);
    package(&repo, &mut catalog, "fixture/planner");
    catalog.as_table_mut().unwrap().insert("developers".into(),toml::Value::try_from(json!({"schema":"af.task-developers/1","keys":{"owner":key.pk.to_box().unwrap().into_string()}})).unwrap());
    catalog.as_table_mut().unwrap().insert(
        "planner".into(),
        toml::Value::try_from(
            json!({"worker":"fixture/planner","max_attempts":if repair {2} else {1}}),
        )
        .unwrap(),
    );
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    let ticket_path = repo.join("ticket.json");
    let mut ticket: Value = serde_json::from_slice(&std::fs::read(&ticket_path).unwrap()).unwrap();
    ticket["pipeline"]["fallback"] = json!("generate");
    ticket["facts"] = json!({"small":false});
    ticket["limits"]["max_attempts"] =
        json!(ticket["limits"]["max_attempts"].as_u64().unwrap() + if repair { 2 } else { 1 });
    ticket["limits"]["wall_ms"] = json!(PLANNING_WALL_MS);
    write_json(&ticket_path, &ticket);
    commit(&repo);
    (repo, state, key)
}
fn decide(
    repo: &Path,
    state: &Path,
    root: &Path,
    key: &minisign::KeyPair,
    rejected: bool,
) -> Value {
    let decision = if rejected { "rejected" } else { "approved" };
    let command = if rejected { "reject" } else { "approve" };
    let payload = root.join(format!("{decision}.payload"));
    let signature = root.join(format!("{decision}.minisig"));
    run(
        repo,
        state,
        &[
            "task",
            "decision-payload",
            "pagination-cli",
            "--developer",
            "owner",
            "--decision",
            decision,
            "--reason",
            "Reviewed generated closure and acceptance",
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
            Some("exact generated plan"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    run(
        repo,
        state,
        &[
            "task",
            command,
            "pagination-cli",
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
        0,
    )
}
fn approve(repo: &Path, state: &Path, root: &Path, key: &minisign::KeyPair) -> Value {
    decide(repo, state, root, key, false)
}

/// The sequences below are chains of real subprocesses, so nothing inside a test can make the
/// Task deadline deterministic; only the admitted budget can keep it off the critical path.
/// Pin that budget: every Attempt a Task may ever start, plus the whole protected reserve, has
/// to fit with the documented margin still to spare, and the margin must be added to the budget
/// rather than taken out of the reserve it protects.
#[test]
fn planning_fixtures_keep_the_task_deadline_off_the_load_sensitive_path() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, _) = setup(root.path(), true, true);
    let planned = run(&repo, &state, &["task", "plan", "--file", "ticket.json"], 0);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let revision = cas
        .get_json(planned["revision_id"].as_str().unwrap())
        .unwrap();
    let limits = &revision["payload"]["limits"];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let deadline = limits["deadline_unix_ms"].as_u64().unwrap();
    let headroom = deadline
        .checked_sub(now)
        .expect("the admitted Task deadline is still ahead of the real clock");
    let attempts = limits["max_attempts"].as_u64().unwrap();
    let reserve = limits["verification"]["wall_ms"].as_u64().unwrap();
    let committed = attempts * FIXTURE_ATTEMPT_WALL_MS + reserve;
    assert!(
        headroom >= committed + PLANNING_SCHEDULING_MARGIN_MS,
        "{headroom}ms of Task deadline leaves no load margin over {committed}ms of Attempt and \
         reserve wall; a slow four-thread run will refuse a valid resume"
    );
    let ticket_path = repo.join("ticket.json");
    let ticket: Value = serde_json::from_slice(&std::fs::read(&ticket_path).unwrap()).unwrap();
    assert_eq!(ticket["limits"]["wall_ms"], json!(PLANNING_WALL_MS));
    assert_eq!(limits["verification"], ticket["limits"]["verification"]);
    assert_eq!(limits["tokens"], ticket["limits"]["tokens"]);
    assert_eq!(limits["max_attempts"], ticket["limits"]["max_attempts"]);
}

#[test]
fn export_refuses_preparation_private_task_values_and_unsafe_destinations() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, _) = setup(root.path(), false, false);
    let ticket: Value =
        serde_json::from_slice(&std::fs::read(repo.join("ticket.json")).unwrap()).unwrap();
    let worker_path = repo.join(".af/task-packages/fixture/implementer/worker.py");
    let mut source = std::fs::read_to_string(&worker_path).unwrap();
    source.push_str(&format!("\n# {}\n", ticket["goal"].as_str().unwrap()));
    std::fs::write(worker_path, source).unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    package(&repo, &mut catalog, "fixture/implementer");
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    commit(&repo);
    run(&repo, &state, &["task", "plan", "--file", "ticket.json"], 0);
    let rejected = |destination: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args([
                "task",
                "export",
                "pagination-cli",
                "--name",
                "team/pagination",
                "--destination",
                destination,
                "--state",
            ])
            .arg(&state)
            .output()
            .unwrap();
        assert!(!output.status.success());
        String::from_utf8(output.stderr).unwrap()
    };
    assert!(rejected("shared").contains("not produced"));
    run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        0,
    );
    let waiting = run(&repo, &state, &["task", "explain", "pagination-cli"], 0);
    assert!(rejected("shared").contains("embeds originating Task state"));
    assert!(!repo.join("shared").exists());
    for path in ["../escape", ".git/export", "/tmp/af-export"] {
        assert!(rejected(path).contains("safe project-relative"));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.path(), repo.join("escape-link")).unwrap();
        assert!(rejected("escape-link/export").contains("symlink"));
    }
    assert_eq!(
        run(&repo, &state, &["task", "explain", "pagination-cli"], 0),
        waiting
    );
}

#[test]
fn generated_definition_exports_without_approval_and_a_second_developer_reuses_it_without_planning()
{
    let root = tempfile::tempdir().unwrap();
    let (repo, state, key) = setup(root.path(), false, true);
    let waiting = run(
        &repo,
        &state,
        &["task", "start", "--execute", "--file", "ticket.json"],
        0,
    );
    assert_eq!(waiting["attempts"], 1);
    let before = run(&repo, &state, &["task", "explain", "pagination-cli"], 0);
    let exported = run(
        &repo,
        &state,
        &[
            "task",
            "export",
            "pagination-cli",
            "--name",
            "team/pagination",
            "--destination",
            "shared",
        ],
        0,
    );
    assert_eq!(exported["execution_authorized"], false);
    assert_eq!(exported["packages"].as_object().unwrap().len(), 4);
    assert!(exported["packages"]["team/pagination/implementation"].is_object());
    assert!(exported["packages"]["fixture/planner"].is_null());
    assert_eq!(
        run(&repo, &state, &["task", "explain", "pagination-cli"], 0),
        before
    );
    let original = std::fs::read(repo.join("shared/catalog.toml")).unwrap();
    let denied = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "export",
            "pagination-cli",
            "--name",
            "team/pagination",
            "--destination",
            "shared",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert_eq!(
        std::fs::read(repo.join("shared/catalog.toml")).unwrap(),
        original
    );
    // Bundle-relative pins work after moving the complete directory within its Git repo.
    std::fs::create_dir(repo.join("relocated")).unwrap();
    std::fs::rename(repo.join("shared"), repo.join("relocated/bundle")).unwrap();
    commit(&repo);
    let catalog = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    let checked = catalog(&[
        "catalog",
        "test",
        "--source",
        ".",
        "--manifest",
        "relocated/bundle/catalog.toml",
        "--json",
    ]);
    assert_eq!(checked["attempts"], 0);
    assert_eq!(checked["contract_fixtures"], "passed");
    let checked = catalog(&[
        "catalog",
        "test",
        "--source",
        ".",
        "--manifest",
        "relocated/bundle/catalog.toml",
        "--worker",
        "fixture/evaluator",
        "--json",
    ]);
    assert_eq!(checked["prerequisites"].as_array().unwrap().len(), 1);
    approve(&repo, &state, root.path(), &key);
    assert_eq!(
        run(
            &repo,
            &state,
            &["task", "run", "--execute", "pagination-cli"],
            0
        )["result"]["acceptance"],
        "satisfied"
    );

    let second = root.path().join("second-developer");
    let (consumer, consumer_state) = task_cli::fixture_named(&second, "pagination");
    let imported = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&consumer)
        .args(["catalog", "sync", "--source"])
        .arg(&repo)
        .args([
            "--manifest",
            "relocated/bundle/catalog.toml",
            "--destination",
            ".af/vendor/team",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let catalog_path = consumer.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    catalog["packages"] = toml::Value::Table(toml::map::Map::new());
    catalog.as_table_mut().unwrap().insert(
        "imports".into(),
        toml::Value::try_from(vec![".af/vendor/team/catalog.lock.json"]).unwrap(),
    );
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    std::fs::remove_dir_all(consumer.join(".af/task-packages")).unwrap();
    let ticket_path = consumer.join("ticket.json");
    let mut ticket: Value = serde_json::from_slice(&std::fs::read(&ticket_path).unwrap()).unwrap();
    ticket["task_id"] = json!("second-pagination");
    ticket["pipeline"]["name"] = json!("team/pagination");
    // The consuming developer's Task is a fresh copy of the fixture file, so it needs the same
    // scheduling margin as the producing one: its three Attempts run behind a catalog sync and
    // a Git commit on the same loaded machine.
    ticket["limits"]["wall_ms"] = json!(PLANNING_WALL_MS);
    write_json(&ticket_path, &ticket);
    commit(&consumer);
    let done = run(
        &consumer,
        &consumer_state,
        &["task", "start", "--execute", "--file", "ticket.json"],
        0,
    );
    assert_eq!(done["result"]["acceptance"], "satisfied");
    assert_eq!(done["attempts"], 3);
    assert!(done["planning"].is_null());
    let done = run(
        &consumer,
        &consumer_state,
        &["task", "explain", "second-pagination"],
        0,
    );
    assert_eq!(done["plan"]["generated_origins"], json!([]));
    assert!(done["plan"]["dependencies"]["fixture/planner"].is_null());
}

#[test]
fn generated_nested_plan_waits_for_exact_approval_then_resumes_with_shared_accounting() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, key) = setup(root.path(), false, true);
    let planned = run(&repo, &state, &["task", "plan", "--file", "ticket.json"], 0);
    assert_eq!(planned["attempts"], 0);
    assert_eq!(planned["plan"]["preparation"], json!({"kind":"planning"}));
    let waiting = run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        0,
    );
    assert_eq!(waiting["attempts"], 1);
    assert_eq!(
        waiting["phase"],
        json!({"kind":"waiting","reason":"needs_plan_review"})
    );
    assert_eq!(
        waiting["plan"]["generated_origins"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(waiting["result"].is_null());
    let denied = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args(["task", "run", "--execute", "pagination-cli", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(!denied.status.success());
    let unchanged = run(&repo, &state, &["task", "explain", "pagination-cli"], 0);
    assert_eq!(unchanged["attempts"], 1);
    assert_eq!(unchanged["plan_id"], waiting["plan_id"]);
    std::fs::write(
        repo.join(".af/task-catalog.toml"),
        "edited unavailable authority",
    )
    .unwrap();
    std::fs::remove_dir_all(repo.join(".af/task-packages")).unwrap();
    let approved = approve(&repo, &state, root.path(), &key);
    assert_eq!(approved["attempts"], 1);
    let done = run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        0,
    );
    assert_eq!(done["attempts"], 4);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    assert_eq!(done["plan_id"], waiting["plan_id"]);
    assert_eq!(done["plan_decisions"], approved["plan_decisions"]);
    assert_eq!(
        run(
            &repo,
            &state,
            &["task", "run", "--execute", "pagination-cli"],
            0
        ),
        done
    );
}

#[test]
fn planner_repairs_once_from_durable_compiler_feedback_and_never_runs_its_proposal() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, key) = setup(root.path(), true, false);
    let waiting = run(
        &repo,
        &state,
        &["task", "start", "--execute", "--file", "ticket.json"],
        0,
    );
    assert_eq!(waiting["attempts"], 2);
    assert_eq!(waiting["phase"]["reason"], "needs_plan_review");
    let feedback: Vec<_> = waiting["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["record"]["result"]["feedback_id"].as_str())
        .collect();
    assert_eq!(feedback.len(), 1);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let artifact = cas.get_json(feedback[0]).unwrap();
    assert_eq!(artifact["payload"]["code"], "compiler_rejected");
    approve(&repo, &state, root.path(), &key);
    let done = run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        0,
    );
    assert_eq!(done["attempts"], 5);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    let prepared: Vec<_> = done["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| {
            matches!(
                entry["record"]["kind"].as_str(),
                Some("prepared" | "reserved")
            )
        })
        .collect();
    assert_eq!(
        prepared[1]["record"]["feedback_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        prepared[2]["record"]["feedback_ids"],
        json!([]),
        "The new plan must not inherit a colliding old node's compiler feedback"
    );
}

#[test]
fn generated_implementation_embeds_review_and_adds_only_its_admitted_history_constructor() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, key) = setup_variant(root.path(), false, false, "embedded-review");
    let waiting = run(
        &repo,
        &state,
        &["task", "start", "--execute", "--file", "ticket.json"],
        0,
    );
    assert_eq!(waiting["phase"]["reason"], "needs_plan_review");
    assert_eq!(waiting["attempts"], 1);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let before = cas
        .get_json(waiting["planning"]["request_revision_id"].as_str().unwrap())
        .unwrap();
    let after = cas
        .get_json(waiting["revision_id"].as_str().unwrap())
        .unwrap();
    assert!(before["payload"]["inputs"]["history"].is_null());
    assert_eq!(
        after["payload"]["inputs"]["history"]["artifact_type"],
        "af/ReviewHistory@1"
    );
    assert_eq!(
        after["payload"]["inputs"]["source"],
        before["payload"]["inputs"]["source"]
    );
    assert_eq!(after["payload"]["limits"], before["payload"]["limits"]);
    approve(&repo, &state, root.path(), &key);
    let done = run(
        &repo,
        &state,
        &["task", "run", "--execute", "pagination-cli"],
        0,
    );
    assert_eq!(done["attempts"], 6);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    assert_eq!(done["review_rounds"].as_array().unwrap().len(), 1);
}

#[test]
fn bounded_planner_failures_and_remaining_budget_refusals_never_dispatch_generated_work() {
    for case in ["invalid_twice", "self_approval", "no_remaining_attempts"] {
        let root = tempfile::tempdir().unwrap();
        let (repo, state, _) = setup(root.path(), case != "no_remaining_attempts", false);
        let package_dir = repo.join(".af/task-packages/fixture/planner");
        if case == "invalid_twice" {
            std::fs::copy(
                package_dir.join("bad.json"),
                package_dir.join("proposal.json"),
            )
            .unwrap();
        } else if case == "self_approval" {
            for name in ["bad.json", "proposal.json"] {
                let path = package_dir.join(name);
                let mut value: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                value["developer_approval"] = json!({"approved":true,"developer":"owner"});
                write_json(&path, &value);
            }
        } else {
            let path = repo.join("ticket.json");
            let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            value["limits"]["max_attempts"] = json!(3);
            write_json(&path, &value);
        }
        let cat_path = repo.join(".af/task-catalog.toml");
        let mut cat: toml::Value =
            toml::from_str(&std::fs::read_to_string(&cat_path).unwrap()).unwrap();
        package(&repo, &mut cat, "fixture/planner");
        std::fs::write(cat_path, toml::to_string(&cat).unwrap()).unwrap();
        commit(&repo);
        let done = run(
            &repo,
            &state,
            &["task", "start", "--execute", "--file", "ticket.json"],
            4,
        );
        assert_eq!(
            done["attempts"],
            if case == "no_remaining_attempts" {
                1
            } else {
                2
            },
            "{case}"
        );
        assert_eq!(done["result"]["acceptance"], "inconclusive");
        assert_eq!(
            done["result"]["domain_conclusion"],
            if case == "no_remaining_attempts" {
                "planning_admission_failed"
            } else {
                "planning_incomplete"
            }
        );
        assert!(done["plan_decisions"].is_null());
        assert!(
            !done["history"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["transition"]["change"]["kind"] == "planning_completed")
        );
        let replay = run(
            &repo,
            &state,
            &["task", "run", "--execute", "pagination-cli"],
            4,
        );
        assert_eq!(replay["attempts"], done["attempts"]);
        assert_eq!(replay["plan_id"], done["plan_id"]);
    }
}

#[test]
fn signed_rejection_and_revocation_survive_reopen_and_block_generated_execution() {
    for revoked in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (repo, state, key) = setup(root.path(), false, false);
        let waiting = run(
            &repo,
            &state,
            &["task", "start", "--execute", "--file", "ticket.json"],
            0,
        );
        if revoked {
            approve(&repo, &state, root.path(), &key);
        }
        let rejected = decide(&repo, &state, root.path(), &key, true);
        assert_eq!(rejected["attempts"], 1);
        if revoked {
            let event = rejected["history"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["transition"]["change"]["kind"] == "approval_revoked")
                .unwrap();
            let id = event["transition"]["change"]["revocation_id"]
                .as_str()
                .unwrap();
            let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
            let proof = cas.get_json(id).unwrap();
            assert_eq!(proof["payload"]["decision"], "rejected");
            assert_eq!(proof["payload"]["plan_id"], waiting["plan_id"]);
            assert_eq!(
                cas.get_json(proof["payload"]["authorization_id"].as_str().unwrap())
                    .unwrap()["schema"],
                "af.signed-task-authorization/1"
            );
        }
        let out = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args(["task", "run", "--execute", "pagination-cli", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let shown = run(&repo, &state, &["task", "explain", "pagination-cli"], 0);
        assert_eq!(shown["attempts"], 1);
        assert_eq!(shown["plan_id"], waiting["plan_id"]);
    }
}

#[test]
fn preview_confirmation_runs_only_the_bootstrap_and_cannot_approve_generated_work() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state, _) = setup(temp.path(), false, true);
    let preview = run(
        &repo,
        &state,
        &["task", "start", "--file", "ticket.json"],
        0,
    );
    assert_eq!(preview["attempts"], 0);
    assert!(preview["plan"]["preparation"].is_object());
    let generated = run(
        &repo,
        &state,
        &[
            "task",
            "run",
            "pagination-cli",
            "--confirm-plan",
            preview["plan_id"].as_str().unwrap(),
        ],
        0,
    );
    assert_eq!(generated["phase"]["reason"], "needs_plan_review");
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "run",
            "pagination-cli",
            "--confirm-plan",
            generated["plan_id"].as_str().unwrap(),
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "A preview confirmation is not a signed developer decision"
    );
    let after = run(&repo, &state, &["task", "explain", "pagination-cli"], 0);
    assert_eq!(after["attempts"], generated["attempts"]);
    assert_eq!(after["chargeable_tokens"], generated["chargeable_tokens"]);
    assert_eq!(after["phase"]["reason"], "needs_plan_review");
}
