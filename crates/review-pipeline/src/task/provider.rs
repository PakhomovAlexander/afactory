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
use review_store::{Cas, store::task::execution::PreparedTaskAttempt};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PROBE_INPUT: &[u8] = b"Reply with exactly: OK\n";

pub struct ProviderTaskDomain<'a> {
    pub graph: &'a CompiledTask,
    pub models: &'a BTreeMap<String, TaskModelBinding<'a>>,
    pub inner: &'a dyn TaskDomain,
}
impl ProviderTaskDomain<'_> {
    fn slots(&self, input: &TaskInvocationV1) -> Option<&BTreeSet<String>> {
        match &self.graph.nodes.get(&input.node)?.operator {
            CompiledOperator::ProviderAdmission { bindings } => Some(bindings),
            _ => None,
        }
    }
    fn receipt(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<TaskProviderAdmissionV1, String> {
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
                || !matches!(&model.binding.execution,review_core::task::plan::WorkerExecutionV1::Model {provider_kind,model: id,effort,..}
                    if model.adapter.provider_kind()==provider_kind && model.adapter.model_settings()==Some((id.clone(),effort.clone())))
            {
                return Err(
                    "Provider admission changed its exact account, model or invocation policy"
                        .into(),
                );
            }
        }
        let receipt = TaskProviderAdmissionV1 {
            plan_id: input.plan_id.clone(),
            bindings: slots.clone(),
            execution: first.binding.execution.clone(),
            invocation_policy_id: first.binding.invocation_policy_id.clone(),
            outcome: ReceiptOutcomeV1::Passed,
        };
        receipt.validate()?;
        Ok(receipt)
    }
    fn probe(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        let prepared = (|| {
            let receipt = self.receipt(cas, input)?;
            let attempt = attempt.ok_or("Provider capability probe has no started Task Attempt")?;
            let context = self.prepare_context(cas, input, &[])?;
            if context != attempt.context_id() {
                return Err("Provider probe lost its exact context".into());
            }
            let model = &self.models[receipt.bindings.first().ok_or("Empty Provider bindings")?];
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
                    outputs: Err(error),
                    charged_tokens: Some(0),
                    raw_artifact_ids: vec![],
                    usage_id: None,
                    feedback_id: None,
                };
            }
        };
        let returned =
            model
                .adapter
                .invoke(cas, directory.path(), PROBE_INPUT.to_vec(), timeout, false);
        let charged_tokens = returned.usage.as_ref().map(|usage| usage.chargeable_tokens);
        let usage = returned
            .usage
            .as_ref()
            .map(|usage| {
                cas.put_json(&serde_json::to_value(usage).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())
            })
            .transpose();
        let mut usage_id = None;
        let outputs = (|| {
            usage_id = usage?;
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
                receipt.invocation_policy_id.clone(),
            ];
            let id = cas
                .put_artifact(
                    TASK_PROVIDER_ADMISSION_V1,
                    invocation_producer(cas, input, attempt)?,
                    refs,
                    None,
                    serde_json::to_value(receipt).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
                .0;
            Ok(BTreeMap::from([(
                "result".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![id],
                    artifact_type: TASK_PROVIDER_ADMISSION_V1.into(),
                    cardinality: review_core::PortCardinality::One,
                    snapshot_id: None,
                },
            )]))
        })();
        TaskWorkOutput {
            outputs,
            charged_tokens,
            raw_artifact_ids: returned.raw_artifact_ids,
            usage_id,
            feedback_id: None,
        }
    }
}
impl TaskOperatorHost for ProviderTaskDomain<'_> {
    fn commit_domain_output(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        id: &str,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        self.inner.commit_domain_output(cas, input, id, output)
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
        cas.put_artifact("af/TaskProviderContext@1",invocation_producer(cas,input,None)?,vec![input.plan_id.clone(),rendered.clone()],None,
            json!({"invocation":input,"capability":receipt,"rendered_id":rendered,"manifest":manifest})).map(|(id,_)|id).map_err(|e|e.to_string())
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        if self.slots(input).is_some() {
            self.probe(cas, input, attempt)
        } else {
            self.inner.execute(cas, input, attempt)
        }
    }
}
impl TaskDomain for ProviderTaskDomain<'_> {
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
        let receipt: TaskProviderAdmissionV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        if artifact.artifact_type != TASK_PROVIDER_ADMISSION_V1
            || receipt != self.receipt(cas, input)?
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
