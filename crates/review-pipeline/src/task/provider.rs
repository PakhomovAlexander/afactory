//! Installed Provider bootstrap uses a normal charged Task Attempt and the exact same native
//! adapter binding as its downstream Workers. It receives no Task source or private inputs.
use super::host::{TaskDomain, TaskModelBinding};
use super::source::invocation_producer;
use super::{TaskOperatorHost, TaskWorkOutput, envelope};
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::ReceiptOutcomeV1;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::provider::*;
use review_core::task::{ArtifactInputV1, TaskResultV1, TaskRevisionV1};
use review_graph::task::{CompiledOperator, CompiledTask};
use review_store::store::task::execution::ReservedTaskAttempt;
use review_store::{Cas, store::task::execution::PreparedTaskAttempt};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_runner::task::provider::{PROBE_INPUT, TASK_PROVIDER_CONTEXT_V2, TaskProviderContextV2};

enum Admission {
    Legacy(TaskProviderAdmissionV1),
    Brokered(TaskProviderAdmissionV2),
}
impl Admission {
    fn bindings(&self) -> &BTreeSet<String> {
        match self {
            Self::Legacy(v) => &v.bindings,
            Self::Brokered(v) => &v.bindings,
        }
    }
    fn policy_id(&self) -> &str {
        match self {
            Self::Legacy(v) => &v.invocation_policy_id,
            Self::Brokered(v) => &v.probe_policy_id,
        }
    }
    fn artifact_type(&self) -> &'static str {
        match self {
            Self::Legacy(_) => TASK_PROVIDER_ADMISSION_V1,
            Self::Brokered(_) => TASK_PROVIDER_ADMISSION_V2,
        }
    }
    fn payload(&self) -> Result<serde_json::Value, String> {
        match self {
            Self::Legacy(v) => serde_json::to_value(v),
            Self::Brokered(v) => serde_json::to_value(v),
        }
        .map_err(|e| e.to_string())
    }
}

pub(super) fn load_probe_policy(
    cas: &Cas,
    plan: &ExecutionPlanV1,
    id: &str,
) -> Result<TaskProviderProbePolicyV1, String> {
    let artifact = envelope(cas, id)?;
    let policy: TaskProviderProbePolicyV1 =
        serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
    policy.validate()?;
    if artifact.artifact_type != TASK_PROVIDER_PROBE_POLICY_V1
        || artifact.input_artifacts != policy.artifact_refs()
        || artifact.subject_snapshot_id.is_some()
        || policy.authority_policy_id != plan.authority.policy_id
        || !plan.dependencies.values().any(|dependency| {
            dependency.artifact_id == id && dependency.content_digest == artifact.content_id
        })
    {
        return Err("Provider probe is not an exact captured plan dependency".into());
    }
    Ok(policy)
}

pub struct ProviderTaskDomain<'a> {
    pub graph: &'a CompiledTask,
    pub models: &'a BTreeMap<String, TaskModelBinding<'a>>,
    pub inner: &'a dyn TaskDomain,
}
impl ProviderTaskDomain<'_> {
    fn slots(&self, input: &TaskInvocationV1) -> Option<&BTreeSet<String>> {
        match &self.graph.nodes.get(&input.node)?.operator {
            CompiledOperator::ProviderAdmission { bindings }
            | CompiledOperator::ProviderAdmissionBrokered { bindings, .. } => Some(bindings),
            _ => None,
        }
    }
    fn receipt(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<Admission, String> {
        if !input.inputs.is_empty() {
            return Err("Provider admission cannot consume business inputs".into());
        }
        let slots = self
            .slots(input)
            .ok_or("Not an installed Provider admission")?;
        let first = self
            .models
            .get(slots.first().ok_or("Empty Provider capability")?)
            .ok_or("Provider admission has no captured adapter")?;
        let plan: ExecutionPlanV1 = serde_json::from_value(envelope(cas, &input.plan_id)?.payload)
            .map_err(|e| e.to_string())?;
        for slot in slots {
            let model = self
                .models
                .get(slot)
                .ok_or("Missing captured Provider adapter")?;
            if plan.bindings.get(slot) != Some(&model.binding)
                || model.binding.execution != first.binding.execution
                || model.binding.invocation_policy_id != first.binding.invocation_policy_id
                || model.adapter.credential_mode() != first.adapter.credential_mode()
                || !matches!(&model.binding.execution,review_core::task::plan::WorkerExecutionV1::Model {provider_kind,model: id,effort,..}
                    if model.adapter.provider_kind()==provider_kind && model.adapter.model_settings()==Some((id.clone(),effort.clone())))
            {
                return Err(
                    "Provider admission changed its exact account, model or invocation policy"
                        .into(),
                );
            }
        }
        if let CompiledOperator::ProviderAdmissionBrokered {
            probe_policy_id, ..
        } = &self.graph.nodes[&input.node].operator
        {
            let policy = load_probe_policy(cas, &plan, probe_policy_id)?;
            if policy.execution != first.binding.execution
                || first.adapter.credential_mode() != policy.credential_mode
                || self
                    .graph
                    .allowances
                    .get(&input.node)
                    .is_none_or(|allowance| {
                        review_core::broker_authority_usage(&policy.operations)
                            .map_or(true, |usage| usage > allowance.tokens_per_attempt)
                    })
            {
                return Err(
                    "Provider probe changed its exact execution, transport or reservation".into(),
                );
            }
            let receipt = TaskProviderAdmissionV2 {
                plan_id: input.plan_id.clone(),
                bindings: slots.clone(),
                execution: policy.execution,
                probe_policy_id: probe_policy_id.clone(),
                outcome: ReceiptOutcomeV1::Passed,
            };
            receipt.validate()?;
            return Ok(Admission::Brokered(receipt));
        }
        let receipt = TaskProviderAdmissionV1 {
            plan_id: input.plan_id.clone(),
            bindings: slots.clone(),
            execution: first.binding.execution.clone(),
            invocation_policy_id: first.binding.invocation_policy_id.clone(),
            outcome: ReceiptOutcomeV1::Passed,
        };
        receipt.validate()?;
        Ok(Admission::Legacy(receipt))
    }
    fn probe(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        let prepared = (|| {
            let receipt = self.receipt(cas, input)?;
            let attempt = attempt.ok_or("Provider capability probe has no started Task Attempt")?;
            let context = self.prepare_context(cas, input, &[])?;
            if context != attempt.context_id() {
                return Err("Provider probe lost its exact context".into());
            }
            let model = &self.models[receipt
                .bindings()
                .first()
                .ok_or("Empty Provider bindings")?];
            if (model.adapter.credential_mode() == review_core::BrokerCredentialModeV1::Brokered)
                != broker.is_some()
            {
                return Err("Provider transport differs from its runtime Broker capability".into());
            }
            let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64;
            let remaining = attempt.reservation().deadline_unix_ms.saturating_sub(now);
            if remaining == 0 {
                return Err("Provider admission deadline expired".into());
            }
            Ok((
                receipt,
                context,
                model,
                directory,
                Duration::from_millis(remaining),
            ))
        })();
        let (receipt, context, model, directory, timeout) = match prepared {
            Ok(value) => value,
            Err(error) => {
                return TaskWorkOutput {
                    usage_observation: None,
                    usage: None,
                    outputs: Err(error),
                    charged_tokens: Some(0),
                    raw_artifact_ids: vec![],
                    usage_id: None,
                    feedback_id: None,
                };
            }
        };
        let returned = model.adapter.invoke_controlled(
            cas,
            directory.path(),
            PROBE_INPUT.to_vec(),
            timeout,
            false,
            broker,
            cancellation,
        );
        let charged_tokens = returned
            .usage
            .as_ref()
            .map(|usage| usage.chargeable_tokens.get());
        let outputs = (|| {
            let message = returned.message?;
            if message.len() > 64
                || !std::str::from_utf8(&message)
                    .map_err(|e| e.to_string())?
                    .trim()
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case("OK")
            {
                return Err(
                    "Provider capability probe did not acknowledge its exact request".into(),
                );
            }
            let refs = vec![
                context,
                input.plan_id.clone(),
                receipt.policy_id().to_owned(),
            ];
            let id = cas
                .put_artifact(
                    receipt.artifact_type(),
                    invocation_producer(cas, input, attempt)?,
                    refs,
                    None,
                    receipt.payload()?,
                )
                .map_err(|e| e.to_string())?
                .0;
            Ok(BTreeMap::from([(
                "result".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![id],
                    artifact_type: receipt.artifact_type().into(),
                    cardinality: review_core::PortCardinality::One,
                    snapshot_id: None,
                },
            )]))
        })();
        TaskWorkOutput {
            usage_observation: returned.usage_observation,
            usage: returned.usage,
            outputs,
            charged_tokens,
            raw_artifact_ids: returned.raw_artifact_ids,
            usage_id: None,
            feedback_id: None,
        }
    }
}
impl TaskOperatorHost for ProviderTaskDomain<'_> {
    fn prepare_owned_children(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
    ) -> Result<super::TaskOwnedChildrenInputs, String> {
        self.inner.prepare_owned_children(cas, parent)
    }
    fn complete_owned_children(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        self.inner
            .complete_owned_children(cas, parent, children, facts)
    }

    fn broker_operations(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<Option<Vec<review_core::BrokerOperationPolicyV1>>, String> {
        if self.slots(input).is_some() {
            match self.receipt(cas, input)? {
                Admission::Legacy(_) => Ok(None),
                Admission::Brokered(receipt) => {
                    let plan: ExecutionPlanV1 =
                        serde_json::from_value(envelope(cas, &input.plan_id)?.payload)
                            .map_err(|e| e.to_string())?;
                    Ok(Some(
                        load_probe_policy(cas, &plan, &receipt.probe_policy_id)?.operations,
                    ))
                }
            }
        } else {
            self.inner.broker_operations(cas, input)
        }
    }

    fn commit_domain_invocation(
        &self,
        cas: &Cas,
        id: &str,
        input: &TaskInvocationV1,
    ) -> Result<(), String> {
        self.inner.commit_domain_invocation(cas, id, input)
    }

    fn commit_domain_output(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        id: &str,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.inner.commit_domain_output(cas, input, id, output)
    }

    fn prepare_context_for_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        if self.slots(input).is_none() {
            self.inner.prepare_context_for_attempt(cas, input, attempt)
        } else {
            self.prepare_context(cas, input, attempt.feedback_ids())
        }
    }
    fn output_rejection_feedback(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
    ) -> Result<Option<String>, String> {
        if self.slots(input).is_none() {
            self.inner.output_rejection_feedback(cas, input, attempt)
        } else {
            Ok(None)
        }
    }

    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        if self.slots(input).is_none() {
            return self.inner.prepare_context(cas, input, feedback);
        }
        if !feedback.is_empty() {
            return Err("Provider admission has one bounded Attempt".into());
        }
        let receipt = self.receipt(cas, input)?;
        let rendered = cas.put(PROBE_INPUT).map_err(|e| e.to_string())?;
        let mut manifest = review_runner::ContextManifest::default();
        manifest.record(
            "capability_probe",
            "installed Provider admission",
            Some(rendered.clone()),
            None,
            PROBE_INPUT.len(),
        );
        manifest.finish(PROBE_INPUT.len());
        if self
            .graph
            .allowances
            .get(&input.node)
            .is_none_or(|a| a.tokens_per_attempt < manifest.estimated_tokens)
        {
            return Err("Provider probe exceeds its captured reservation".into());
        }
        let (artifact_type, refs, payload) = match receipt {
            Admission::Legacy(receipt) => (
                "af/TaskProviderContext@1",
                vec![input.plan_id.clone(), rendered.clone()],
                json!({"invocation":input,"capability":receipt,"rendered_id":rendered,"manifest":manifest}),
            ),
            Admission::Brokered(capability) => {
                let context = TaskProviderContextV2 {
                    invocation: input.clone(),
                    capability,
                    rendered_id: rendered,
                    manifest,
                };
                context.validate()?;
                (
                    TASK_PROVIDER_CONTEXT_V2,
                    context.artifact_refs(),
                    serde_json::to_value(context).map_err(|e| e.to_string())?,
                )
            }
        };
        cas.put_artifact(
            artifact_type,
            invocation_producer(cas, input, None)?,
            refs,
            None,
            payload,
        )
        .map(|(id, _)| id)
        .map_err(|e| e.to_string())
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        self.execute_with_broker(cas, input, attempt, None)
    }

    fn execute_with_broker(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
    ) -> TaskWorkOutput {
        self.execute_controlled(cas, input, attempt, broker, None)
    }

    fn execute_controlled(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = crate::task::control::check(cancellation) {
            return crate::task::control::refused(error);
        }

        if self.slots(input).is_some() {
            self.probe(cas, input, attempt, broker, cancellation)
        } else {
            self.inner
                .execute_controlled(cas, input, attempt, broker, cancellation)
        }
    }
}
impl TaskDomain for ProviderTaskDomain<'_> {
    fn validate_review_integration_selection(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        self.inner
            .validate_review_integration_selection(cas, task, plan, phase, evidence)
    }
    #[allow(clippy::too_many_arguments)]
    fn validate_review_integration_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        report: &review_core::task::report::TaskRunReportV2,
        events: &[review_store::NewEvent],
        evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        self.inner.validate_review_integration_completion(
            cas, task, plan, phase, report, events, evidence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_review_continuation(
        &self,
        cas: &Cas,
        previous: &TaskRevisionV1,
        next: &TaskRevisionV1,
        previous_plan: &ExecutionPlanV1,
        next_plan: &ExecutionPlanV1,
        handoff: &review_core::task::review_handoff::TaskReviewHandoffV1,
    ) -> Result<(), String> {
        self.inner.validate_review_continuation(
            cas,
            previous,
            next,
            previous_plan,
            next_plan,
            handoff,
        )
    }
    fn validate_owned_children(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        self.inner
            .validate_owned_children(cas, task, plan, parent, children)
    }
    fn validate_owned_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.inner
            .validate_owned_completion(cas, task, plan, parent, children, facts, output)
    }

    fn validate_broker_binding(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        binding: &review_core::task::broker::TaskBrokerBindingV1,
    ) -> Result<(), String> {
        let input = TaskInvocationV1 {
            plan_id: binding.plan_id.clone(),
            node: binding.node.clone(),
            inputs: BTreeMap::new(),
        };
        if self.slots(&input).is_none() {
            return self.inner.validate_broker_binding(cas, task, plan, binding);
        }
        binding.validate()?;
        let Admission::Brokered(receipt) = self.receipt(cas, &input)? else {
            return Err("Provider admission has no captured Broker operation policy".into());
        };
        let policy = load_probe_policy(cas, plan, &receipt.probe_policy_id)?;
        if binding.task_id != task.task_id
            || binding.task_revision_id != plan.task_revision_id
            || binding.lease.node_id != binding.node
            || binding.target
                != (review_core::task::broker::TaskBrokerTargetV1::ProviderAdmission {
                    probe_policy_id: receipt.probe_policy_id,
                })
            || binding.operations != policy.operations
        {
            return Err(
                "Provider Broker binding changed its original node or separate probe policy".into(),
            );
        }
        Ok(())
    }

    fn validate_retry(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        previous: &BTreeMap<String, review_core::task::execution::TaskAttemptResultV1>,
    ) -> Result<(), String> {
        if self.slots(input).is_some() {
            Ok(())
        } else {
            self.inner.validate_retry(cas, task, plan, input, previous)
        }
    }
    fn assemble_result(
        &self,
        cas: &Cas,
        state: &review_store::store::task::TaskProjection,
        report: &review_graph::RunReport,
    ) -> Result<TaskResultV1, String> {
        self.inner.assemble_result(cas, state, report)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
        id: &str,
    ) -> Result<(), String> {
        if self.slots(input).is_none() {
            return self.inner.validate_context(cas, input, feedback, id);
        }
        if self.prepare_context(cas, input, feedback)? != id {
            return Err("Provider context changed its admitted capability or input".into());
        }
        Ok(())
    }
    fn validate_context_for_attempt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
        id: &str,
    ) -> Result<(), String> {
        if self.slots(input).is_none() {
            self.inner
                .validate_context_for_attempt(cas, input, attempt, id)
        } else {
            self.validate_context(cas, input, attempt.feedback_ids(), id)
        }
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        if self.slots(input).is_none() {
            return self.inner.validate_output(cas, task, plan, input, output);
        }
        let id = output
            .outputs
            .get("result")
            .and_then(|p| p.artifact_ids.first())
            .ok_or("Provider admission lacks a result")?;
        let artifact = envelope(cas, id)?;
        let receipt = self.receipt(cas, input)?;
        if artifact.artifact_type != receipt.artifact_type()
            || artifact.payload != receipt.payload()?
        {
            return Err("Provider receipt changed its exact capability".into());
        }
        Ok(())
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        self.inner.validate_result(cas, task, result)
    }
}
