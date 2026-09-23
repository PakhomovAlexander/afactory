//! Typed Worker transport shared by Task kinds. A Worker supplies payloads; it cannot assign
//! canonical producer identities, Snapshot lineage, Store receipts, or developer authority.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use review_core::task::execution::TaskInvocationV1;
use review_core::task::feedback::TaskFeedbackCodeV1;
use review_core::{ArtifactEnvelope, CredentialModeV1, Producer};
use review_store::{Cas, validate_envelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ContextManifest, RunnerError};
pub mod provider;
pub mod usage;

pub const TASK_CONTEXT_V1: &str = "af/TaskContext@1";
pub const MAX_WORKER_BYTES: usize = 1024 * 1024;
pub const WORKER_REPLY_FORMAT: &str = "Return exactly one JSON object: {\"schema\":\"af.worker-reply/1\",\"outputs\":{\"PORT\":[PAYLOAD]}}. Use declared output ports; each PAYLOAD must match output_schemas[PORT].";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerValue {
    pub artifact_id: String,
    pub artifact_type: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub snapshot_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequest {
    pub schema: String,
    pub reply_format: String,
    pub instructions: String,
    pub inputs: BTreeMap<String, Vec<WorkerValue>>,
    pub feedback: Vec<WorkerValue>,
    pub output_schemas: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerReply {
    pub schema: String,
    /// Every item is a payload governed by its declared output port schema. Canonical
    /// envelopes are created only after schema, cardinality and domain admission.
    pub outputs: BTreeMap<String, Vec<Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContext {
    pub invocation: TaskInvocationV1,
    pub feedback_ids: Vec<String>,
    pub rendered_id: String,
    pub contract_id: String,
    pub manifest: ContextManifest,
}

impl TaskContext {
    /// Structural checks for the existing captured payload. The installed host separately
    /// rederives exact inputs, instructions, contracts and feedback before context admission.
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        if !review_core::is_digest(&self.rendered_id)
            || !review_core::is_digest(&self.contract_id)
            || self.feedback_ids.len() > 16
            || self
                .feedback_ids
                .iter()
                .any(|id| !review_core::is_digest(id))
            || self
                .feedback_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.feedback_ids.len()
        {
            return Err("Task context needs exact identities and bounded distinct feedback".into());
        }
        let safe = review_core::json::SAFE_INTEGER_MAX as u64;
        if self.manifest.entries.len() < 3
            || self.manifest.rendered_bytes > MAX_WORKER_BYTES as u64
            || self.manifest.estimated_tokens != self.manifest.rendered_bytes.div_ceil(4)
            || self.manifest.entries.iter().any(|entry| {
                entry.name.is_empty()
                    || entry.required_by.is_empty()
                    || entry
                        .artifact_id
                        .as_deref()
                        .is_some_and(|id| !review_core::is_digest(id))
                    || entry
                        .artifact_type
                        .as_deref()
                        .is_some_and(|ty| !review_core::is_artifact_type(ty))
                    || entry.rendered_bytes > safe
                    || entry.estimated_tokens > safe
                    || entry.estimated_tokens != entry.rendered_bytes.div_ceil(4)
            })
        {
            return Err(
                "Task context manifest exceeds captured counter bounds or changes its estimate"
                    .into(),
            );
        }
        Ok(())
    }
}

pub struct WorkerContract {
    id: String,
    output_schemas: BTreeMap<String, Value>,
    input: jsonschema::Validator,
    outputs: BTreeMap<String, jsonschema::Validator>,
}

/// Contracts are local captured data. External schema resolution would add undeclared
/// filesystem/network inputs while validating a supposedly immutable Worker package.
fn schema_validator(schema: &Value) -> Result<jsonschema::Validator, String> {
    fn local(value: &Value, depth: usize, nodes: &mut usize) -> bool {
        *nodes += 1;
        if depth > 64 || *nodes > 16384 {
            return false;
        }
        match value {
            Value::Object(map) => map.iter().all(|(key, value)| {
                !matches!(
                    key.as_str(),
                    "$id" | "$schema" | "$dynamicRef" | "$recursiveRef"
                ) && (key != "$ref" || value.as_str().is_some_and(|s| s.starts_with("#/")))
                    && local(value, depth + 1, nodes)
            }),
            Value::Array(values) => values.iter().all(|v| local(v, depth + 1, nodes)),
            _ => true,
        }
    }
    if !local(schema, 0, &mut 0) {
        return Err("Worker schema exceeds local resolution bounds".into());
    }
    jsonschema::draft202012::options()
        .build(schema)
        .map_err(|e| e.to_string())
}

impl WorkerContract {
    pub fn capture(
        cas: &Cas,
        input: Value,
        outputs: BTreeMap<String, Value>,
    ) -> Result<Self, String> {
        if outputs.is_empty()
            || outputs.len() > 32
            || outputs.keys().any(|p| !review_core::task::is_name(p))
        {
            return Err("Worker needs bounded named output contracts".into());
        }
        let bytes = serde_json::to_vec(&serde_json::json!({"inputs":input,"outputs":outputs}))
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_WORKER_BYTES {
            return Err("Worker contracts exceed capture bound".into());
        }
        let input_validator = schema_validator(&input)?;
        let output_validators = outputs
            .iter()
            .map(|(port, schema)| schema_validator(schema).map(|v| (port.clone(), v)))
            .collect::<Result<_, _>>()?;
        let id = cas.put(&bytes).map_err(|e| e.to_string())?;
        Ok(Self {
            id,
            output_schemas: outputs,
            input: input_validator,
            outputs: output_validators,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn validate_reply(&self, bytes: &[u8]) -> Result<WorkerReply, String> {
        if bytes.len() > MAX_WORKER_BYTES {
            return Err("Worker reply exceeds byte bound".into());
        }
        let reply: WorkerReply = serde_json::from_slice(bytes)
            .map_err(|e| format!("Invalid typed Worker reply: {e}"))?;
        if reply.schema != "af.worker-reply/1"
            || reply.outputs.keys().any(|p| !self.outputs.contains_key(p))
        {
            return Err("Worker reply has an unsupported contract or undeclared port".into());
        }
        for (port, values) in &reply.outputs {
            if values.len() > 1024 {
                return Err("Worker reply exceeds port item bound".into());
            }
            for value in values {
                self.outputs[port]
                    .validate(value)
                    .map_err(|e| format!("Worker output {port}: {e}"))?;
            }
        }
        Ok(reply)
    }

    /// Renders exact declared ports and admitted retry feedback. No Task goal, repository
    /// traversal, parent transcript or sibling result is added by this generic boundary.
    pub fn prepare(
        &self,
        cas: &Cas,
        invocation: &TaskInvocationV1,
        feedback: &[String],
        instructions: &str,
    ) -> Result<String, String> {
        invocation.validate()?;
        if instructions.len() > MAX_WORKER_BYTES / 4 || feedback.len() > 16 {
            return Err("Worker instructions or feedback exceed context bounds".into());
        }
        let mut manifest = ContextManifest::default();
        manifest.record(
            "instructions",
            "captured Worker package",
            None,
            None,
            instructions.len(),
        );
        let mut inputs = BTreeMap::new();
        for (port, input) in &invocation.inputs {
            let mut values = Vec::new();
            for id in &input.artifact_ids {
                let value = worker_value(cas, id)?;
                if value.artifact_type != input.artifact_type
                    || value.snapshot_id != input.snapshot_id
                {
                    return Err("Worker input differs from its admitted type or Snapshot".into());
                }
                manifest.record(
                    port,
                    "declared input port",
                    Some(id.clone()),
                    Some(value.artifact_type.clone()),
                    serde_json::to_vec(&value).map_err(|e| e.to_string())?.len(),
                );
                values.push(value);
            }
            inputs.insert(port.clone(), values);
        }
        self.input
            .validate(&serde_json::to_value(&inputs).map_err(|e| e.to_string())?)
            .map_err(|e| format!("Worker input schema: {e}"))?;
        let mut feedback_values = Vec::new();
        for id in feedback {
            let value = worker_value(cas, id)?;
            manifest.record(
                "feedback",
                "admitted retry feedback",
                Some(id.clone()),
                Some(value.artifact_type.clone()),
                serde_json::to_vec(&value).map_err(|e| e.to_string())?.len(),
            );
            feedback_values.push(value);
        }
        let schemas_len = serde_json::to_vec(&self.output_schemas)
            .map_err(|e| e.to_string())?
            .len();
        manifest.record(
            "output_schemas",
            "captured Worker result contract",
            Some(self.id.clone()),
            None,
            schemas_len,
        );
        let request = WorkerRequest {
            schema: "af.worker-request/1".into(),
            reply_format: WORKER_REPLY_FORMAT.into(),
            instructions: instructions.into(),
            inputs,
            feedback: feedback_values,
            output_schemas: self.output_schemas.clone(),
        };
        manifest.record(
            "reply_format",
            "installed Worker protocol",
            None,
            None,
            WORKER_REPLY_FORMAT.len(),
        );
        let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_WORKER_BYTES {
            return Err("Worker context exceeds byte bound; narrow the declared input".into());
        }
        manifest.finish(bytes.len());
        let rendered_id = cas.put(&bytes).map_err(|e| e.to_string())?;
        let context = TaskContext {
            invocation: invocation.clone(),
            feedback_ids: feedback.to_vec(),
            rendered_id: rendered_id.clone(),
            contract_id: self.id.clone(),
            manifest,
        };
        context.validate()?;
        let refs = invocation
            .inputs
            .values()
            .flat_map(|v| v.artifact_ids.iter().cloned())
            .chain(feedback.iter().cloned())
            .chain([invocation.plan_id.clone(), rendered_id, self.id.clone()])
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        cas.put_artifact(
            TASK_CONTEXT_V1,
            Producer::KernelOperation {
                run_id: "task-context-v1".into(),
                node_id: Some(invocation.node.clone()),
                operation_id: "render@1".into(),
            },
            refs,
            None,
            serde_json::to_value(context).map_err(|e| e.to_string())?,
        )
        .map(|(id, _)| id)
        .map_err(|e| e.to_string())
    }

    pub fn read_context(&self, cas: &Cas, id: &str) -> Result<(TaskContext, Vec<u8>), String> {
        let envelope: ArtifactEnvelope =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        validate_envelope(&envelope)?;
        if envelope.artifact_id != id || envelope.artifact_type != TASK_CONTEXT_V1 {
            return Err("Worker context identity or type differs".into());
        }
        let context: TaskContext =
            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
        context.validate()?;
        if context.contract_id != self.id {
            return Err("Worker context uses another contract".into());
        }
        let bytes = cas
            .get_bounded(&context.rendered_id, MAX_WORKER_BYTES as u64)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_WORKER_BYTES || context.manifest.rendered_bytes != bytes.len() as u64 {
            return Err("Worker context manifest differs from rendered input".into());
        }
        Ok((context, bytes))
    }
}

fn worker_value(cas: &Cas, id: &str) -> Result<WorkerValue, String> {
    let bytes = cas
        .get_bounded(id, MAX_WORKER_BYTES as u64)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_WORKER_BYTES {
        return Err("Worker input exceeds byte bound".into());
    }
    let envelope: ArtifactEnvelope = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate_envelope(&envelope)?;
    if envelope.artifact_id != id {
        return Err("Worker input identity differs from reference".into());
    }
    Ok(WorkerValue {
        artifact_id: id.into(),
        artifact_type: envelope.artifact_type,
        snapshot_id: envelope.subject_snapshot_id,
        payload: envelope.payload,
    })
}

/// Usage and raw evidence survive both nonzero process exits and output-schema refusal.
pub struct WorkerReturn {
    pub usage_observation: Option<review_core::task::usage::TaskUsageObservationV1>,
    pub reply: Result<WorkerReply, String>,
    pub usage: Option<review_core::task::usage::TaskTokenUsageV3>,
    pub raw_artifact_ids: Vec<String>,
    pub feedback_code: Option<TaskFeedbackCodeV1>,
}

/// Provider framing is separate from the Worker's business contract. A model adapter returns
/// final-message bytes even when they are not a Reviewer Result; the captured Worker schema
/// performs admission afterwards. Every failure retains raw evidence and any known usage.
pub struct ModelWorkerReturn {
    /// Explicit native billing completeness when reporting was malformed or partial. None
    /// means `usage` is either complete or wholly unavailable.
    pub usage_observation: Option<review_core::task::usage::TaskUsageObservationV1>,
    pub message: Result<Vec<u8>, String>,
    pub usage: Option<review_core::task::usage::TaskTokenUsageV3>,
    pub raw_artifact_ids: Vec<String>,
}

/// The sandbox authority one model Attempt runs with. The host derives it from the Worker's
/// captured effects alone; each adapter maps it onto its own tool and sandbox flags, so a
/// package can never name a tool, a permission mode or an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerAccess {
    /// Read the materialized source; no shell and no edits.
    ReadOnly,
    /// Read the source and run a shell inside an ephemeral-write clone. Nothing is sealed
    /// back: the kernel refuses the Attempt if the declared source changed.
    ExecuteChecks,
    /// Edit files inside the sandbox; the kernel captures the sealed tree as the candidate.
    WriteSource,
}

impl WorkerAccess {
    /// Whether the adapter must let the process write inside its sandbox root.
    pub fn writes_sandbox(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }
}

pub trait WorkerModelAdapter: Send + Sync {
    /// Credential boundary this adapter actually provides. Model transports run trusted and
    /// may hold ambient Provider credentials.
    fn credential_mode(&self) -> CredentialModeV1 {
        CredentialModeV1::TrustedUnsafe
    }

    fn provider_kind(&self) -> &'static str;
    /// Exact model and effort encoded by the adapter's command builder. None cannot satisfy
    /// a plan binding; provider defaults or aliases must be resolved during host admission.
    fn model_settings(&self) -> Option<(String, String)>;
    /// Called only by the host after common Task Attempt admission. The adapter owns security
    /// flags and derives its tools from `access` alone; no access grants anything outside the
    /// supplied source sandbox. An adapter honors
    /// both controls or refuses before it spawns: `cancellation` must stop an in-flight
    /// process (a preflight flag check alone is not support), and `environment` carries the
    /// sandbox-local, non-secret variables the kernel resolved for this exact Attempt, such as
    /// `CARGO_TARGET_DIR` pointing at a cloned Build Cache, which must reach the process.
    #[allow(clippy::too_many_arguments)]
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn;
}

impl ModelWorkerReturn {
    pub fn failed(error: RunnerError) -> Self {
        Self {
            usage_observation: None,
            message: Err(error.to_string()),
            usage: None,
            raw_artifact_ids: vec![],
        }
    }
}

/// Validates the captured context and the returned output around one model call. This path
/// gives model Workers no sandbox-local environment; a Review Task reviewer is dispatched
/// through the Campaign Review host instead, which forwards its Build Cache location.
#[allow(clippy::too_many_arguments)]
pub fn invoke_model(
    cas: &Cas,
    workdir: &Path,
    adapter: &dyn WorkerModelAdapter,
    contract: &WorkerContract,
    context_id: &str,
    timeout: Duration,
    access: WorkerAccess,
    cancellation: Option<&AtomicBool>,
) -> WorkerReturn {
    let bytes = match contract.read_context(cas, context_id) {
        Ok((_, bytes)) => bytes,
        Err(error) => {
            return WorkerReturn {
                usage_observation: None,
                reply: Err(error),
                usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
                raw_artifact_ids: vec![],
                feedback_code: Some(TaskFeedbackCodeV1::ContextRejected),
            };
        }
    };
    let returned = adapter.invoke(cas, workdir, bytes, timeout, access, cancellation, &[]);
    let (reply, feedback_code) = match returned.message {
        Ok(bytes) => {
            let reply = contract.validate_reply(&bytes);
            let code = reply
                .is_err()
                .then_some(TaskFeedbackCodeV1::InvalidOutputContract);
            (reply, code)
        }
        Err(error) => (Err(error), Some(TaskFeedbackCodeV1::ProviderFailure)),
    };
    WorkerReturn {
        usage_observation: returned.usage_observation,
        reply,
        usage: returned.usage,
        raw_artifact_ids: returned.raw_artifact_ids,
        feedback_code,
    }
}

mod command;
pub use command::{invoke_command, invoke_command_bytes};
