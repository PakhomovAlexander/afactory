//! One Task in a Store on disk whose Attempts a test reserves, starts, observes, records in the
//! sidecar, settles and recovers through the Store's own API, with a fixture authority that
//! admits only that: no Worker or Provider runs. Adapted from `af`'s report accounting fixture.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::campaign_review::{CAMPAIGN_REVIEW_ROUND_V1, CampaignReviewRoundV1};
use review_core::task::execution::*;
use review_core::task::pipeline::*;
use review_core::task::plan::*;
use review_core::task::usage::{
    TASK_TOKEN_USAGE_V3, TASK_USAGE_OBSERVATION_V1, TaskTokenUsageV3, TaskUsageObservationV1,
};
use review_core::task::{self, ArtifactInputV1, EXECUTION_PLAN_V1, TaskResultV1, TaskRevisionV1};
use review_graph::task::{
    CompileContext, CompiledOperator, OperatorAttemptCost, OperatorSignature, ReviewOperation,
    compile_task,
};
use review_store::store::task::execution::PreparedTaskAttempt;
use review_store::store::task::{DeveloperGrant, TaskAuthority, TaskLease, task_run_id};
use review_store::{Cas, EventStore, TaskAttemptWall};
use serde_json::json;

/// Each Attempt's charge and unknown-usage cause, by Attempt id.
pub type Charges = BTreeMap<String, (u128, Option<&'static str>)>;

/// The fixture's Task.
pub const TASK_ID: &str = "telemetry-accounting";

/// The Task file the fixture's revision was read from (`af.task-file-adapter/1`).
pub const TASK_FILE: &str =
    "{\"schema\":\"af.task-file/1\",\"task_id\":\"telemetry-accounting\"}\n";

/// The `pipeline.toml` the fixture's root pipeline package declares.
pub const PIPELINE_TOML: &str = "name = \"builtin/document\"\nversion = 2\n";

/// The fixture's lease: renewed before every operation, and short enough that a successor can
/// take the Task once it lapses (ADR-0089).
const LEASE_MS: u64 = 3_000;

/// Admits the fixture's plan, contexts and outputs; nothing generated, no acceptance.
pub struct Authority;

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
        _: &review_store::store::task::execution::ReservedTaskAttempt,
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
        _definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        Ok(())
    }
    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("no acceptance in the accounting fixture".into())
    }
}

pub fn put(cas: &Cas, kind: &str, value: impl serde::Serialize) -> String {
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

fn source_input(cas: &Cas) -> (String, String) {
    use review_core::task::source::*;
    let issue = IssueInputV1 {
        schema: "af.issue-input/1".into(),
        id: "42".into(),
        key: "AF-42".into(),
        revision: "v1".into(),
        summary: "Inspect accounting".into(),
        description: "Captured requirements v1".into(),
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
        source_revision: "v1".into(),
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

/// The Campaign Round the Task's review nodes are bound to.
fn open_round(cas: &Cas, store: &mut EventStore) -> (String, String) {
    let tree = review_source_git::Manifest::default();
    let tree_id = cas.put_json(&serde_json::to_value(&tree).unwrap()).unwrap();
    let head = cas.put_json(&json!({
        "repository_id":"fixture/accounting", "vcs":"git", "capture":{"kind":"committed", "tree_id":"fixture"},
        "source_revision":"fixture", "content_digest":tree.content_digest(), "artifact_manifest":tree_id,
    })).unwrap();
    let pipeline = cas.put(b"version = 2\n[subject]\nkind = \"whole-tree\"\n[[nodes]]\nid = \"ledger\"\nkind = \"ledger\"\noutputs = [{ name = \"findings\", type = \"review.kernel/FindingSet@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"any\" }]\n").unwrap();
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
    let run = "campaign-accounting".to_string();
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
    let started = store
        .append(
            &run,
            cas,
            review_store::NewEvent::new(
                review_core::EventType::RoundStartedV1,
                json!({"round":1, "epoch":1, "campaign_manifest_id":manifest_id,
                    "subject_id":subject_id, "prior_finding_set_id":prior,
                    "prior_demand_set_id":demand_genesis}),
            )
            .caused_by(opened.event_id)
            .referencing(vec![
                head.clone(),
                manifest_id.clone(),
                subject_id.clone(),
                prior,
                demand_genesis,
            ]),
        )
        .unwrap();
    let round = CampaignReviewRoundV1 {
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
            CAMPAIGN_REVIEW_ROUND_V1,
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

/// The Task, its Store and its lease. Its nodes `write`, `second` and `probe` each reserve 10
/// tokens per Attempt.
pub struct TaskFixture {
    pub directory: tempfile::TempDir,
    pub cas: Cas,
    pub store: EventStore,
    pub task_id: String,
    pub run_id: String,
    pub lease: TaskLease,
}

impl TaskFixture {
    /// The Task, in the Store at `directory`: `observe` installs a commit observer on the Store
    /// before anything is committed, so every commit of the fixture is told.
    pub fn new(directory: tempfile::TempDir, observe: impl FnOnce(&mut EventStore)) -> Self {
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        observe(&mut store);
        let policy = cas
            .put_json(&json!({"fixture":"trusted accounting inputs"}))
            .unwrap();
        let (source, goal) = source_input(&cas);
        let (round, head) = open_round(&cas, &mut store);
        let mut task: TaskRevisionV1 = serde_json::from_str(include_str!(
            "../../../../../fixtures/task-contracts/v1/task-revision.json"
        ))
        .unwrap();
        task.task_id = TASK_ID.into();
        task.goal = goal;
        task.inputs.get_mut("requirements").unwrap().artifact_ids = vec![source.clone()];
        task.inputs.insert(
            "round".into(),
            ArtifactInputV1 {
                artifact_ids: vec![round],
                artifact_type: CAMPAIGN_REVIEW_ROUND_V1.into(),
                cardinality: review_core::PortCardinality::One,
                snapshot_id: Some(head),
            },
        );
        task.authority.policy_id = policy.clone();
        task.acceptance.get_mut("checked").unwrap().verifier_policy = policy.clone();
        // The revision a Task file made: its adapter names the file's bytes as read.
        let task_file = cas.put(TASK_FILE.as_bytes()).unwrap();
        task.provenance.adapter_id = cas
            .put_json(&json!({"schema":"af.task-file-adapter/1","source_file_id":task_file}))
            .unwrap();
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
            "../../../../../fixtures/task-contracts/v1/pipeline-definition.json"
        ))
        .unwrap();
        let mut round_port = pipeline.contract.inputs["requirements"].clone();
        round_port.artifact_type = CAMPAIGN_REVIEW_ROUND_V1.into();
        pipeline.contract.inputs.insert("round".into(), round_port);
        let mut probe = pipeline.nodes[0].clone();
        probe.id = "probe".into();
        let mut second = pipeline.nodes[0].clone();
        second.id = "second".into();
        pipeline.nodes.extend([probe, second]);
        pipeline.max_attempts = 9;
        // The root pipeline as the plan names it: the captured package declaring `pipeline.toml`.
        let files = BTreeMap::from([(
            "pipeline.toml".to_string(),
            PIPELINE_TOML.as_bytes().to_vec(),
        )]);
        let pipeline_id = put(
            &cas,
            "af/TaskPackage@1",
            json!({"schema":"af.task-package/1","name":"builtin/document","version":"1.0.0",
                   "digest":review_config::lock::package_digest_from_files(&files),
                   "files":files}),
        );
        let mut output = pipeline.contract.outputs["document"].clone();
        output.covers.clear();
        let signature = OperatorSignature {
            unbound_allowed: Default::default(),
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
            "../../../../../fixtures/task-contracts/v1/execution-plan.json"
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
            .open_task(&cas, &revision_id, "fixture", LEASE_MS)
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
        let run_id = task_run_id(&task.task_id).unwrap();
        Self {
            directory,
            cas,
            store,
            task_id: task.task_id,
            run_id,
            lease,
        }
    }

    /// Keep the lease: renewed to [`LEASE_MS`] from now.
    pub fn renew(&mut self) {
        std::thread::sleep(std::time::Duration::from_millis(2));
        self.store
            .renew_task_lease(&self.cas, &self.lease, LEASE_MS)
            .unwrap();
    }

    /// Reserve an Attempt of `node` and bind its context, without starting it.
    pub fn prepare(&mut self, node: &str) -> PreparedTaskAttempt {
        self.renew();
        let context = self.cas.put_json(&json!({ "context": node })).unwrap();
        let reserved = self
            .store
            .reserve_task_attempt(
                &self.cas,
                &self.lease,
                &format!("root.nodes.{node}"),
                &Authority,
            )
            .unwrap();
        self.store
            .bind_task_attempt_context(&self.cas, &self.lease, &reserved, &context, &Authority)
            .unwrap()
    }

    /// Reserve, bind and start an Attempt of `node`; its id.
    pub fn start(&mut self, node: &str) -> String {
        let attempt = self.prepare(node);
        self.store
            .start_task_attempt(&self.cas, &self.lease, &attempt, &Authority)
            .unwrap();
        attempt.id().to_string()
    }

    pub fn usage(&self, usage: &TaskTokenUsageV3) -> String {
        put(&self.cas, TASK_TOKEN_USAGE_V3, usage)
    }

    /// Commit a cumulative usage observation of `attempt`.
    pub fn observe(&mut self, attempt: &str, usage: TaskTokenUsageV3) {
        self.renew();
        self.observe_raw(attempt, usage, vec![]);
    }

    /// Commit a cumulative usage observation of `attempt` naming the raw artifacts `raw`, on
    /// the lease as it is.
    pub fn observe_raw(&mut self, attempt: &str, usage: TaskTokenUsageV3, raw: Vec<String>) {
        let usage_id = self.usage(&usage);
        self.store
            .observe_task_usage(
                &self.cas,
                &self.lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: attempt.into(),
                    charged_tokens: usage.chargeable_tokens.get(),
                    usage_id,
                    raw_artifact_ids: raw,
                },
            )
            .unwrap();
    }

    /// The native usage observation `native` of the Attempt `attempt` of `node`, in the CAS as
    /// its Worker would put it; its id.
    pub fn native(&self, attempt: &str, node: &str, native: &TaskUsageObservationV1) -> String {
        let context = self.cas.put_json(&json!({ "context": node })).unwrap();
        self.cas
            .put_artifact(
                TASK_USAGE_OBSERVATION_V1,
                review_core::Producer::Attempt {
                    run_id: self.run_id.clone(),
                    node_id: format!("root.nodes.{node}"),
                    attempt_id: attempt.into(),
                },
                vec![context],
                None,
                serde_json::to_value(native).unwrap(),
            )
            .unwrap()
            .0
    }

    /// Write the sidecar row of `attempt` (no Task event).
    pub fn wall(&self, store: &EventStore, attempt: &str, node: &str, usage: TaskTokenUsageV3) {
        store
            .record_task_attempt_wall(&TaskAttemptWall {
                run_id: self.run_id.clone(),
                attempt_id: attempt.into(),
                node_id: format!("root.nodes.{node}"),
                round: 1,
                epoch: u32::try_from(self.lease.epoch()).unwrap(),
                started_unix_ms: 1_000,
                elapsed_ms: 15,
                usage: Some(usage),
            })
            .unwrap();
    }

    /// Settle `attempt` as failed, charging `usage`.
    pub fn settle_failed(&mut self, attempt: &str, usage: TaskTokenUsageV3) {
        self.renew();
        let usage_id = self.usage(&usage);
        let diagnostic_id = self
            .cas
            .put_json(&json!({"failure":"deterministic fixture"}))
            .unwrap();
        self.store
            .settle_task_attempt_with_feedback(
                &self.cas,
                &self.lease,
                TaskExecutionRecordV1::Settled {
                    attempt_id: attempt.into(),
                    charged_tokens: usage.chargeable_tokens.get(),
                    result: TaskAttemptResultV1::Failed {
                        diagnostic_id,
                        feedback_id: None,
                    },
                    raw_artifact_ids: vec![],
                    usage_id: Some(usage_id),
                    unknown_usage: None,
                },
                &Authority,
                || Ok(None),
            )
            .unwrap();
    }

    /// The lease is lost: it expires, a successor takes the Task (ADR-0089) and recovers every
    /// pending Attempt.
    pub fn take_over_and_recover(&mut self) {
        std::thread::sleep(std::time::Duration::from_millis(LEASE_MS + 100));
        self.lease = self
            .store
            .take_task_lease(&self.cas, &self.task_id, "successor", LEASE_MS)
            .unwrap();
        self.store
            .recover_task_attempts(&self.cas, &self.lease)
            .unwrap();
    }

    /// Each Attempt's charge and unknown-usage cause as the Task projection holds them, the
    /// Task's committed charge (what `af task show` prints as `chargeable_tokens`) and how many
    /// Attempts' usage is unknown.
    pub fn kernel(&self) -> (Charges, u128, u64) {
        let state = self
            .store
            .task_projection(&self.cas, &self.task_id)
            .unwrap()
            .unwrap();
        let execution = state.execution.unwrap();
        let attempts: Charges = execution
            .attempt_accounting()
            .into_iter()
            .map(|row| {
                (
                    row.attempt_id,
                    (
                        row.charged_tokens,
                        row.unknown_usage.map(|cause| cause.as_str()),
                    ),
                )
            })
            .collect();
        let unknown = attempts
            .values()
            .filter(|(_, cause)| cause.is_some())
            .count() as u64;
        (attempts, execution.budget.committed_tokens(), unknown)
    }
}
