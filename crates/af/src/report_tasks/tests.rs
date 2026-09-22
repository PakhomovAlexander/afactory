use super::*;
use review_core::task::execution::*;
use review_core::task::pipeline::*;
use review_core::task::plan::*;
use review_core::task::usage::TaskTokenUsageV1;
use review_core::task::{self, TaskResultV1, TaskRevisionV1};
use review_graph::task::{CompileContext, OperatorAttemptCost, OperatorSignature, compile_task};
use review_store::AttemptWall;
use review_store::store::task::{DeveloperGrant, TaskAuthority, TaskLease};
use serde_json::json;

fn event() -> review_core::RunEvent {
    review_core::RunEvent {
        event_id: "b".repeat(26),
        run_id: crate::campaign_run_id("accounting"),
        sequence: 0,
        event_type: review_core::EventType::RunReportV6,
        occurred_at: "2026-09-12T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs: vec![],
        payload: json!({}),
    }
}

// A deterministic fixture admits only the graph/contexts below. No Worker or Provider runs.
struct Authority;
impl TaskAuthority for Authority {
    fn validate_plan(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        Ok(vec![])
    }
    fn authorize_decision(
        &self,
        _: &TaskRevisionV1,
        _: &str,
        _: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        Err("no generated plans".into())
    }
    fn authorization_current(&self, _: &PlanDecisionV1) -> Result<(), String> {
        Err("no generated plans".into())
    }
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    fn validate_output(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &TaskInvocationV1,
        _: &TaskOutputV1,
    ) -> Result<(), String> {
        Ok(())
    }
    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("no acceptance in accounting fixture".into())
    }
}

fn put(cas: &Cas, kind: &str, value: impl serde::Serialize) -> String {
    cas.put_artifact(
        kind,
        review_core::Producer::KernelOperation {
            run_id: "accounting-fixture".into(),
            node_id: None,
            operation_id: "capture".into(),
        },
        vec![],
        None,
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}

fn source_input(cas: &Cas, version: &str) -> (String, String) {
    use review_core::task::source::*;
    let issue = IssueInputV1 {
        schema: "af.issue-input/1".into(),
        id: "42".into(),
        key: "AF-42".into(),
        revision: version.into(),
        summary: "Inspect accounting".into(),
        description: format!("Captured requirements {version}"),
        acceptance: BTreeMap::new(),
    };
    let raw = cas
        .put_json(&serde_json::to_value(&issue).unwrap())
        .unwrap();
    let mut refs = BTreeSet::from([raw.clone()]);
    let fields = issue
        .fields()
        .into_iter()
        .map(|(key, text)| {
            let value_id = cas.put_json(&json!(text)).unwrap();
            let text_id = cas.put(text.as_bytes()).unwrap();
            refs.extend([value_id.clone(), text_id.clone()]);
            (key, TaskSourceFieldV1 { value_id, text_id })
        })
        .collect();
    let capture = TaskSourceCaptureV1 {
        schema: "af.task-source-capture/1".into(),
        adapter: TaskSourceAdapterV1::LocalIssue,
        locator: "issue.json".into(),
        external_id: issue.id.clone(),
        external_key: issue.key.clone(),
        source_revision: version.into(),
        raw_source_id: raw,
        fields,
    };
    let producer = review_core::Producer::KernelOperation {
        run_id: "accounting-fixture".into(),
        node_id: None,
        operation_id: "source".into(),
    };
    let capture = cas
        .put_artifact(
            TASK_SOURCE_CAPTURE_V1,
            producer.clone(),
            refs.into_iter().collect(),
            None,
            serde_json::to_value(capture).unwrap(),
        )
        .unwrap()
        .0;
    let requirements = issue.requirements(None);
    let id = cas
        .put_artifact(
            "af/Requirements@1",
            producer,
            vec![capture],
            None,
            serde_json::to_value(&requirements).unwrap(),
        )
        .unwrap()
        .0;
    (
        id,
        format!("Inspect the captured request\n\n{}", requirements.text),
    )
}

fn open_round(cas: &Cas, store: &mut EventStore) -> (String, String) {
    let tree = review_source_git::Manifest::default();
    let tree_id = cas.put_json(&serde_json::to_value(&tree).unwrap()).unwrap();
    let head = cas.put_json(&json!({
        "repository_id":"fixture/accounting", "vcs":"git", "capture":{"kind":"committed", "tree_id":"fixture"},
        "source_revision":"fixture", "content_digest":tree.content_digest(), "artifact_manifest":tree_id,
    })).unwrap();
    let pipeline = cas.put(b"version = 2\n[subject]\nkind = \"whole-tree\"\n[[nodes]]\nid = \"ledger\"\nkind = \"ledger\"\n").unwrap();
    let lock = cas.put(b"version = 1\n").unwrap();
    let finding_genesis = cas
        .put_json(&json!({"kind":"finding-set-genesis@1", "authority_snapshot_id":head}))
        .unwrap();
    let demand_genesis = cas
        .put_json(&json!({"kind":"demand-set-genesis@1", "authority_snapshot_id":head}))
        .unwrap();
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(json!({
        "authority_snapshot_id":head, "subject_kind":"whole-tree",
        "pipeline":{"path":"pipeline.toml", "artifact_id":pipeline}, "reviewer_lock":{"path":"af.lock", "artifact_id":lock},
        "reviewers":[], "execution_policy_ids":[], "project_policy_ids":[],
        "convergence":{"clean_rounds":1, "max_rounds":2, "gate":"major"}, "reviewer_timeout_seconds":60,
        "check_timeout_seconds":3600, "git_timeout_seconds":300,
        "finding_identity_policy":review_core::CANONICAL_FINDING_IDENTITY_POLICY,
        "finding_genesis_id":finding_genesis, "demand_genesis_id":demand_genesis,
    })).unwrap();
    let manifest_id = cas
        .put_json(&serde_json::to_value(manifest).unwrap())
        .unwrap();
    let subject_id = cas
        .put_json(&serde_json::to_value(review_core::SubjectV1::whole_tree(&head)).unwrap())
        .unwrap();
    let prior = cas
        .put_json(&json!({"subject_id":subject_id, "round":1, "prior_findings":[]}))
        .unwrap();
    let run = crate::campaign_run_id("accounting");
    let opened = store
        .append(
            &run,
            cas,
            review_store::NewEvent::new(
                review_core::EventType::CampaignOpenedV1,
                json!({"campaign_manifest_id":manifest_id, "authority_snapshot_id":head}),
            )
            .referencing(vec![manifest_id.clone(), head.clone()]),
        )
        .unwrap();
    let started = store.append(&run, cas, review_store::NewEvent::new(review_core::EventType::RoundStartedV1,
        json!({"round":1, "epoch":1, "campaign_manifest_id":manifest_id, "subject_id":subject_id,
            "prior_finding_set_id":prior, "prior_demand_set_id":demand_genesis}))
        .caused_by(opened.event_id).referencing(vec![head.clone(), manifest_id.clone(), subject_id.clone(), prior, demand_genesis])).unwrap();
    let round = LegacyReviewRoundV1 {
        campaign_id: run,
        round_event_id: started.event_id,
        campaign_manifest_id: manifest_id,
        subject_id,
        head_snapshot_id: head.clone(),
        round: 1,
        epoch: 1,
    };
    let wrapper = cas
        .put_artifact(
            LEGACY_REVIEW_ROUND_V1,
            review_core::Producer::KernelOperation {
                run_id: "accounting-fixture".into(),
                node_id: None,
                operation_id: "capture".into(),
            },
            round
                .artifact_refs()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            Some(head.clone()),
            serde_json::to_value(&round).unwrap(),
        )
        .unwrap()
        .0;
    (wrapper, head)
}

struct Fixture {
    directory: tempfile::TempDir,
    cas: Cas,
    store: EventStore,
    task: TaskRevisionV1,
    revision_id: String,
    plan: ExecutionPlanV1,
    plan_id: String,
    lease: TaskLease,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let policy = cas
            .put_json(&json!({"fixture":"trusted accounting inputs"}))
            .unwrap();
        let (source, goal) = source_input(&cas, "v1");
        let (round, head) = open_round(&cas, &mut store);
        let mut task: TaskRevisionV1 = serde_json::from_str(include_str!(
            "../../../../fixtures/task-contracts/v1/task-revision.json"
        ))
        .unwrap();
        task.task_id = "accounting-task".into();
        task.goal = goal;
        task.inputs.get_mut("requirements").unwrap().artifact_ids = vec![source.clone()];
        task.inputs.insert(
            "round".into(),
            ArtifactInputV1 {
                artifact_ids: vec![round],
                artifact_type: LEGACY_REVIEW_ROUND_V1.into(),
                cardinality: review_core::PortCardinality::One,
                snapshot_id: Some(head),
            },
        );
        task.authority.policy_id = policy.clone();
        task.acceptance.get_mut("checked").unwrap().verifier_policy = policy.clone();
        task.provenance.adapter_id = policy.clone();
        task.provenance.input_artifact_ids = vec![source];
        task.limits.deadline_unix_ms = 9_000_000_000_000;
        task.limits.max_attempts = 10;
        task.limits.verification = task::VerificationReserveV1 {
            tokens: 200,
            attempts: 3,
            wall_ms: 3000,
        };
        let revision_id = put(&cas, task::TASK_REVISION_V1, &task);
        let mut pipeline: PipelineDefinitionV1 = serde_json::from_str(include_str!(
            "../../../../fixtures/task-contracts/v1/pipeline-definition.json"
        ))
        .unwrap();
        let mut round_port = pipeline.contract.inputs["requirements"].clone();
        round_port.artifact_type = LEGACY_REVIEW_ROUND_V1.into();
        pipeline.contract.inputs.insert("round".into(), round_port);
        let mut probe = pipeline.nodes[0].clone();
        probe.id = "probe".into();
        let mut second = pipeline.nodes[0].clone();
        second.id = "second".into();
        pipeline.nodes.extend([probe, second]);
        pipeline.max_attempts = 9;
        let pipeline_id = put(&cas, "af/Pipeline@1", &pipeline);
        let mut output = pipeline.contract.outputs["document"].clone();
        output.covers.clear();
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([(
                    "input".into(),
                    pipeline.contract.inputs["requirements"].clone(),
                )]),
                outputs: BTreeMap::from([("output".into(), output)]),
            },
            effects: BTreeSet::new(),
            evidence: BTreeMap::from([("output".into(), BTreeSet::from([policy.clone()]))]),
            retains: BTreeMap::new(),
            roles: BTreeSet::from(["author".into()]),
            worker_input_type: Some("af/Requirements@1".into()),
            worker_output_type: Some("af/CheckedDocument@1".into()),
            outcome_port: None,
            attempt: Some(OperatorAttemptCost {
                tokens: 10,
                wall_ms: 1000,
            }),
        };
        let mut graph = compile_task(
            &task,
            "builtin/document",
            &CompileContext {
                slot_workers: BTreeMap::new(),
                pipelines: &BTreeMap::from([(pipeline.name.clone(), pipeline)]),
                signatures: &BTreeMap::from([("worker/builtin/document-author".into(), signature)]),
                acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
                max_nodes: 64,
                max_depth: 4,
            },
        )
        .unwrap();
        for (node, review_node) in [("write", "first"), ("second", "second")] {
            graph
                .nodes
                .get_mut(&format!("root.nodes.{node}"))
                .unwrap()
                .operator = CompiledOperator::ReviewDomain {
                review_node: review_node.into(),
                operation: ReviewOperation::Reviewer {
                    slot: review_node.into(),
                },
            };
        }
        graph.nodes.get_mut("root.nodes.probe").unwrap().operator =
            CompiledOperator::ProviderAdmission {
                bindings: BTreeSet::from(["first".into(), "second".into()]),
            };
        let mut plan: ExecutionPlanV1 = serde_json::from_str(include_str!(
            "../../../../fixtures/task-contracts/v1/execution-plan.json"
        ))
        .unwrap();
        plan.task_revision_id = revision_id.clone();
        plan.authority = task.authority.clone();
        plan.limits = task.limits.clone();
        plan.inputs = task.inputs.clone();
        plan.engine_id = policy;
        plan.pipeline_id = pipeline_id.clone();
        plan.compiled_graph_id = put(&cas, "af/CompiledTask@1", graph);
        plan.bindings.clear();
        plan.generated_origins.clear();
        plan.dependencies = BTreeMap::from([(
            "builtin/document".into(),
            PlanDependencyV1 {
                name: "builtin/document".into(),
                content_digest: cas.get_artifact(&pipeline_id).unwrap().content_id,
                artifact_id: pipeline_id,
            },
        )]);
        let plan_id = put(&cas, EXECUTION_PLAN_V1, &plan);
        let lease = store
            .open_task(&cas, &revision_id, "fixture", 60000)
            .unwrap();
        store
            .propose_task_plan(&cas, &lease, &plan_id, &Authority)
            .unwrap();
        store.admit_task_plan(&cas, &lease, &Authority).unwrap();
        let root = put(
            &cas,
            TASK_INVOCATION_V1,
            TaskInvocationV1 {
                plan_id: plan_id.clone(),
                node: "root.inputs".into(),
                inputs: BTreeMap::new(),
            },
        );
        store
            .record_task_invocation(&cas, &lease, &root, &Authority)
            .unwrap();
        let output = put(
            &cas,
            TASK_OUTPUT_V1,
            TaskOutputV1 {
                invocation_id: root,
                outputs: task.inputs.clone(),
            },
        );
        store
            .publish_task_output(&cas, &lease, &output, None, &Authority)
            .unwrap();
        for node in ["write", "second", "probe"] {
            let invocation = put(
                &cas,
                TASK_INVOCATION_V1,
                TaskInvocationV1 {
                    plan_id: plan_id.clone(),
                    node: format!("root.nodes.{node}"),
                    inputs: BTreeMap::from([("input".into(), task.inputs["requirements"].clone())]),
                },
            );
            store
                .record_task_invocation(&cas, &lease, &invocation, &Authority)
                .unwrap();
        }
        Self {
            directory,
            cas,
            store,
            task,
            revision_id,
            plan,
            plan_id,
            lease,
        }
    }

    fn fail(&mut self, node: &str, charge: u64) -> String {
        let context = self.cas.put_json(&json!({"context":node})).unwrap();
        let reserved = self
            .store
            .reserve_task_attempt(
                &self.cas,
                &self.lease,
                &format!("root.nodes.{node}"),
                &Authority,
            )
            .unwrap();
        let attempt = self
            .store
            .bind_task_attempt_context(&self.cas, &self.lease, &reserved, &context, &Authority)
            .unwrap();
        self.store
            .start_task_attempt(&self.cas, &self.lease, &attempt, &Authority)
            .unwrap();
        let diagnostic_id = self
            .cas
            .put_json(&json!({"failure":"deterministic fixture"}))
            .unwrap();
        let settlement = self
            .store
            .settle_task_attempt_with_feedback(
                &self.cas,
                &self.lease,
                TaskExecutionRecordV1::Settled {
                    attempt_id: attempt.id().into(),
                    charged_tokens: u128::from(charge),
                    result: TaskAttemptResultV1::Failed {
                        diagnostic_id,
                        feedback_id: None,
                    },
                    raw_artifact_ids: vec![],
                    usage_id: None,
                },
                &Authority,
                || Ok(None),
            )
            .unwrap();
        assert_eq!(
            settlement,
            review_store::store::task::execution::TaskSettlement::Settled
        );
        attempt.id().into()
    }

    fn observe(&mut self, attempt: &str, charge: u64) {
        let usage_id = put(
            &self.cas,
            task::usage::TASK_TOKEN_USAGE_V1,
            TaskTokenUsageV1 {
                input_tokens: Some(charge.into()),
                chargeable_tokens: charge.into(),
                ..Default::default()
            },
        );
        self.store
            .observe_task_usage(
                &self.cas,
                &self.lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: attempt.into(),
                    charged_tokens: u128::from(charge),
                    usage_id,
                    raw_artifact_ids: vec![],
                },
            )
            .unwrap();
    }

    fn report_event(&self, charge: u128, sequence: u64) -> review_core::RunEvent {
        let mut event = event();
        event.payload = serde_json::to_value(review_core::RunReportPayloadV6 {
            outcomes: vec![],
            blocked_gates: vec![],
            verdict: review_core::RunVerdictV3::Pass,
            spent_tokens: charge.into(),
            task_accounting: review_core::TaskReviewAccountingV1 {
                task_id: self.task.task_id.clone(),
                task_revision_id: self.revision_id.clone(),
                plan_id: self.plan_id.clone(),
                task_report_id: self.plan.engine_id.clone(),
                through_sequence: sequence,
            },
            execution: review_core::RunReportExecutionV6::Unbound {},
        })
        .unwrap();
        event
    }

    fn read(&self, events: &[review_core::RunEvent]) -> TaskAccountingReport {
        let store =
            EventStore::open_read_only(self.directory.path().join("events.sqlite")).unwrap();
        read(
            &store,
            &self.cas,
            &crate::campaign_run_id("accounting"),
            events,
        )
        .unwrap()
    }
}

#[test]
fn failures_before_first_conclusion_use_common_provider_and_business_attempts() {
    let mut f = Fixture::new();
    let reserved = f
        .store
        .reserve_task_attempt(&f.cas, &f.lease, "root.nodes.write", &Authority)
        .unwrap();
    f.store
        .release_reserved_task_attempt(&f.cas, &f.lease, &reserved, "context refused before start")
        .unwrap();
    f.fail("probe", 3);
    f.fail("write", 7);
    f.fail("second", 2);
    assert!(
        f.store
            .replay(&crate::campaign_run_id("accounting"))
            .unwrap()
            .iter()
            .all(|event| !event.event_type.is_run_report())
    );
    let report = f.read(&[]);
    assert_eq!(report.tasks.len(), 1);
    let value = serde_json::to_value(&report.tasks[0]).unwrap();
    assert_eq!(value["chargeable_tokens"], "12");
    assert_eq!(value["attempts_started"], "3");
    assert_eq!(value["provider_attempts_started"], "1");
    assert_eq!(value["business_attempts_started"], "2");
    assert_eq!(value["attempts"].as_array().unwrap().len(), 4);
    let provider = value["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attempt| attempt["category"] == "provider")
        .unwrap();
    assert_eq!(provider["binding_slots"], json!(["first", "second"]));
    assert_eq!(provider["plan_id"], f.plan_id);
    assert_eq!(provider["round"], 1);
    assert_eq!(provider["reserved_tokens"], "10");
    let view = crate::read_report_view(&f.store, &f.cas, "accounting").unwrap();
    let view = serde_json::to_value(view).unwrap();
    assert_eq!(view["schema"], "af/review-report@3");
    assert_eq!(view["runs_recorded"], 0);
    assert!(view.get("spend").is_none());
    assert_eq!(view["task_accounting"][0]["chargeable_tokens"], "12");
}

#[test]
fn frozen_cumulative_reports_are_not_summed_and_late_usage_remains_exact() {
    let mut f = Fixture::new();
    f.fail("write", 7);
    let second = f.fail("second", 2);
    let history = vec![f.report_event(7, 10), f.report_event(9, 20)];
    let before = serde_json::to_value(&history).unwrap();
    for charge in [u64::MAX, u64::MAX, 1] {
        f.observe(&second, charge);
    }
    let report = f.read(&history);
    assert_eq!(report.tasks.len(), 1);
    assert_eq!(
        report.tasks[0].chargeable_tokens.get(),
        u128::from(u64::MAX) + 7
    );
    let attempt = report.tasks[0]
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == second)
        .unwrap();
    assert_eq!(attempt.chargeable_tokens.get(), u128::from(u64::MAX));
    assert_eq!(attempt.reserved_tokens.get(), 10);
    let rows = crate::report_rounds(&history.iter().collect::<Vec<_>>(), &BTreeMap::new()).unwrap();
    assert_eq!(rows[0].task_chargeable_tokens_at_report.unwrap().get(), 7);
    assert_eq!(rows[1].task_chargeable_tokens_at_report.unwrap().get(), 9);
    assert_eq!(rows[1].reported_tokens, None);
    assert_eq!(serde_json::to_value(&history).unwrap(), before);
    for markdown in [false, true] {
        let text = render(&report.tasks, markdown);
        assert!(
            text.contains("cumulative charge 18446744073709551622 tokens"),
            "{text}"
        );
        assert!(
            text.contains("18446744073709551615 tokens (original cap 10)"),
            "{text}"
        );
    }
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    let attempts = state.execution.unwrap().attempt_accounting();
    assert_eq!(
        attempts
            .iter()
            .find(|attempt| attempt.attempt_id == second)
            .unwrap()
            .charged_tokens,
        u128::from(u64::MAX)
    );
    let events = f
        .store
        .replay(&task_run_id(&f.task.task_id).unwrap())
        .unwrap();
    let terminal = events.into_iter().filter_map(|event| {
        let transition: task::event::TaskTransitionV1 = serde_json::from_value(event.payload).unwrap();
        let task::event::TaskChangeV1::ExecutionRecorded { record_id } = transition.change else { return None; };
        let decoded = review_store::store::task::execution::read_execution_record(&f.cas, &record_id).unwrap();
        matches!(&decoded.record, TaskExecutionRecordV1::Settled { attempt_id, .. } if attempt_id == &second).then_some(decoded.envelope.payload)
    }).next().unwrap();
    assert_eq!(terminal["charged_tokens"], "2");
}

#[test]
fn inspection_keeps_one_attempts_wide_aggregate_and_native_components_exact() {
    for wide in [false, true] {
        let mut f = Fixture::new();
        f.fail("write", 7);
        let attempt = f.fail("second", 2);
        let exact = u128::from(u64::MAX) + 17;
        let usage = TaskTokenUsageV3 {
            input_tokens: Some((if wide { exact } else { u128::from(u64::MAX) }).into()),
            chargeable_tokens: exact.into(),
            ..Default::default()
        };
        let usage_id = put(
            &f.cas,
            if wide {
                task::usage::TASK_TOKEN_USAGE_V3
            } else {
                task::usage::TASK_TOKEN_USAGE_V2
            },
            &usage,
        );
        f.store
            .observe_task_usage(
                &f.cas,
                &f.lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: attempt.clone(),
                    charged_tokens: exact,
                    usage_id,
                    raw_artifact_ids: vec![],
                },
            )
            .unwrap();
        f.store
            .record_task_attempt_wall(&review_store::TaskAttemptWall {
                run_id: task_run_id(&f.task.task_id).unwrap(),
                attempt_id: attempt.clone(),
                node_id: "root.nodes.second".into(),
                round: 0,
                epoch: 1,
                started_unix_ms: 1000,
                elapsed_ms: 15,
                usage: Some(usage),
            })
            .unwrap();
        let view = crate::read_report_view(&f.store, &f.cas, "accounting").unwrap();
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(
            value["schema"],
            if wide {
                "af/review-report@4"
            } else {
                "af/review-report@3"
            }
        );
        assert_eq!(
            value["task_accounting"][0]["chargeable_tokens"],
            (exact + 7).to_string()
        );
        let row = value["task_accounting"][0]["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["attempt_id"] == attempt)
            .unwrap();
        assert_eq!(row["chargeable_tokens"], exact.to_string());
        assert_eq!(row["wall"]["usage"]["chargeable_tokens"], exact.to_string());
        assert_eq!(
            row["wall"]["usage"]["input_tokens"],
            (if wide { exact } else { u128::from(u64::MAX) }).to_string()
        );
        assert_eq!(row["reserved_tokens"], "10");
        assert_eq!(value["task_accounting"][0]["attempts_started"], "2");
        assert_eq!(value["wall_ms"], 15);
        let text = render(&view.task_accounting, false);
        assert!(
            text.contains(&format!("{exact} tokens (original cap 10)")),
            "{text}"
        );

        if wide {
            let schema: serde_json::Value =
                serde_json::from_str(include_str!("../../../../schemas/review-report-v4.json"))
                    .unwrap();
            let mut registry = jsonschema::Registry::new();
            for raw in [
                include_str!("../../../../schemas/task-contracts-v1.json"),
                include_str!("../../../../schemas/task-token-usage-v1.json"),
                include_str!("../../../../schemas/task-token-usage-v3.json"),
                include_str!("../../../../schemas/task-review-accounting-v1.json"),
            ] {
                let resource: serde_json::Value = serde_json::from_str(raw).unwrap();
                let id = resource["$id"].as_str().unwrap().to_owned();
                registry = registry
                    .add(id, jsonschema::Resource::from_contents(resource))
                    .unwrap();
            }
            let validator = {
                let registry = registry.prepare().unwrap();
                jsonschema::options()
                    .with_registry(&registry)
                    .build(&schema)
                    .unwrap()
            };
            assert!(
                validator.is_valid(&value),
                "{:?}",
                validator.iter_errors(&value).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn task_sidecar_uses_decimal_usage_and_captured_round_authority() {
    let mut f = Fixture::new();
    let attempt = f.fail("probe", 1);
    f.store
        .record_attempt_wall(&AttemptWall {
            run_id: task_run_id(&f.task.task_id).unwrap(),
            attempt_id: attempt,
            node_id: "root.nodes.probe".into(),
            round: 0,
            epoch: 19,
            started_unix_ms: 1000,
            elapsed_ms: 2500,
            usage: Some(review_store::AttemptUsage {
                input_tokens: Some(u64::MAX),
                chargeable_tokens: u64::MAX,
                ..Default::default()
            }),
        })
        .unwrap();
    let report = f.read(&[]);
    assert_eq!(
        (report.wall_rows[0].round, report.wall_rows[0].epoch),
        (1, 1)
    );
    let value = serde_json::to_value(&report.tasks[0]).unwrap();
    assert_eq!(value["wall_ms"], 2500);
    assert_eq!(
        value["attempts"][0]["wall"]["usage"]["input_tokens"],
        u64::MAX.to_string()
    );
    // Sidecar evidence does not itself rewrite the canonical charge during inspection.
    assert_eq!(value["chargeable_tokens"], "1");
    assert!(
        render(&report.tasks, true)
            .contains("Provider usage: chargeable 18446744073709551615; in 18446744073709551615")
    );
}

#[test]
fn overlapping_attempt_walls_merge_within_their_round_epoch() {
    let first = TaskAttemptWall {
        run_id: "task".into(),
        attempt_id: "first".into(),
        node_id: "root.reviewer".into(),
        round: 1,
        epoch: 1,
        started_unix_ms: 100,
        elapsed_ms: 40,
        usage: None,
    };
    let mut overlapping = first.clone();
    overlapping.attempt_id = "overlapping".into();
    overlapping.started_unix_ms = 120;
    overlapping.elapsed_ms = 50;
    let mut report = TaskAccountingReport {
        tasks: vec![],
        wall_rows: vec![],
    };
    assert_eq!(report.wall_ms(), None);
    report.wall_rows = vec![first, overlapping.clone()];
    assert_eq!(report.wall_ms(), Some(70));
    let mut restarted = overlapping;
    restarted.epoch = 2;
    restarted.started_unix_ms = 1000;
    report.wall_rows.push(restarted);
    assert_eq!(report.wall_ms(), Some(120));
}

/// A Campaign whose first Task capture failed has no Task accounting, so it keeps the
/// `af/review-report@1` label rather than claiming the Task-backed `@3` shape.
#[test]
fn a_campaign_without_a_task_keeps_the_review_report_1_label() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let store = EventStore::open_in_memory().unwrap();
    let view = crate::read_report_view(&store, &cas, "empty").unwrap();
    let value = serde_json::to_value(view).unwrap();
    assert_eq!(value["schema"], "af/review-report@1");
    assert!(value.get("task_accounting").is_none());
    assert!(value.get("spend").is_none());
}

/// RunReport@3-@5 carry a plain `u64` spend, which the Round row reports as `reported_tokens`
/// rather than as a Task cumulative charge.
#[test]
fn a_numeric_run_report_spend_is_shown_as_reported_tokens() {
    let event = review_core::RunEvent {
        event_type: review_core::EventType::RunReportV3,
        payload: serde_json::to_value(review_core::RunReportPayloadV3 {
            outcomes: vec![review_core::RunNodeReportV2 {
                node: "reviewer".into(),
                outcome: review_core::RunNodeOutcomeV2::Completed {
                    output_artifacts: vec![],
                },
            }],
            blocked_gates: vec![],
            verdict: review_core::RunVerdictV3::Pass,
            spent_tokens: Some(9),
        })
        .unwrap(),
        ..event()
    };
    let rows = crate::report_rounds(&[&event], &BTreeMap::new()).unwrap();
    assert_eq!(rows[0].tokens_label(), "reported tokens 9");
    assert_eq!(rows[0].tokens_cell(), "9");
    assert_eq!(
        serde_json::to_value(&rows[0]).unwrap(),
        json!({"run":1, "verdict":"pass", "reported_tokens":9})
    );
}

#[test]
fn retired_attempt_classification_uses_its_original_plan_after_replacement() {
    let mut f = Fixture::new();
    let original_attempt = f.fail("probe", 3);
    let (source, goal) = source_input(&f.cas, "v2");
    let mut next = f.task.clone();
    next.previous_revision_id = Some(f.revision_id.clone());
    next.revision += 1;
    next.goal = goal;
    next.inputs.get_mut("requirements").unwrap().artifact_ids = vec![source];
    next.provenance.input_artifact_ids = next
        .inputs
        .values()
        .flat_map(|input| input.artifact_ids.iter().cloned())
        .collect();
    let revision_id = put(&f.cas, task::TASK_REVISION_V1, &next);
    let mut graph: CompiledTask =
        artifact(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
    graph.inputs = next.inputs.clone();
    graph.nodes.get_mut("root.nodes.probe").unwrap().operator = CompiledOperator::ReviewDomain {
        review_node: "replacement".into(),
        operation: ReviewOperation::Reviewer {
            slot: "replacement".into(),
        },
    };
    let mut plan = f.plan.clone();
    plan.task_revision_id = revision_id.clone();
    plan.inputs = next.inputs;
    plan.compiled_graph_id = put(&f.cas, "af/CompiledTask@1", graph);
    let replacement = put(&f.cas, EXECUTION_PLAN_V1, plan);
    f.store
        .refresh_task_source(
            &f.cas,
            &f.lease,
            &revision_id,
            Some(&replacement),
            None,
            &Authority,
        )
        .unwrap();
    f.observe(&original_attempt, 9);
    let state = f
        .store
        .task_projection(&f.cas, &f.task.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(state.plan_id.as_deref(), Some(replacement.as_str()));
    assert!(matches!(
        state.execution.unwrap().graph.nodes["root.nodes.probe"].operator,
        CompiledOperator::ReviewDomain { .. }
    ));
    let report = f.read(&[]);
    assert_eq!(report.tasks[0].provider_attempts_started.get(), 1);
    assert_eq!(report.tasks[0].business_attempts_started.get(), 0);
    assert_eq!(report.tasks[0].chargeable_tokens.get(), 9);
    assert_eq!(report.tasks[0].attempts[0].plan_id, f.plan_id);
    assert_eq!(
        report.tasks[0].attempts[0].binding_slots,
        ["first", "second"]
    );
    let digest = f.plan.compiled_graph_id.strip_prefix("sha256:").unwrap();
    let original_graph = f
        .directory
        .path()
        .join("cas/objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    std::fs::remove_file(&original_graph).unwrap();
    let store = EventStore::open_read_only(f.directory.path().join("events.sqlite")).unwrap();
    assert!(read(&store, &f.cas, &crate::campaign_run_id("accounting"), &[]).is_err());
    assert!(
        !original_graph.exists(),
        "inspection must not reconstruct missing historical artifacts"
    );
}

#[test]
fn task_report_view_schema_requires_exact_decimals_and_snapshot_identity() {
    let mut f = Fixture::new();
    f.fail("write", 1);
    let mut view = crate::read_report_view(&f.store, &f.cas, "accounting").unwrap();
    view.rounds =
        crate::report_rounds(&[&f.report_event(u128::MAX, 20)], &BTreeMap::new()).unwrap();
    let value = serde_json::to_value(view).unwrap();
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../../schemas/review-report-v3.json")).unwrap();
    let mut registry = jsonschema::Registry::new();
    for resource in [
        include_str!("../../../../schemas/task-contracts-v1.json"),
        include_str!("../../../../schemas/task-token-usage-v1.json"),
        include_str!("../../../../schemas/task-token-usage-v2.json"),
        include_str!("../../../../schemas/task-review-accounting-v1.json"),
    ] {
        let resource: serde_json::Value = serde_json::from_str(resource).unwrap();
        let id = resource["$id"].as_str().unwrap().to_owned();
        registry = registry
            .add(id, jsonschema::Resource::from_contents(resource))
            .unwrap();
    }
    let validator = {
        let registry = registry.prepare().unwrap();
        jsonschema::options()
            .with_registry(&registry)
            .build(&schema)
            .unwrap()
    };
    assert!(
        validator.is_valid(&value),
        "{:?}",
        validator.iter_errors(&value).collect::<Vec<_>>()
    );
    for invalid in [
        json!(9007199254740992_u64),
        json!("01"),
        json!("340282366920938463463374607431768211456"),
    ] {
        let mut changed = value.clone();
        changed["task_accounting"][0]["chargeable_tokens"] = invalid;
        assert!(!validator.is_valid(&changed));
    }
    let mut changed = value.clone();
    changed["rounds"][0]
        .as_object_mut()
        .unwrap()
        .remove("task_accounting");
    assert!(!validator.is_valid(&changed));
    let mut changed = value;
    changed["task_accounting"][0]["attempts_started"] = json!(1);
    assert!(!validator.is_valid(&changed));
}

#[test]
fn bound_context_size_reads_referenced_and_inline_manifests() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let manifest = json!({"entries": [], "rendered_bytes": 24199, "estimated_tokens": 6050});
    // A captured Review Attempt names its manifest by identity.
    let manifest_id = cas.put_json(&manifest).unwrap();
    let review_context = put(
        &cas,
        review_core::task::review_compat::TASK_REVIEW_CONTEXT_V1,
        json!({"context_manifest_id": manifest_id, "review_node": "correctness"}),
    );
    assert_eq!(
        bound_context_size(&cas, &review_context),
        Some((24199, 6050)),
        "a Task-backed review Attempt reports its rendered input"
    );
    // A generic Task Worker context carries its manifest inline.
    let generic = cas.put_json(&json!({"manifest": manifest})).unwrap();
    assert_eq!(bound_context_size(&cas, &generic), Some((24199, 6050)));
    // Neither shape, no size: the report shows the layers without a cost.
    let bare = cas
        .put_json(&json!({"review_node": "correctness"}))
        .unwrap();
    assert_eq!(bound_context_size(&cas, &bare), None);
}
