//! Inspect actual durable Broker receipts without invoking a Worker or Provider.
use super::*;
use review_config::task::catalog::{
    AdmittedWorkerSettings, TaskPackagePin, TaskPlanCompiler, TaskWorkerManifest, TaskWorkerRunner,
};
use review_core::task::broker::*;
use review_core::task::execution::*;
use review_core::task::plan::*;
use review_core::task::*;
use review_core::{
    BrokerFailureReasonV1, BrokerOperationOutcomeV1, BrokerOperationPolicyV1,
    BrokerOperationReceiptV2, Producer,
};
use review_pipeline::task::code::{CodeTaskPolicy, code_signatures};
use review_store::store::task::{DeveloperGrant, TaskAuthority, task_run_id};
use review_store::{Cas, EventStore};
use std::collections::BTreeMap;

const TASK: &str = "broker-inspection";
const NODE: &str = "root.nodes.implement";
const SLOT: &str = "root.slots.implementer";

fn capture(cas: &Cas, kind: &str, value: &impl serde::Serialize) -> String {
    cas.put_artifact(
        kind,
        Producer::KernelOperation {
            run_id: task_run_id(TASK).unwrap(),
            node_id: None,
            operation_id: "inspection-fixture@1".into(),
        },
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}

/// Test-only host authority rechecks the real compiler closure and admits only this
/// fixture's root input publication, exact context, and captured Broker operations.
struct Authority {
    compiler: TaskPlanCompiler,
    task: TaskRevisionV1,
}

impl TaskAuthority for Authority {
    fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        if task != &self.task {
            return Err("Inspection fixture does not authorize another Task".into());
        }
        self.compiler.validate_plan(cas, task, plan)
    }
    fn authorize_decision(
        &self,
        _: &TaskRevisionV1,
        _: &str,
        _: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        Err("Inspection fixture has no developer grant".into())
    }
    fn authorization_current(&self, _: &PlanDecisionV1) -> Result<(), String> {
        Err("Inspection fixture has no developer grant".into())
    }
    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("Paid transport failure is not an accepted Task result".into())
    }
    fn validate_context(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        invocation: &TaskInvocationV1,
        feedback: &[String],
        context_id: &str,
    ) -> Result<(), String> {
        self.validate_plan(cas, task, plan)?;
        if invocation.node != NODE
            || invocation.inputs != task.inputs
            || !feedback.is_empty()
            || cas.get_json(context_id).map_err(|e| e.to_string())? != json!(invocation)
        {
            return Err("Inspection context differs from captured invocation".into());
        }
        Ok(())
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        invocation: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.validate_plan(cas, task, plan)?;
        if invocation.node != "root.inputs" || output.outputs != task.inputs {
            return Err("Only captured root inputs may be published".into());
        }
        Ok(())
    }
    fn validate_broker_binding(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        binding: &TaskBrokerBindingV1,
    ) -> Result<(), String> {
        self.validate_plan(cas, task, plan)?;
        let policy_id = &plan.bindings[SLOT].invocation_policy_id;
        let policy = cas.get_json(policy_id).map_err(|e| e.to_string())?;
        if binding.node != NODE
            || binding.target
                != (TaskBrokerTargetV1::Worker {
                    slot: SLOT.into(),
                    invocation_policy_id: policy_id.clone(),
                })
            || json!(binding.operations) != policy["operations"]
        {
            return Err("Broker binding differs from the captured fixture policy".into());
        }
        Ok(())
    }
}

fn broker_plan(
    cas: &Cas,
    planned: &Value,
    operation: &BrokerOperationPolicyV1,
) -> (Authority, String, String) {
    let original: ExecutionPlanV1 = serde_json::from_value(planned["plan"].clone()).unwrap();
    let mut task: TaskRevisionV1 = serde_json::from_value(
        cas.get_artifact(planned["revision_id"].as_str().unwrap())
            .unwrap()
            .payload,
    )
    .unwrap();
    let captured = cas.get_json(&task.authority.policy_id).unwrap();
    let code_policy_id = captured["code_policy_id"].as_str().unwrap();
    let code_policy: CodeTaskPolicy =
        serde_json::from_value(cas.get_json(code_policy_id).unwrap()).unwrap();
    let policy_id = cas
        .put_json(&json!({
            "fixture":"broker-inspection", "captured_authority_id": task.authority.policy_id,
            "operations":[operation]
        }))
        .unwrap();
    task.task_id = TASK.into();
    task.authority.policy_id = policy_id.clone();
    let revision_id = capture(cas, TASK_REVISION_V1, &task);
    let mut compiler = TaskPlanCompiler::new(
        original.engine_id.clone(),
        policy_id.clone(),
        code_signatures(code_policy_id, &code_policy).unwrap(),
        BTreeMap::from([
            ("verified".into(), "snapshot".into()),
            ("goal".into(), "snapshot".into()),
        ]),
        serde_json::from_value(captured["independence"].clone()).unwrap(),
    )
    .unwrap();
    for (name, dependency) in &original.dependencies {
        let package = cas.get_artifact(&dependency.artifact_id).unwrap().payload;
        if name == "fixture/implementer" {
            // A paid Model reservation must come from a valid captured Model package.
            // The original zero-token Command package remains intact for the CLI control.
            let mut files: BTreeMap<String, Vec<u8>> =
                serde_json::from_value(package["files"].clone()).unwrap();
            let mut worker: TaskWorkerManifest =
                toml::from_str(std::str::from_utf8(&files["worker.toml"]).unwrap()).unwrap();
            worker.runner = TaskWorkerRunner::Model {
                provider_kind: "fixture".into(),
                model: "fixture-model".into(),
                effort: "high".into(),
            };
            worker.signature.attempt.as_mut().unwrap().tokens = 10;
            files.insert(
                "worker.toml".into(),
                toml::to_string(&worker).unwrap().into_bytes(),
            );
            let pin = TaskPackagePin {
                version: worker.version,
                digest: review_config::lock::package_digest_from_files(&files),
                path: "package".into(),
            };
            compiler
                .capture_package(
                    cas,
                    name,
                    &pin,
                    &files
                        .into_iter()
                        .map(|(path, bytes)| (format!("package/{path}"), bytes))
                        .collect(),
                )
                .unwrap();
        } else {
            compiler
                .restore_package(
                    cas,
                    name,
                    package["digest"].as_str().unwrap(),
                    &dependency.artifact_id,
                )
                .unwrap();
        }
    }
    for (slot, binding) in &original.bindings {
        let name = planned["graph"]["slots"][slot]["worker"].as_str().unwrap();
        compiler
            .bind_worker(
                name,
                AdmittedWorkerSettings {
                    execution: if slot == SLOT {
                        WorkerExecutionV1::Model {
                            provider: "fixture".into(),
                            provider_kind: "fixture".into(),
                            principal_id: "fixture-principal".into(),
                            model: "fixture-model".into(),
                            effort: "high".into(),
                        }
                    } else {
                        binding.execution.clone()
                    },
                    invocation_policy_id: if slot == SLOT {
                        policy_id.clone()
                    } else {
                        binding.invocation_policy_id.clone()
                    },
                },
            )
            .unwrap();
    }
    let (plan, graph) = compiler
        .compile(cas, &revision_id, "fixture/implementation")
        .unwrap();
    assert_eq!(plan.inputs, original.inputs);
    assert_eq!(plan.limits, original.limits);
    assert_eq!(graph.allowances[NODE].tokens_per_attempt, 10);
    let plan_id = capture(cas, EXECUTION_PLAN_V1, &plan);
    (Authority { compiler, task }, revision_id, plan_id)
}

#[test]
fn broker_inspection_reopens_exact_receipts_in_one_task_history_and_keeps_normal_v3() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let planned = json_output(
        cli(&repo, &state, &["task", "plan", "--file", "ticket.json"]),
        0,
    );
    valid(&validator("task-inspection-v3.json"), &planned);
    let normal = json_output(cli(&repo, &state, &["task", "show", "pagination-cli"]), 0);
    assert_eq!(normal["schema"], "af/task-inspection@3");

    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let mut store = EventStore::open(state.join("events.sqlite")).unwrap();
    let operation = BrokerOperationPolicyV1 {
        name: "generate".into(),
        destination: "fixture".into(),
        method: "generate".into(),
        max_request_bytes: 16,
        max_response_bytes: 16,
        max_calls: 2,
        max_usage: 10,
    };
    let (authority, revision_id, plan_id) = broker_plan(&cas, &planned, &operation);
    let lease = store
        .open_task(&cas, &revision_id, "fixture", 60_000)
        .unwrap();
    store
        .propose_task_plan(&cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(&cas, &lease, &authority).unwrap();
    let root_id = capture(
        &cas,
        TASK_INVOCATION_V1,
        &TaskInvocationV1 {
            plan_id: plan_id.clone(),
            node: "root.inputs".into(),
            inputs: BTreeMap::new(),
        },
    );
    store
        .record_task_invocation(&cas, &lease, &root_id, &authority)
        .unwrap();
    let root_output = capture(
        &cas,
        TASK_OUTPUT_V1,
        &TaskOutputV1 {
            invocation_id: root_id,
            outputs: authority.task.inputs.clone(),
        },
    );
    store
        .publish_task_output(&cas, &lease, &root_output, None, &authority)
        .unwrap();
    let invocation = TaskInvocationV1 {
        plan_id: plan_id.clone(),
        node: NODE.into(),
        inputs: authority.task.inputs.clone(),
    };
    let invocation_id = capture(&cas, TASK_INVOCATION_V1, &invocation);
    store
        .record_task_invocation(&cas, &lease, &invocation_id, &authority)
        .unwrap();
    let context_id = cas.put_json(&json!(invocation)).unwrap();
    let attempt = store
        .prepare_task_attempt(&cas, &lease, NODE, &context_id, &authority)
        .unwrap();
    assert_eq!(attempt.reservation().tokens, 10);
    store
        .start_task_attempt(&cas, &lease, &attempt, &authority)
        .unwrap();
    let bound = store
        .bind_task_broker(
            &cas,
            &lease,
            &attempt,
            "hhhhhhhhhhhhhhhhhhhhhhhhhh",
            &[operation],
            &authority,
        )
        .unwrap();
    for (ordinal, reserved, charge) in [(1, 7, 7), (2, 3, u64::MAX)] {
        let receipt = BrokerOperationReceiptV2 {
            handle_id: bound.binding().handle_id.clone(),
            node: bound.binding().lease.node_id.clone(),
            attempt_id: attempt.id().into(),
            lease_epoch: lease.epoch(),
            operation: "generate".into(),
            destination: "fixture".into(),
            method: "generate".into(),
            ordinal,
            outcome: if ordinal == 1 {
                BrokerOperationOutcomeV1::Succeeded
            } else {
                BrokerOperationOutcomeV1::Failed
            },
            failure_reason: if ordinal == 1 {
                None
            } else {
                Some(BrokerFailureReasonV1::UsageOverrun)
            },
            request_digest: cas.put(b"request").unwrap(),
            response_digest: Some(cas.put(b"reply").unwrap()),
            request_bytes: 7,
            response_bytes: 5,
            reserved_usage: reserved,
            charged_usage: charge.into(),
        };
        store
            .record_task_broker_receipt(&cas, &bound, &receipt, &authority)
            .unwrap();
    }
    store
        .settle_task_attempt(
            &cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 3,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id: cas
                        .put_json(&json!({"failure":"fixture transport ended"}))
                        .unwrap(),
                    feedback_id: None,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &authority,
        )
        .unwrap();
    let run_id = task_run_id(TASK).unwrap();
    let before = store.replay(&run_id).unwrap();
    drop(store);

    let show = json_output(cli(&repo, &state, &["task", "show", TASK]), 0);
    let explain = json_output(cli(&repo, &state, &["task", "explain", TASK]), 0);
    let total = (u128::from(u64::MAX) + 7).to_string();
    for view in [&show, &explain] {
        valid(&validator("task-inspection-v4.json"), view);
        assert_eq!(view["schema"], "af/task-inspection@4");
        assert_eq!(view["plan_id"], plan_id);
        assert_eq!(view["revision_id"], revision_id);
        assert_eq!(
            view["attempts"], 1,
            "Broker operations are not Worker Attempts"
        );
        assert_eq!(view["chargeable_tokens"], total);
        let records = view["broker_records"].as_array().unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["artifact_id"], bound.binding_id());
        assert_eq!(records[0]["artifact_type"], TASK_BROKER_BINDING_V1);
        assert_eq!(records[0]["record"], json!(bound.binding()));
        for (record, ordinal, charge) in [
            (&records[1], 1, "7"),
            (&records[2], 2, "18446744073709551615"),
        ] {
            assert_eq!(record["artifact_type"], TASK_BROKER_OPERATION_V1);
            assert_eq!(record["record"]["binding_id"], bound.binding_id());
            assert_eq!(record["record"]["receipt"]["attempt_id"], attempt.id());
            assert_eq!(record["record"]["receipt"]["ordinal"], ordinal);
            assert_eq!(record["record"]["receipt"]["charged_usage"], charge);
        }
        let history = view["history"].as_array().unwrap();
        assert_eq!(history.len(), before.len());
        for (index, event) in history.iter().enumerate() {
            assert_eq!(event["sequence"], index);
        }
        let broker_ids: Vec<_> = history
            .iter()
            .filter_map(|event| event.pointer("/broker_transition/record_id"))
            .collect();
        assert_eq!(
            broker_ids,
            records
                .iter()
                .map(|r| &r["artifact_id"])
                .collect::<Vec<_>>()
        );
        let execution = view["execution_records"].as_array().unwrap();
        for record in execution {
            let accounting = matches!(
                record["record"]["kind"].as_str(),
                Some("settled" | "usage_observed")
            );
            assert_eq!(
                record["artifact_type"],
                if accounting {
                    TASK_EXECUTION_RECORD_V3
                } else {
                    TASK_EXECUTION_RECORD_V1
                }
            );
        }
        assert_eq!(
            execution
                .iter()
                .filter(|r| r["record"]["kind"] == "started")
                .count(),
            1
        );
        let settled: Vec<_> = execution
            .iter()
            .filter(|r| r["record"]["kind"] == "settled")
            .collect();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0]["record"]["attempt_id"], attempt.id());
        assert_eq!(settled[0]["record"]["charged_tokens"], "3");
        assert!(view["run_reports"].as_array().unwrap().is_empty());
    }
    assert_eq!(show["history"], explain["history"]);
    assert_eq!(show["execution_records"], explain["execution_records"]);
    assert_eq!(
        explain["plan"]["compiled_graph_id"],
        cas.get_artifact(&plan_id).unwrap().payload["compiled_graph_id"]
    );
    assert_eq!(
        explain["graph"]["allowances"][NODE]["tokens_per_attempt"],
        10
    );
    let list = json_output(cli(&repo, &state, &["task", "list"]), 0);
    assert_eq!(list["schema"], "af/task-list@2");
    assert_eq!(list["tasks"].as_array().unwrap().len(), 2);
    for entry in list["tasks"].as_array().unwrap() {
        valid(&validator("task-list-entry-v2.json"), entry);
        assert_eq!(
            entry["chargeable_tokens"],
            if entry["task_id"] == TASK {
                total.as_str()
            } else {
                "0"
            }
        );
    }
    assert_eq!(
        json_output(cli(&repo, &state, &["task", "show", "pagination-cli"]), 0),
        normal
    );
    assert!(normal.get("broker_records").is_none());
    assert_eq!(
        json_output(cli(&repo, &state, &["task", "show", TASK]), 0),
        show
    );
    let reopened = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    assert_eq!(
        reopened.replay(&run_id).unwrap(),
        before,
        "inspection must not append history"
    );
    let execution = reopened
        .task_projection(&cas, TASK)
        .unwrap()
        .unwrap()
        .execution
        .unwrap();
    assert!(execution.budget.breached());
    assert_eq!(execution.budget.committed_tokens().to_string(), total);
    assert_eq!(execution.budget.begun_attempts(), 1);
}
