//! Typed Worker transport shared by Task kinds. A Worker supplies payloads; it cannot assign
//! canonical producer identities, Snapshot lineage, Store receipts, or developer authority.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use review_core::task::execution::TaskInvocationV1;
use review_core::task::feedback::TaskFeedbackCodeV1;
use review_core::{ArtifactEnvelope, Command, Producer};
use review_store::{Cas, validate_envelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ContextManifest, ModelRunner, RunnerError, TokenUsage};

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

pub struct WorkerContract {
    id: String,
    input_schema: Value,
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
            input_schema: input,
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

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
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
    pub reply: Result<WorkerReply, String>,
    pub usage: Option<TokenUsage>,
    pub raw_artifact_ids: Vec<String>,
    pub feedback_code: Option<TaskFeedbackCodeV1>,
}

/// Provider framing is separate from the Worker's business contract. A model adapter returns
/// final-message bytes even when they are not a Reviewer Result; the captured Worker schema
/// performs admission afterwards. Every failure retains raw evidence and any known usage.
pub struct ModelWorkerReturn {
    pub message: Result<Vec<u8>, String>,
    pub usage: Option<TokenUsage>,
    pub raw_artifact_ids: Vec<String>,
}

pub trait WorkerModelAdapter: Send + Sync {
    fn provider_kind(&self) -> &'static str;
    /// Exact model and effort encoded by the adapter's command builder. None cannot satisfy
    /// a plan binding; provider defaults or aliases must be resolved during host admission.
    fn model_settings(&self) -> Option<(String, String)>;
    /// Called only by the host after common Task Attempt admission. The adapter owns security
    /// flags; writable grants only edits inside the supplied source sandbox.
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
    ) -> ModelWorkerReturn;
}

impl ModelWorkerReturn {
    pub fn failed(error: RunnerError) -> Self {
        let raw_artifact_ids = match &error {
            RunnerError::TimedOut { raw_artifact, .. } => raw_artifact.iter().cloned().collect(),
            RunnerError::MalformedOutput { raw_artifact, .. } => vec![raw_artifact.clone()],
            _ => vec![],
        };
        Self {
            message: Err(error.to_string()),
            usage: None,
            raw_artifact_ids,
        }
    }
}

pub fn invoke_model(
    cas: &Cas,
    workdir: &Path,
    adapter: &dyn WorkerModelAdapter,
    contract: &WorkerContract,
    context_id: &str,
    timeout: Duration,
    writable: bool,
) -> WorkerReturn {
    let bytes = match contract.read_context(cas, context_id) {
        Ok((_, bytes)) => bytes,
        Err(error) => {
            return WorkerReturn {
                reply: Err(error),
                usage: Some(TokenUsage::charge_only(0)),
                raw_artifact_ids: vec![],
                feedback_code: Some(TaskFeedbackCodeV1::ContextRejected),
            };
        }
    };
    let returned = adapter.invoke(cas, workdir, bytes, timeout, writable);
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
        reply,
        usage: returned.usage,
        raw_artifact_ids: returned.raw_artifact_ids,
        feedback_code,
    }
}

pub fn invoke_command(
    cas: &Cas,
    workdir: &Path,
    runtime_root: &Path,
    command: &Command,
    contract: &WorkerContract,
    context_id: &str,
    timeout: Duration,
) -> WorkerReturn {
    let prepared = contract
        .read_context(cas, context_id)
        .map_err(RunnerError::Refused);
    let result = prepared.and_then(|(_, bytes)| {
        let mut runner = ModelRunner::new(workdir, timeout);
        for (key, directory) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
            ("TMPDIR", "tmp"),
        ] {
            let path = runtime_root.join(directory);
            std::fs::create_dir_all(&path).map_err(|e| RunnerError::Unavailable(e.to_string()))?;
            runner = runner.with_env(
                key,
                path.to_str().ok_or_else(|| {
                    RunnerError::Refused("Worker runtime path is not UTF-8".into())
                })?,
            );
        }
        runner.capture_with_stdin(cas, command, bytes)
    });
    match result {
        Ok(raw) => {
            let reply = if raw.status.success() {
                contract.validate_reply(&raw.stdout)
            } else {
                Err(format!("Command Worker exited with {}", raw.status))
            };
            let feedback_code = reply.is_err().then_some(if raw.status.success() {
                TaskFeedbackCodeV1::InvalidOutputContract
            } else {
                TaskFeedbackCodeV1::ProcessFailure
            });
            WorkerReturn {
                reply,
                usage: Some(TokenUsage::charge_only(0)),
                raw_artifact_ids: vec![raw.raw_artifact],
                feedback_code,
            }
        }
        Err(error) => WorkerReturn {
            raw_artifact_ids: match &error {
                RunnerError::TimedOut { raw_artifact, .. } => {
                    raw_artifact.iter().cloned().collect()
                }
                _ => vec![],
            },
            reply: Err(error.to_string()),
            usage: Some(TokenUsage::charge_only(0)),
            feedback_code: Some(TaskFeedbackCodeV1::ProcessFailure),
        },
    }
}
