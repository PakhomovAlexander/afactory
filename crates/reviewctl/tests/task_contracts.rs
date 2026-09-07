//! The Task log contract (ADR-0046): a closed event vocabulary identical in Rust and in
//! `schemas/task-event-v1.json`, a payload schema every persisted Task record satisfies, a
//! derived-Snapshot ceiling that refuses build output before publishing it, and an evaluator
//! context that stays bounded however much the implementer touched.

use std::path::{Path, PathBuf};
use std::process::Command;

use review_config::lock::package_digest;
use review_core::event::TaskEventType;
use review_store::Cas;
use serde_json::{Value, json};

const TASK_SCHEMAS: [&str; 9] = [
    "task-event-v1.json",
    "task-snapshot-v1.json",
    "task-opened-v1.json",
    "task-worker-evidence-v1.json",
    "task-derived-snapshot-v1.json",
    "task-evaluation-v1.json",
    "task-outcome-v1.json",
    "task-delivery-prepared-v1.json",
    "task-delivery-v1.json",
];

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn schema(name: &str) -> Value {
    let path = workspace_root().join("schemas").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}")))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn validator(name: &str) -> jsonschema::Validator {
    let mut options = jsonschema::options();
    for resource in TASK_SCHEMAS.iter().chain(["check-result-v1.json"].iter()) {
        let value = schema(resource);
        let id = value["$id"].as_str().unwrap().to_string();
        options.with_resource(
            id,
            jsonschema::Resource::from_contents(value).expect("schema resource"),
        );
    }
    options
        .build(&schema(name))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn assert_valid(name: &str, instance: &Value) {
    let validator = validator(name);
    if !validator.is_valid(instance) {
        let errors: Vec<String> = validator
            .iter_errors(instance)
            .map(|e| format!("{} at {}", e, e.instance_path))
            .collect();
        panic!(
            "{name} rejected a value it must accept: {}\n{instance:#}",
            errors.join("; ")
        );
    }
}

fn assert_invalid(name: &str, instance: &Value, why: &str) {
    assert!(
        !validator(name).is_valid(instance),
        "{name} accepted a value it must reject ({why})"
    );
}

/// The payload schema an event's artifact must satisfy, mirrored from `TaskEventType::artifact_schema`.
fn payload_schema(event_type: TaskEventType) -> &'static str {
    match event_type {
        TaskEventType::TaskOpenedV1 => "task-opened-v1.json",
        TaskEventType::WorkerCompletedV1 => "task-worker-evidence-v1.json",
        TaskEventType::SnapshotDerivedV1 => "task-derived-snapshot-v1.json",
        TaskEventType::GateCompletedV1 => "check-result-v1.json",
        TaskEventType::EvaluationCompletedV1 => "task-evaluation-v1.json",
        TaskEventType::TaskCompletedV1 => "task-outcome-v1.json",
        TaskEventType::TaskDeliveryPreparedV1 => "task-delivery-prepared-v1.json",
        TaskEventType::TaskDeliveredV1 | TaskEventType::TaskDeliveryFailedV1 => {
            "task-delivery-v1.json"
        }
    }
}

fn git(repo: &Path, home: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_package(root: &Path, name: &str, script: &str, prompt: &str) {
    let package = root.join(name);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        format!(
            "name = \"{name}\"\nversion = \"1.0.0\"\nsubjects = [\"whole-tree\"]\n\n\
             [runner]\nprogram = \"/bin/sh\"\nargs = [{{ value = \"-c\" }}, {{ value = '''{script}''' }}]\n"
        ),
    )
    .unwrap();
    std::fs::write(package.join("reviewer.md"), prompt).unwrap();
}

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

/// A committed repository whose implementer runs `implementer_script` in its sandbox, whose one
/// acceptance gate checks `implemented.txt`, and whose evaluator approves.
fn fixture(root: &Path, implementer_script: &str) -> Fixture {
    fixture_with(root, implementer_script, APPROVING_EVALUATOR, 60)
}

/// [`fixture`] with the evaluator and the Worker deadline chosen by the caller — the two knobs a
/// Worker that runs, costs tokens, and then fails needs.
fn fixture_with(
    root: &Path,
    implementer_script: &str,
    evaluator_script: &str,
    worker_timeout_seconds: u64,
) -> Fixture {
    let repo = root.join("repo");
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
    std::fs::create_dir_all(repo.join(".af/workers")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(repo.join("seed.txt"), "source\n").unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[project]\nname = \"fixture\"\nmin_af = \"0.6\"\n[defaults]\npipeline = \"review\"\ntask_pipeline = \"implement\"\n",
    )
    .unwrap();
    write_package(
        &repo.join(".af/workers"),
        "implementer",
        implementer_script,
        "Implement the exact goal in the sandbox.",
    );
    write_package(
        &repo.join(".af/workers"),
        "evaluator",
        evaluator_script,
        "Evaluate the goal against the provided Snapshot. Return only the requested JSON.",
    );
    let pipeline = format!(
        "version = 1\nkind = \"implement\"\nimplementer = \"implementer\"\n\
         evaluator = \"evaluator\"\ntimeout_seconds = {worker_timeout_seconds}\ncheck_timeout_seconds = 60\n\n\
         attempt_tokens = 1000\nrun_tokens = 2000\n\n[[checks]]\nname = \"acceptance\"\nprogram = \"/bin/sh\"\n\
         args = [{{ value = \"-c\" }}, {{ value = '''test \"$(cat implemented.txt)\" = derived''' }}]\n"
    );
    let pipeline = pipeline.as_str();
    std::fs::write(repo.join(".af/pipelines/implement.toml"), pipeline).unwrap();
    let implementer = package_digest("implementer", &repo.join(".af/workers/implementer")).unwrap();
    let evaluator = package_digest("evaluator", &repo.join(".af/workers/evaluator")).unwrap();
    let pipeline_digest = review_store::canonical::blob_content_id(pipeline.as_bytes());
    std::fs::write(
        repo.join(".af/af.lock"),
        format!(
            "version = 1\n\n[workers.implementer]\nversion = \"1.0.0\"\ndigest = \"{implementer}\"\n\n\
             [workers.evaluator]\nversion = \"1.0.0\"\ndigest = \"{evaluator}\"\n\n\
             [pipelines.implement]\nversion = \"1.0.0\"\ndigest = \"{pipeline_digest}\"\n"
        ),
    )
    .unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "t@t.invalid"]);
    git(&repo, &home, &["config", "user.name", "T"]);
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "initial"]);
    Fixture { repo, home, state }
}

fn run_af(fixture: &Fixture, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn run_task(fixture: &Fixture) -> (i32, Value, String) {
    let (code, stdout, stderr) = run_af(
        fixture,
        &[
            "task",
            "start",
            "--kind",
            "implement",
            "--goal",
            "create implemented.txt",
            "--authority",
            "HEAD",
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    let outcome = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("{error}: stdout={stdout} stderr={stderr}"));
    (code, outcome, stderr)
}

fn task_events(fixture: &Fixture, task_id: &str) -> Vec<(u64, String, String)> {
    let connection = rusqlite::Connection::open(fixture.state.join("tasks.sqlite")).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT sequence, event_type, artifact_id FROM task_events
             WHERE task_id = ?1 ORDER BY sequence",
        )
        .unwrap();
    statement
        .query_map([task_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn evaluator_task_input(cas: &Cas, outcome: &Value) -> Value {
    let evaluator = &outcome["workers"][1];
    assert_eq!(evaluator["role"], "evaluator");
    let task_input = evaluator["context_manifest"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "task_input")
        .unwrap();
    serde_json::from_slice(
        &cas.get(task_input["artifact_id"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap()
}

const SIMPLE_IMPLEMENTER: &str = "printf 'derived\\n' > implemented.txt; printf done";
const APPROVING_EVALUATOR: &str = r#"printf '{"verdict":"approve","summary":"independent pass"}'"#;

#[test]
fn task_event_schema_and_rust_vocabulary_are_identical() {
    let schema = schema("task-event-v1.json");
    let declared = schema["properties"]["event_type"]["enum"].clone();
    let rust: Vec<Value> = TaskEventType::ALL
        .iter()
        .map(|event_type| Value::String(event_type.as_str().to_string()))
        .collect();
    assert_eq!(declared, Value::Array(rust));
    for event_type in TaskEventType::ALL {
        assert_eq!(
            event_type.as_str().parse::<TaskEventType>().unwrap(),
            event_type
        );
        assert_eq!(
            serde_json::to_value(event_type).unwrap(),
            event_type.as_str()
        );
        assert_eq!(
            serde_json::from_value::<TaskEventType>(json!(event_type.as_str())).unwrap(),
            event_type
        );
        let payload = payload_schema(event_type);
        match event_type.artifact_schema() {
            Some(marker) => {
                let title = schema_file_title(payload);
                assert!(
                    title.starts_with(marker),
                    "{event_type}: {payload} is titled {title:?}, not `{marker}`"
                );
            }
            None => assert_eq!(payload, "check-result-v1.json"),
        }
    }
    assert!("TaskForked@1".parse::<TaskEventType>().is_err());
}

fn schema_file_title(name: &str) -> String {
    schema(name)["title"].as_str().unwrap().to_string()
}

#[test]
fn every_task_schema_is_a_valid_json_schema() {
    for name in TASK_SCHEMAS {
        let _ = validator(name);
    }
}

#[test]
fn task_schemas_reject_what_the_contract_forbids() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let envelope = json!({
        "task_id": "task-0123456789abcdef0123",
        "sequence": 1,
        "event_type": "TaskOpened@1",
        "artifact_id": digest,
    });
    assert_valid("task-event-v1.json", &envelope);
    let mut unknown = envelope.clone();
    unknown["event_type"] = json!("TaskForked@1");
    assert_invalid(
        "task-event-v1.json",
        &unknown,
        "a type outside the vocabulary",
    );
    let mut zero = envelope.clone();
    zero["sequence"] = json!(0);
    assert_invalid("task-event-v1.json", &zero, "sequences start at 1");
    let mut bare = envelope.clone();
    bare["artifact_id"] = json!("not-a-digest");
    assert_invalid(
        "task-event-v1.json",
        &bare,
        "artifact references are digests",
    );
    let mut inline = envelope;
    inline["payload"] = json!({"inline": true});
    assert_invalid(
        "task-event-v1.json",
        &inline,
        "a Task event carries no inline payload",
    );

    let evaluation =
        json!({"schema": "af/task-evaluation@1", "verdict": "approve", "summary": "ok"});
    assert_valid("task-evaluation-v1.json", &evaluation);
    let mut maybe = evaluation.clone();
    maybe["verdict"] = json!("maybe");
    assert_invalid(
        "task-evaluation-v1.json",
        &maybe,
        "a verdict is approve or reject",
    );
    let mut transcript = evaluation;
    transcript["transcript"] = json!("...");
    assert_invalid(
        "task-evaluation-v1.json",
        &transcript,
        "no undeclared field",
    );

    let receipt = json!({
        "schema": "af/task-delivery@1",
        "delivery_id": format!("delivery-{}", "b".repeat(64)),
        "task_id": "task-0123456789abcdef0123",
        "source_snapshot_id": digest,
        "derived_snapshot_id": digest,
        "target": {"repository": "/r", "repository_id": "id", "branch": "b", "worktree": "/w"},
        "outcome": {"kind": "delivered"},
        "ignored_paths": [],
        "remote_actions": [],
    });
    assert_valid("task-delivery-v1.json", &receipt);
    let mut pushed = receipt.clone();
    pushed["remote_actions"] = json!(["push"]);
    assert_invalid(
        "task-delivery-v1.json",
        &pushed,
        "delivery never acts remotely",
    );
    let mut unexplained = receipt;
    unexplained["outcome"] = json!({"kind": "failed"});
    assert_invalid(
        "task-delivery-v1.json",
        &unexplained,
        "a failure names its reason",
    );
}

#[test]
fn every_persisted_task_record_matches_its_schema() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = fixture(directory.path(), SIMPLE_IMPLEMENTER);
    let (code, outcome, stderr) = run_task(&fixture);
    assert_eq!(code, 0, "{stderr}");
    let task_id = outcome["task_id"].as_str().unwrap().to_string();
    let worktree = directory.path().join("delivered");
    let (code, _, stderr) = run_af(
        &fixture,
        &[
            "task",
            "deliver",
            &task_id,
            "--repo",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "af/contract",
            "--worktree",
            worktree.to_str().unwrap(),
            "--confirm",
            &task_id,
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{stderr}");

    let cas = Cas::open(fixture.state.join("cas")).unwrap();
    let events = task_events(&fixture, &task_id);
    let types: Vec<&str> = events.iter().map(|(_, kind, _)| kind.as_str()).collect();
    assert_eq!(
        types,
        [
            "TaskOpened@1",
            "WorkerCompleted@1",
            "SnapshotDerived@1",
            "GateCompleted@1",
            "WorkerCompleted@1",
            "EvaluationCompleted@1",
            "TaskCompleted@1",
            "TaskDeliveryPrepared@1",
            "TaskDelivered@1",
        ]
    );
    for (index, (sequence, kind, artifact_id)) in events.iter().enumerate() {
        assert_eq!(*sequence, index as u64 + 1, "dense, gapless sequence");
        let envelope = json!({
            "task_id": task_id,
            "sequence": sequence,
            "event_type": kind,
            "artifact_id": artifact_id,
        });
        assert_valid("task-event-v1.json", &envelope);
        let event_type: TaskEventType = kind.parse().unwrap();
        let artifact = cas.get_json(artifact_id).unwrap();
        if let Some(marker) = event_type.artifact_schema() {
            assert_eq!(artifact["schema"], marker, "{kind} artifact marker");
        }
        assert_valid(payload_schema(event_type), &artifact);
    }

    let source = cas
        .get_json(outcome["source_snapshot_id"].as_str().unwrap())
        .unwrap();
    assert_valid("task-snapshot-v1.json", &source);
    assert_eq!(source["kind"], "source");
    assert!(source.get("source_revision").is_some());
    let derived = cas
        .get_json(outcome["derived_snapshot_id"].as_str().unwrap())
        .unwrap();
    assert_valid("task-snapshot-v1.json", &derived);
    assert_eq!(derived["kind"], "derived");
    assert_eq!(derived["parent_snapshot_id"], outcome["source_snapshot_id"]);

    // The receipt measures the derived Snapshot instead of leaving its size to be discovered.
    let size = &outcome["derived_snapshot_size"];
    assert_eq!(size["mutated_entries"], 1, "{size}");
    assert_eq!(size["mutated_bytes"], "derived\n".len());
    assert!(size["entries"].as_u64().unwrap() > 1);
    assert!(size["bytes"].as_u64().unwrap() > size["mutated_bytes"].as_u64().unwrap());
    let snapshot_record = cas.get_json(&events[2].2).unwrap();
    assert_eq!(snapshot_record["size"], *size);
    assert_eq!(
        snapshot_record["mutations"]["added"],
        json!(["implemented.txt"])
    );
}

#[test]
fn derived_snapshot_ceiling_refuses_build_output_before_publishing_it() {
    let directory = tempfile::tempdir().unwrap();
    // One more added entry than MAX_DERIVED_MUTATION_ENTRIES_V1 admits, shaped like a compiler
    // cache. Empty files: the seal scan never reads added bytes, and the ceiling must not either.
    let script = "printf 'derived\\n' > implemented.txt; mkdir -p target/debug; i=0; \
                  while [ $i -lt 4096 ]; do : > target/debug/unit-$i.o; i=$((i+1)); done; printf done";
    let fixture = fixture(directory.path(), script);
    let (code, outcome, stderr) = run_task(&fixture);
    assert_eq!(code, 3, "{stderr}");
    assert_eq!(outcome["outcome"]["kind"], "unverified");
    assert_eq!(outcome["outcome"]["stage"], "snapshot");
    let reason = outcome["outcome"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("MAX_DERIVED_MUTATION_ENTRIES_V1 = 4096"),
        "refusal must name the limit: {reason}"
    );
    assert!(
        reason.contains("4097 added or modified entries"),
        "{reason}"
    );
    assert_eq!(outcome["derived_snapshot_id"], Value::Null);
    assert_eq!(outcome["derived_snapshot_size"], Value::Null);
    assert_eq!(outcome["workers"].as_array().unwrap().len(), 1);
    assert!(outcome["gates"].as_array().unwrap().is_empty());
    assert_valid("task-outcome-v1.json", &outcome);
    let task_id = outcome["task_id"].as_str().unwrap();
    let types: Vec<String> = task_events(&fixture, task_id)
        .into_iter()
        .map(|(_, kind, _)| kind)
        .collect();
    assert_eq!(
        types,
        ["TaskOpened@1", "WorkerCompleted@1", "TaskCompleted@1"],
        "nothing of the refused tree was published"
    );
}

#[test]
fn evaluator_input_carries_a_bounded_mutation_summary() {
    let directory = tempfile::tempdir().unwrap();
    let script = "printf 'derived\\n' > implemented.txt; mkdir -p generated; i=0; \
                  while [ $i -lt 300 ]; do printf 'g\\n' > generated/case-$i.txt; i=$((i+1)); done; printf done";
    let fixture = fixture(directory.path(), script);
    let (code, outcome, stderr) = run_task(&fixture);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(outcome["outcome"]["kind"], "verified");
    assert_eq!(outcome["derived_snapshot_size"]["mutated_entries"], 301);

    let cas = Cas::open(fixture.state.join("cas")).unwrap();
    let input = evaluator_task_input(&cas, &outcome);
    let mutations = &input["mutations"];
    assert_eq!(mutations["count"], 301);
    assert_eq!(mutations["added"], 301);
    assert_eq!(mutations["modified"], 0);
    assert_eq!(mutations["deleted"], 0);
    assert_eq!(mutations["truncated"], true);
    let sample = mutations["sample"].as_array().unwrap();
    assert_eq!(sample.len(), 20);
    assert_eq!(sample[0], "generated/case-0.txt");
    let rendered = serde_json::to_string_pretty(&input).unwrap();
    assert!(
        rendered.len() < 4 * 1024,
        "evaluator input scaled with the mutation set: {} bytes",
        rendered.len()
    );
    let task_input_entry = outcome["workers"][1]["context_manifest"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "task_input")
        .unwrap();
    assert!(task_input_entry["rendered_bytes"].as_u64().unwrap() < 4 * 1024);

    // The complete list is durable exactly once, in the artifact the summary names.
    let record = cas
        .get_json(mutations["artifact"].as_str().unwrap())
        .unwrap();
    assert_eq!(record["schema"], "af/derived-snapshot@1");
    assert_eq!(record["snapshot_id"], outcome["derived_snapshot_id"]);
    assert_eq!(record["mutations"]["added"].as_array().unwrap().len(), 301);
    assert!(input.get("implementer_output").is_none());
    assert!(input.get("implementer_transcript").is_none());
}

/// A Worker that reached a provider and spent tokens leaves a durable record, whatever failed
/// afterwards. A deadline and the reviewer-output ceiling both publish the Attempt's raw output
/// before returning an error, so the Task must name that Worker and reference those bytes —
/// not report `workers: []` and `chargeable_tokens: 0` while the artifact sits unreferenced.
#[test]
fn a_worker_that_ran_is_recorded_even_when_its_attempt_failed() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = fixture_with(
        directory.path(),
        "printf 'partial answer'; sleep 30",
        APPROVING_EVALUATOR,
        1,
    );
    let (code, outcome, stderr) = run_task(&fixture);
    assert_eq!(code, 3, "{stderr}");
    assert_eq!(outcome["outcome"]["kind"], "unverified");
    assert_eq!(outcome["outcome"]["stage"], "implementer");
    let workers = outcome["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 1, "{outcome:#}");
    assert_eq!(workers[0]["role"], "implementer");
    assert_valid("task-outcome-v1.json", &outcome);

    let task_id = outcome["task_id"].as_str().unwrap();
    let events = task_events(&fixture, task_id);
    let types: Vec<&str> = events.iter().map(|(_, kind, _)| kind.as_str()).collect();
    assert_eq!(
        types,
        ["TaskOpened@1", "WorkerCompleted@1", "TaskCompleted@1"],
        "the outcome names a Worker the log never recorded"
    );

    // The evidence is the Attempt's own published stdout, not an invention.
    let cas = Cas::open(fixture.state.join("cas")).unwrap();
    let evidence = cas.get_json(&events[1].2).unwrap();
    assert_valid("task-worker-evidence-v1.json", &evidence);
    assert_eq!(evidence["raw_artifact"], workers[0]["raw_artifact"]);
    assert_eq!(
        cas.get(evidence["raw_artifact"].as_str().unwrap()).unwrap(),
        b"partial answer"
    );
}

/// An evaluator that mutates its read-only Snapshot still ran and still cost tokens. Its
/// evidence is already in the terminal record's `workers` and totals, so it has to be in the
/// log too: the log is the ordering authority (ADR-0046), and a Task reconstructed from it must
/// not disagree with the outcome about which Workers existed.
#[test]
fn a_mutating_evaluator_is_logged_before_its_snapshot_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = fixture_with(
        directory.path(),
        SIMPLE_IMPLEMENTER,
        // Read-only is permission bits, and the owner may lift them: exactly the tamper the
        // seal comparison exists to catch.
        r#"chmod u+w . seed.txt; printf 'tampered\n' >> seed.txt; printf '{"verdict":"approve","summary":"x"}'"#,
        60,
    );
    let (code, outcome, stderr) = run_task(&fixture);
    assert_eq!(code, 3, "{stderr}");
    assert_eq!(outcome["outcome"]["stage"], "evaluator");
    assert_eq!(
        outcome["outcome"]["reason"],
        "evaluator mutated its read-only Snapshot"
    );
    assert_eq!(outcome["workers"].as_array().unwrap().len(), 2);
    assert_valid("task-outcome-v1.json", &outcome);

    let task_id = outcome["task_id"].as_str().unwrap();
    let events = task_events(&fixture, task_id);
    let types: Vec<&str> = events.iter().map(|(_, kind, _)| kind.as_str()).collect();
    assert_eq!(
        types,
        [
            "TaskOpened@1",
            "WorkerCompleted@1",
            "SnapshotDerived@1",
            "GateCompleted@1",
            "WorkerCompleted@1",
            "TaskCompleted@1",
        ],
        "the outcome names two Workers; the log must too"
    );
    let cas = Cas::open(fixture.state.join("cas")).unwrap();
    let evidence = cas.get_json(&events[4].2).unwrap();
    assert_eq!(evidence["role"], "evaluator");
    assert_valid("task-worker-evidence-v1.json", &evidence);
}
