//! The configuration record of design §7.4 (`af/TelemetryConfig@2`), written by the Task
//! runtime and read back from the telemetry directory alone: a record per run, its sections
//! written when the run opens and when its plan resolves, each part bound to the run that owns
//! it, and the texts it names identified at every level that records it.

use std::path::Path;

use review_pipeline::task::{TaskOperatorHost, TaskRuntime, TaskWorkOutput};
use review_pipeline::telemetry::{MachineConfig, row};
use review_store::Cas;
use review_store::store::task::{DeveloperGrant, TaskAuthority};
use review_telemetry::level::Level;
use review_telemetry::record::SealExit;
use review_telemetry::segment::writer::{ConfigOwner, Writer, WriterOptions};
use review_telemetry::subscriber::{TelemetrySubscriber, Typed};
use review_telemetry::view::{self, ConfigRecord, Resolved};

use crate::telemetry_ledger::task_fixture::{Authority, TaskFixture};

/// The digest the command's `.af/af.toml` text is named by.
const AF_TOML: &str = "sha256:4444444444444444444444444444444444444444444444444444444444444444";

/// The command's sections, as `af` hands them to every run it opens.
fn machine() -> MachineConfig {
    MachineConfig {
        version: vec![row("version", "version.version", "0.0.0-fixture", None)],
        configuration: vec![row(
            "environment",
            "telemetry.level",
            "<env>",
            Some("AF_TELEMETRY__LEVEL".into()),
        )],
        policy_files: vec![row("policy", "af.af_toml.digest", AF_TOML, None)],
        providers: vec![row("provider", "registry", "empty", None)],
        harnesses: vec![row("harness", "claude_code.managed_settings", "", None)],
        host: vec![row("host", "host.os", std::env::consts::OS, None)],
        variables: vec![row("variable", "AF_TELEMETRY__LEVEL", "", None)],
        texts: vec![("af_toml", AF_TOML.into(), 12)],
    }
}

/// A host whose operators never run: the runtimes here stop before dispatching anything.
struct NoOperators;

impl TaskOperatorHost for NoOperators {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &review_graph::task::CompiledNode,
        _: &review_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        Err("no operator runs in this fixture".into())
    }
    fn execute(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &review_graph::task::CompiledNode,
        _: Option<&review_store::store::task::execution::PreparedTaskAttempt>,
        _: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        unreachable!("no operator runs in this fixture")
    }
}

/// The fixture's authority, refusing the plan when the runtime checks it: the plan changed
/// since it was admitted.
struct RefusesThePlan;

impl TaskAuthority for RefusesThePlan {
    fn validate_plan(
        &self,
        _: &Cas,
        _: &review_core::task::TaskRevisionV1,
        _: &review_core::task::plan::ExecutionPlanV1,
    ) -> Result<Vec<review_core::task::plan::GeneratedOriginV1>, String> {
        Err("the plan's pipeline changed since it was admitted".into())
    }
    fn authorize_decision(
        &self,
        _: &review_core::task::TaskRevisionV1,
        _: &str,
        _: review_core::task::plan::PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        Err("no generated plans".into())
    }
    fn authorization_current(
        &self,
        _: &review_core::task::plan::PlanDecisionV1,
    ) -> Result<(), String> {
        Err("no generated plans".into())
    }
    fn validate_result(
        &self,
        _: &Cas,
        _: &review_core::task::TaskRevisionV1,
        _: &review_core::task::TaskResultV1,
    ) -> Result<(), String> {
        Err("no acceptance in this fixture".into())
    }
}

/// A writer at `level` into `root`, its subscriber, and the command's sections installed.
fn recording(root: &Path, level: Level) -> (Writer, tracing::Dispatch) {
    let mut options = WriterOptions::new(root, level);
    options.redactor = review_telemetry::redact::Redactor::new();
    let writer = Writer::new(options).unwrap();
    let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
    review_pipeline::telemetry::set_machine_config(Box::new(machine));
    (writer, dispatch)
}

/// End every run span and seal the segment.
fn seal(writer: &Writer, dispatch: &tracing::Dispatch) {
    tracing::dispatcher::with_default(dispatch, review_pipeline::telemetry::end_runs);
    writer.finish(SealExit::Ok).unwrap();
}

/// The configuration record of `run_id` in `root`, as `af telemetry config` assembles it.
fn assembled(root: &Path, run_id: &str) -> ConfigRecord {
    let (index, read, _) = view::open(root).unwrap();
    let Resolved::Run {
        store_id, trace_id, ..
    } = view::resolve(&index, run_id, None).unwrap()
    else {
        panic!("run {run_id} is not recorded in {}", root.display());
    };
    let record = view::config(&index, root, read, (&store_id, run_id, &trace_id), None)
        .unwrap()
        .unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("telemetry-config-v2.json"),
        &serde_json::to_value(&record).unwrap(),
    );
    record
}

fn states(record: &ConfigRecord) -> Vec<(&str, &str)> {
    record
        .sections
        .iter()
        .map(|section| (section.section.as_str(), section.state.as_str()))
        .collect()
}

fn keys(record: &ConfigRecord) -> Vec<&str> {
    record.rows.iter().map(|row| row.key.as_str()).collect()
}

/// Design §7.4 (P3): the configuration record exists for every recorded execution, also one
/// refused while the runtime checks the current plan. Its sections the command knew are
/// written when the run's `af.run` opens; the plan section, never resolved, is `unknown` and
/// the record `incomplete` — never presented as whole — in the document and in the text.
#[test]
fn telemetry_config_recorded_when_the_plan_fails() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let store = tempfile::tempdir_in(temp.path()).unwrap();
    let (writer, dispatch) = recording(&root, Level::Spans);
    let mut fixture = TaskFixture::new(store, |_| {});
    let refused = tracing::dispatcher::with_default(&dispatch, || {
        TaskRuntime::new(
            &mut fixture.store,
            &fixture.cas,
            fixture.lease.clone(),
            &RefusesThePlan,
            &NoOperators,
        )
        .err()
    });
    let refusal = refused.expect("the runtime refuses the changed plan");
    assert!(refusal.contains("pipeline changed"), "{refusal}");
    seal(&writer, &dispatch);
    let record = assembled(&root, &fixture.run_id);
    assert_eq!(record.state, "incomplete");
    assert_eq!(
        states(&record),
        [
            ("configuration", "complete"),
            ("plan", "unknown"),
            ("machine", "complete")
        ]
    );
    let keys = keys(&record);
    for key in [
        "version.version",
        "telemetry.level",
        "af.af_toml.digest",
        "host.os",
        "AF_TELEMETRY__LEVEL",
    ] {
        assert!(keys.contains(&key), "{key}: {keys:?}");
    }
    assert!(!keys.contains(&"af.policy_rev"), "{keys:?}");
    let text = crate::telemetry_ledger::af(&root, &["telemetry", "config", &fixture.run_id]);
    assert!(text.status.success());
    let printed = String::from_utf8_lossy(&text.stdout);
    assert!(printed.contains("incomplete"), "{printed}");
    assert!(printed.contains("section plan: unknown"), "{printed}");
}

/// Design §7.4 (P3): at `spans` — not only at `content` — the record names the texts it
/// identifies by digest and size: the pipeline and the Task file when the plan resolves, and
/// `.af/af.toml` when the run opens. The texts' bytes are never written at `spans`.
#[test]
fn telemetry_config_text_identities_at_spans() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let store = tempfile::tempdir_in(temp.path()).unwrap();
    let (writer, dispatch) = recording(&root, Level::Spans);
    let mut fixture = TaskFixture::new(store, |_| {});
    tracing::dispatcher::with_default(&dispatch, || {
        let runtime = TaskRuntime::new(
            &mut fixture.store,
            &fixture.cas,
            fixture.lease.clone(),
            &Authority,
            &NoOperators,
        )
        .unwrap();
        drop(runtime);
    });
    seal(&writer, &dispatch);
    let record = assembled(&root, &fixture.run_id);
    assert_eq!(record.state, "complete", "{record:?}");
    let texts: Vec<(&str, bool, bool)> = record
        .texts
        .iter()
        .map(|text| {
            (
                text.kind.as_str(),
                text.digest
                    .as_deref()
                    .is_some_and(|digest| digest.starts_with("sha256:")),
                text.bytes.is_some_and(|bytes| bytes > 0),
            )
        })
        .collect();
    for kind in ["pipeline", "task_file", "af_toml"] {
        assert!(
            texts.contains(&(kind, true, true)),
            "{kind} named by digest and size: {texts:?}"
        );
    }
    // Nothing but metadata: no blob was written at `spans`.
    assert!(!root.join("v1/blobs").exists() || dir_is_empty(&root.join("v1/blobs")));
}

fn dir_is_empty(path: &Path) -> bool {
    std::fs::read_dir(path).unwrap().next().is_none()
}

/// Design §7.4 and §7.7 (P3): a measurement run inside a Task Attempt has a configuration
/// record of its own, never the Task's. Both runs live in one execution (one segment): each
/// part names the run that owns it — its run id and its `af.run` span — the writer's one-time
/// guard is keyed by segment and run, and `af telemetry config RUN` assembles by run identity
/// and execution, so the measurement's record carries what it measures and the Task's carries
/// the Task's plan, neither the other's.
#[test]
fn telemetry_config_of_a_measurement_run_is_its_own() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let (writer, dispatch) = recording(&root, Level::Spans);
    let store_id = "5".repeat(64);
    let task_id = "measured";
    let task_run = review_store::store::task::task_run_id(task_id).unwrap();
    let attempt = "01jd8m4qz9k7v3n2p6r8t0w2a1";
    let measurement_run = tracing::dispatcher::with_default(&dispatch, || {
        // The command's span opens the segment the run's spans name.
        let command = tracing::info_span!("af.command", process.command = "af");
        let _command = command.enter();
        let run = tracing::info_span!(
            "af.run",
            af.store_id = Typed::identity(store_id.clone()).unwrap().field(),
            af.run_id = Typed::identity(task_run.clone()).unwrap().field(),
            af.run.kind = Typed::enumerated("task").field(),
            af.execution_id = Typed::identity(writer.segment_id().unwrap())
                .unwrap()
                .field(),
        );
        let _run = run.enter();
        // The Task run's own record, as the runtime writes it for its plan.
        let run_span = review_telemetry::subscriber::recorded(&run)
            .unwrap()
            .span_id;
        let segment = writer.segment_id().unwrap();
        writer.config(
            &ConfigOwner {
                execution_id: &segment,
                run_id: &task_run,
                run_span_id: &run_span,
            },
            "plan",
            vec![row("policy", "af.task_id", task_id, None)],
        );
        let attempt_span = tracing::info_span!("af.attempt", af.attempt_id = attempt);
        let _attempt = attempt_span.enter();
        let measured = review_pipeline::telemetry::measurement(task_id, attempt, "bench");
        let run_id = review_telemetry::subscriber::recorded(&measured)
            .and_then(|recorded| recorded.trace_id)
            .unwrap();
        drop(measured);
        run_id
    });
    seal(&writer, &dispatch);
    let owners = config_owners(&root);
    let measurement_runs: std::collections::BTreeSet<&str> = owners
        .iter()
        .map(|(run, _, _)| run.as_str())
        .filter(|run| *run != task_run)
        .collect();
    let [measured] = measurement_runs.into_iter().collect::<Vec<_>>()[..] else {
        panic!("one measurement run owns parts: {owners:?}");
    };
    assert_eq!(
        review_telemetry::trace::trace_id(&store_id, measured),
        measurement_run
    );
    // The measurement's record: the command's sections and what it measures, complete.
    let own = assembled(&root, measured);
    assert_eq!(own.state, "complete", "{own:?}");
    let own_keys = keys(&own);
    for key in ["version.version", "af.measure", "af.attempt_id", "host.os"] {
        assert!(own_keys.contains(&key), "{key}: {own_keys:?}");
    }
    assert!(
        own.rows
            .iter()
            .any(|row| row.key == "af.run.kind" && row.value == "measure")
    );
    // The Task's: its own plan section, none of the measurement's rows.
    let task = assembled(&root, &task_run);
    assert_ne!(task.run_span_id, own.run_span_id);
    assert_eq!(task.execution_id, own.execution_id, "one execution");
    assert!(keys(&task).contains(&"af.task_id"));
    assert!(!keys(&task).contains(&"af.measure"), "{:?}", keys(&task));
    // The command prints the measurement's own record.
    let printed = crate::telemetry_ledger::af(&root, &["telemetry", "config", measured, "--json"]);
    assert!(printed.status.success());
    let document: serde_json::Value = serde_json::from_slice(&printed.stdout).unwrap();
    assert_eq!(document["run_id"], measured);
    assert_eq!(document["run_span_id"], own.run_span_id.as_str());
}

/// Who owns each `config` part the index holds: its run id, its run span and its section.
fn config_owners(root: &Path) -> std::collections::BTreeSet<(String, String, String)> {
    let (index, _, _) = view::open(root).unwrap();
    let mut statement = index
        .connection()
        .prepare("SELECT run_id, run_span_id, section FROM configs")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
