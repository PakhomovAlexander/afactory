//! Real captured Review execution and fresh-process inspection of owned Task history.
use super::*;
use review_core::task::execution::{TASK_EXECUTION_RECORD_V4, TaskInvocationV1, TaskOutputV1};
use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_core::task::{TaskLimitsV1, TaskResultV1, TaskRevisionV1, VerificationReserveV1};
use review_graph::task::{Address, OperatorAttemptCost};
use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper, TaskDomain};
use review_pipeline::task::legacy_review::{
    CapturedLegacyReviewRound,
    host::LegacyReviewTaskHost,
    plan::{LegacyReviewPlanCompiler, ReviewPlanSettings, ReviewPlanSettingsV2},
};
use review_pipeline::task::{TaskOperatorHost, TaskRuntime, TaskWorkOutput};
use review_store::store::task::execution::PreparedTaskAttempt;
use review_store::{Cas, EventStore, SharedEventStore};
use std::collections::BTreeMap;

const TASK: &str = "owned-inspection";
struct AdmissionOnly;
impl TaskOperatorHost for AdmissionOnly {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
    ) -> Result<String, String> {
        panic!("admission rendered context")
    }
    fn execute(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        panic!("admission executed work")
    }
}
impl TaskDomain for AdmissionOnly {
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        Err("admission only".into())
    }
    fn validate_output(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &TaskInvocationV1,
        _: &TaskOutputV1,
    ) -> Result<(), String> {
        Err("admission only".into())
    }
    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("admission only".into())
    }
}
fn artifact(cas: &Cas, kind: &str, value: &impl serde::Serialize) -> String {
    cas.put_artifact(
        kind,
        review_core::Producer::KernelOperation {
            run_id: "owned-inspection-fixture".into(),
            node_id: None,
            operation_id: "capture@1".into(),
        },
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}

struct MissingSecond<'a> {
    inner: &'a (dyn review_graph::Dispatch + Sync),
}
impl review_graph::Dispatch for MissingSecond<'_> {
    fn coordinates_owned_children(&self, node: &review_graph::Node) -> bool {
        self.inner.coordinates_owned_children(node)
    }
    fn expand_owned_children(
        &self,
        node: &review_graph::Node,
        inputs: &review_graph::ArtifactMap,
    ) -> Result<Vec<review_graph::OwnedChildDispatch>, String> {
        self.inner.expand_owned_children(node, inputs)
    }
    fn complete_owned_children(
        &self,
        node: &review_graph::Node,
        inputs: &review_graph::ArtifactMap,
        children: &[(String, review_graph::NodeOutcome)],
    ) -> Result<review_graph::ArtifactMap, String> {
        self.inner.complete_owned_children(node, inputs, children)
    }
    fn requires_successful_predecessors(&self, node: &review_graph::Node) -> bool {
        self.inner.requires_successful_predecessors(node)
    }
    fn task_node_selected(
        &self,
        node: &review_graph::Node,
        inputs: &review_graph::ArtifactMap,
    ) -> Result<bool, String> {
        self.inner.task_node_selected(node, inputs)
    }
    fn record_invocation(
        &self,
        node: &review_graph::Node,
        inputs: &review_graph::ArtifactMap,
    ) -> Result<(), String> {
        if node.id.ends_with(".slice2") {
            return Err("fixture lost admission before the second child invocation".into());
        }
        self.inner.record_invocation(node, inputs)
    }
    fn run(
        &self,
        node: &review_graph::Node,
        inputs: &review_graph::ArtifactMap,
    ) -> Result<review_graph::ArtifactMap, String> {
        self.inner.run(node, inputs)
    }
    fn record_outputs(
        &self,
        node: &review_graph::Node,
        outputs: &review_graph::ArtifactMap,
    ) -> Result<(), String> {
        self.inner.record_outputs(node, outputs)
    }
    fn failure_class(&self, node: &str) -> Option<review_graph::NodeFailureClass> {
        self.inner.failure_class(node)
    }
}

#[test]
fn owned_inspection_reopens_typed_membership_failed_and_missing_children_with_frozen_normal_control()
 {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let planned = json_output(
        cli(&repo, &state, &["task", "plan", "--file", "ticket.json"]),
        0,
    );
    valid(&validator("task-inspection-v3.json"), &planned);
    let normal = json_output(cli(&repo, &state, &["task", "show", "pagination-cli"]), 0);
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let mut store = EventStore::open(state.join("events.sqlite")).unwrap();
    let definition=include_str!("../../../review-config/tests/fixtures/dynamic-v5.toml")
        .replace("[budgets]\nunit = \"tokens\"\nattempt = 100\nfan_out = 200\nrun = 400\n","")
        .replace("runner = { program = \"/bin/true\" }","runner = { program = \"/bin/sh\", args = [{value=\"-c\"},{value=\"cat >/dev/null; exit 1\"}] }");
    let round = captured_fixture::open_round_authority(&cas, &mut store, &definition, None);
    let settings = ReviewPlanSettingsV2 {
        review: ReviewPlanSettings {
            mode: "light".into(),
            resources: review_config::task::legacy_review::resources::ReviewResourcePolicy {
                uncapped_attempt_tokens: 1,
            },
            outputs: BTreeMap::from([(
                "findings".into(),
                Address {
                    node: "ledger".into(),
                    port: "findings".into(),
                },
            )]),
            executions: BTreeMap::from([
                ("scatter".into(), WorkerExecutionV1::Command {}),
                ("closeout".into(), WorkerExecutionV1::Command {}),
            ]),
            provider_admission: OperatorAttemptCost {
                tokens: 1,
                wall_ms: 1000,
            },
            allowed_effects: Default::default(),
        },
        provider_probes: BTreeMap::new(),
    };
    let compiler = LegacyReviewPlanCompiler::capture(
        &cas,
        CapturedLegacyReviewRound::load(&cas, &store, "review", &round).unwrap(),
        cas.put(b"owned public inspection fixture").unwrap(),
        settings,
    )
    .unwrap();
    let task = compiler
        .prepare_revision(
            &cas,
            TASK,
            TaskLimitsV1 {
                tokens: 100,
                max_attempts: 4,
                deadline_unix_ms: 9999999999999,
                verification: VerificationReserveV1 {
                    tokens: 0,
                    attempts: 0,
                    wall_ms: 0,
                },
            },
        )
        .unwrap();
    let revision = artifact(&cas, review_core::task::TASK_REVISION_V1, &task);
    let (plan, _) = compiler.compile(&cas, &revision).unwrap();
    let plan_id = artifact(&cas, review_core::task::EXECUTION_PLAN_V1, &plan);
    let authority =
        CapturedTaskAuthority::for_legacy_review(&compiler, &AdmissionOnly, &NoTaskDeveloper);
    let lease = store.open_task(&cas, &revision, "fixture", 60_000).unwrap();
    store
        .propose_task_plan(&cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(&cas, &lease, &authority).unwrap();
    let (set_id, set, parent_output, shards, expected_attempts, before) = {
        let shared = SharedEventStore::new(&mut store);
        let host = LegacyReviewTaskHost::new(
            &cas,
            shared.clone(),
            &compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let authority =
            CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
        let runtime =
            TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host)
                .unwrap();
        // Inject a lost admission before child2's common invocation. Every other callback,
        // including exact registry publication and final fold, uses the real TaskRuntime.
        let graph: CompiledTask =
            serde_json::from_value(cas.get_artifact(&plan.compiled_graph_id).unwrap().payload)
                .unwrap();
        let report = graph.run(&MissingSecond { inner: &runtime }).unwrap();
        assert!(!report.complete());
        let projection = runtime.projection().unwrap();
        let execution = projection.execution.unwrap();
        let parent = execution.graph.owned_children.keys().next().unwrap();
        let locked = shared.lock().unwrap();
        let registration = locked
            .get_task_owned_children(&cas, TASK, &execution.invocations[parent].0)
            .unwrap()
            .unwrap();
        let facts = locked
            .task_owned_child_evidence(&cas, &registration)
            .unwrap();
        assert_eq!(facts.len(), 2);
        assert!(
            facts[0].attempts.iter().any(|attempt| attempt.started),
            "facts={facts:?}; report={report:?}"
        );
        assert!(
            facts[1].attempts.iter().all(|attempt| !attempt.started),
            "{facts:?}"
        );
        let parent_output = &execution.outputs[parent];
        let shards: review_core::ShardSetV1 = serde_json::from_value(
            cas.get_artifact(&parent_output.1.outputs["o0"].artifact_ids[0])
                .unwrap()
                .payload,
        )
        .unwrap();
        assert!(matches!(
            shards.shards[0].outcome,
            review_core::ShardOutcomeV1::Failed { .. }
        ));
        assert!(matches!(
            shards.shards[1].outcome,
            review_core::ShardOutcomeV1::Missing { .. }
        ));
        assert!(
            execution
                .attempt_accounting()
                .iter()
                .all(|attempt| &attempt.reservation.node != parent)
        );
        assert_eq!(execution.budget.committed_tokens(), 0);
        (
            registration.child_set_id().to_owned(),
            registration.child_set().clone(),
            parent_output.0.clone(),
            shards,
            execution.budget.begun_attempts(),
            locked
                .replay(&review_store::store::task::task_run_id(TASK).unwrap())
                .unwrap(),
        )
    };
    let review_before = store.replay("review").unwrap();
    drop(store);
    let schema = validator("task-inspection-v5.json");
    let shown = json_output(cli(&repo, &state, &["task", "show", TASK]), 0);
    let explained = json_output(cli(&repo, &state, &["task", "explain", TASK]), 0);
    for value in [&shown, &explained] {
        assert_eq!(value["schema"], "af/task-inspection@5");
        valid(&schema, value);
        assert_eq!(
            value["owned_child_sets"],
            json!([{"artifact_id":set_id,"artifact_type":review_core::task::owned_children::TASK_OWNED_CHILD_SET_V1,"record":set}])
        );
        let records = value["execution_records"].as_array().unwrap();
        let owned: Vec<_> = records
            .iter()
            .filter(|entry| entry["artifact_type"] == TASK_EXECUTION_RECORD_V4)
            .collect();
        assert_eq!(owned.len(), 2);
        assert_eq!(owned[0]["record"]["kind"], "owned_children_registered");
        assert_eq!(
            owned[1]["record"],
            json!({"kind":"owned_children_completed","child_set_id":set_id,"output_id":parent_output})
        );
        assert_eq!(value["attempts"], expected_attempts);
        assert_eq!(value["chargeable_tokens"], "0");
        assert_eq!(value["history"].as_array().unwrap().len(), before.len());
    }
    let listed = json_output(cli(&repo, &state, &["task", "list"]), 0);
    let listed = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["task_id"] == TASK)
        .unwrap();
    valid(&validator("task-list-entry-v2.json"), listed);
    assert_eq!(listed["phase"], shown["phase"]);
    assert_eq!(listed["chargeable_tokens"], "0");
    assert_eq!(
        json_output(cli(&repo, &state, &["task", "show", "pagination-cli"]), 0),
        normal
    );
    let reopened = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let current = reopened.task_projection(&cas, TASK).unwrap().unwrap();
    assert_eq!(
        current
            .execution
            .unwrap()
            .outputs
            .values()
            .filter(|(id, _)| id == &parent_output)
            .count(),
        1
    );
    assert_eq!(
        reopened
            .replay(&review_store::store::task::task_run_id(TASK).unwrap())
            .unwrap(),
        before
    );
    assert_eq!(reopened.replay("review").unwrap(), review_before);
    assert!(!shards.complete());
}
