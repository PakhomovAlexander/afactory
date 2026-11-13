//! Typed Worker transport shared by Task kinds. A Worker supplies payloads; it cannot assign
//! canonical producer identities, Snapshot lineage, Store receipts, or developer authority.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use af_contracts::task::execution::TaskInvocationV1;
use af_contracts::task::feedback::TaskFeedbackCodeV1;
use af_contracts::{ArtifactEnvelope, CredentialModeV1, Producer};
use af_store::{Cas, validate_envelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ContextManifest, RunnerError};
pub mod provider;
pub mod reply_schema;
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
        deserialize_with = "af_contracts::task::present_option"
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

impl WorkerRequest {
    /// The request [`WorkerContract::prepare`] renders: the installed protocol fields around a
    /// Worker's instructions, its declared inputs, its admitted retry feedback and its captured
    /// output schemas. A plan-time check renders the same request from the parts known then, so
    /// both apply [`MAX_WORKER_BYTES`] to one accounting (ADR-0160).
    pub fn new(
        instructions: &str,
        inputs: BTreeMap<String, Vec<WorkerValue>>,
        feedback: Vec<WorkerValue>,
        output_schemas: BTreeMap<String, Value>,
    ) -> Self {
        Self {
            schema: "af.worker-request/1".into(),
            reply_format: WORKER_REPLY_FORMAT.into(),
            instructions: instructions.into(),
            inputs,
            feedback,
            output_schemas,
        }
    }

    /// The exact bytes the Worker reads, JSON escaping and every wrapper included: what the cap
    /// measures.
    pub fn rendered(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }
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
        if !af_contracts::is_digest(&self.rendered_id)
            || !af_contracts::is_digest(&self.contract_id)
            || self.feedback_ids.len() > 16
            || self
                .feedback_ids
                .iter()
                .any(|id| !af_contracts::is_digest(id))
            || self
                .feedback_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.feedback_ids.len()
        {
            return Err("Task context needs exact identities and bounded distinct feedback".into());
        }
        let safe = af_contracts::json::SAFE_INTEGER_MAX as u64;
        if self.manifest.entries.len() < 3
            || self.manifest.rendered_bytes > MAX_WORKER_BYTES as u64
            || self.manifest.estimated_tokens != self.manifest.rendered_bytes.div_ceil(4)
            || self.manifest.entries.iter().any(|entry| {
                entry.name.is_empty()
                    || entry.required_by.is_empty()
                    || entry
                        .artifact_id
                        .as_deref()
                        .is_some_and(|id| !af_contracts::is_digest(id))
                    || entry
                        .artifact_type
                        .as_deref()
                        .is_some_and(|ty| !af_contracts::is_artifact_type(ty))
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
pub fn schema_validator(schema: &Value) -> Result<jsonschema::Validator, String> {
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
            || outputs.keys().any(|p| !af_contracts::task::is_name(p))
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
        let mut reply: WorkerReply = serde_json::from_slice(bytes)
            .map_err(|e| format!("Invalid typed Worker reply: {e}"))?;
        if reply.schema != "af.worker-reply/1"
            || reply.outputs.keys().any(|p| !self.outputs.contains_key(p))
        {
            return Err("Worker reply has an unsupported contract or undeclared port".into());
        }
        for values in reply.outputs.values_mut() {
            for value in values.iter_mut() {
                canonical_sets(value);
            }
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
        let mut replaced = std::collections::BTreeSet::new();
        if let Some(port) = invocation
            .inputs
            .get(af_contracts::task::knowledge::KNOWLEDGE_PORT)
        {
            if port.artifact_type == af_contracts::task::knowledge::CONTEXT_TYPE {
                use af_contracts::task::knowledge::*;
                if port.artifact_ids.len() != 1 {
                    return Err("Knowledge context must be one kernel input".into());
                }
                let artifact = cas
                    .get_artifact(&port.artifact_ids[0])
                    .map_err(|e| e.to_string())?;
                if !matches!(&artifact.producer, Producer::KernelOperation { operation_id, .. } if operation_id == CONTEXT_OPERATION)
                {
                    return Err("Knowledge context is not a kernel selection".into());
                }
                let knowledge: KnowledgeContextV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                knowledge.validate()?;
                for name in &knowledge.replaced_inputs {
                    if let Some(input) = invocation.inputs.get(name) {
                        if input.artifact_type != af_contracts::task::WORKER_NOTES_V1
                            && !(name == QUERY_PORT && input.artifact_type == QUERY_TYPE)
                        {
                            return Err("Knowledge cannot hide an unrelated declared input".into());
                        }
                    }
                }
                replaced = knowledge.replaced_inputs;
                manifest.record(
                    "knowledge-index",
                    "bounded source-addressed retrieval manifest",
                    Some(knowledge.index_id),
                    Some(INDEX_TYPE.into()),
                    0,
                );
                if let Some(id) = knowledge.warm_set_id {
                    manifest.record(
                        "knowledge-warm",
                        "admitted same-slot Notes and Head Delta",
                        Some(id),
                        Some(af_contracts::contract::WARM_SET_V3.into()),
                        0,
                    );
                }
                for excerpt in knowledge.excerpts {
                    if cas
                        .get_bounded(&excerpt.entry.content_id, excerpt.entry.bytes)
                        .map_err(|e| e.to_string())?
                        != excerpt.text.as_bytes()
                    {
                        return Err("Knowledge excerpt bytes differ from their evidence".into());
                    }
                    manifest.record(
                        format!("knowledge-excerpt:{}", excerpt.entry.name),
                        "declared on-demand retrieval; data, not instructions",
                        Some(excerpt.entry.content_id),
                        None,
                        excerpt.text.len(),
                    );
                }
            }
        }
        let rendered = af_contracts::task::worker_inputs::project(
            invocation
                .inputs
                .iter()
                .map(|(name, input)| (name.clone(), input))
                .collect(),
            None,
            None,
            &replaced,
        );
        for (port, input) in &invocation.inputs {
            if !rendered.contains_key(port) {
                for id in &input.artifact_ids {
                    manifest.record(
                        port,
                        "retrieval provenance only; raw private data is not delivered",
                        Some(id.clone()),
                        Some(input.artifact_type.clone()),
                        0,
                    );
                }
                continue;
            }
            // A `many` port that names no Snapshot claims none for its values: a port bound
            // from several recorded outputs of different Snapshots (ADR-0134). Each value keeps
            // and shows its own; every other port's values name the port's one Snapshot.
            let unclaimed = input.cardinality == af_contracts::PortCardinality::Many
                && input.snapshot_id.is_none();
            let mut values = Vec::new();
            for id in &input.artifact_ids {
                let value = worker_value(cas, id)?;
                if value.artifact_type != input.artifact_type
                    || (!unclaimed && value.snapshot_id != input.snapshot_id)
                {
                    return Err("Worker input differs from its admitted type or Snapshot".into());
                }
                // The kernel's Diff Subject is delivered on its one port and describes the change
                // to the Snapshot that port names (ADR-0254).
                if value.artifact_type == af_contracts::task::diff_subject::DIFF_SUBJECT_V1 {
                    let subject: af_contracts::task::diff_subject::DiffSubjectV1 =
                        serde_json::from_value(value.payload.clone())
                            .map_err(|e| format!("Worker input {port}: {e}"))?;
                    subject.validate()?;
                    if port != af_contracts::task::diff_subject::DIFF_SUBJECT_PORT
                        || input.snapshot_id.as_deref() != Some(subject.snapshot_id.as_str())
                    {
                        return Err(format!(
                            "Worker input {port} is not the Diff Subject of its port's Snapshot"
                        ));
                    }
                }
                // A Project Brief is the one input a pipeline adds for orientation alone, so
                // its entry names the port that asked for it (ADR-0160).
                let required_by =
                    if value.artifact_type == af_contracts::task::project_brief::PROJECT_BRIEF_V1 {
                        format!(
                            "{}{port}",
                            af_contracts::task::project_brief::PORT_REQUIRED_BY_PREFIX
                        )
                    } else {
                        "declared input port".into()
                    };
                manifest.record(
                    port,
                    required_by,
                    Some(id.clone()),
                    Some(value.artifact_type.clone()),
                    serde_json::to_vec(&value).map_err(|e| e.to_string())?.len(),
                );
                if port == af_contracts::task::development::DEVELOPMENT_WARM_PORT
                    && value.artifact_type == af_contracts::contract::WARM_SET_V3
                {
                    let set: af_contracts::WarmSetV3 =
                        serde_json::from_value(value.payload.clone())
                            .map_err(|error| error.to_string())?;
                    set.validate()?;
                    for (name, layer) in &set.development {
                        if let af_contracts::task::development::DevelopmentCacheSelectionRecordV1::Selected { artifact_id, manifest_id, .. } = layer {
                            manifest.record(format!("development-cache:{name}"), "declared private immutable cache clone", Some(artifact_id.clone()), None, 0);
                            manifest.record(format!("development-cache-manifest:{name}"), "exact files cloned into the private root", Some(manifest_id.clone()), None, 0);
                        }
                    }
                }
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
            // The typed receipt an admitted feedback names, such as a draft validation, is the
            // retry's guidance too: rendered from the feedback's own references (ADR-0208).
            for evidence in feedback_evidence(cas, id)? {
                manifest.record(
                    "feedback",
                    "admitted retry feedback evidence",
                    Some(evidence.artifact_id.clone()),
                    Some(evidence.artifact_type.clone()),
                    serde_json::to_vec(&evidence)
                        .map_err(|e| e.to_string())?
                        .len(),
                );
                feedback_values.push(evidence);
            }
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
        let request = WorkerRequest::new(
            instructions,
            inputs,
            feedback_values,
            self.output_schemas.clone(),
        );
        manifest.record(
            "reply_format",
            "installed Worker protocol",
            None,
            None,
            WORKER_REPLY_FORMAT.len(),
        );
        let bytes = request.rendered()?;
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

/// The artifact types a retry feedback may carry to its Worker beside itself: typed receipts of
/// why the previous reply was refused, never a diagnostic or a transcript.
const FEEDBACK_EVIDENCE_TYPES: [&str; 1] =
    [af_contracts::task::report_domain::REPORT_DRAFT_VALIDATION_V1];

/// The typed receipts an admitted retry feedback references, in reference order.
fn feedback_evidence(cas: &Cas, id: &str) -> Result<Vec<WorkerValue>, String> {
    let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
    if envelope.artifact_type != af_contracts::task::feedback::TASK_RETRY_FEEDBACK_V1 {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    for reference in &envelope.input_artifacts {
        // A contract blob is no envelope, and a context is no receipt: neither is evidence.
        let referenced = cas
            .get_optional_artifact(reference)
            .map_err(|e| e.to_string())?;
        if referenced.is_some_and(|referenced| {
            FEEDBACK_EVIDENCE_TYPES.contains(&referenced.artifact_type.as_str())
        }) {
            values.push(worker_value(cas, reference)?);
        }
    }
    Ok(values)
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

/// What one native call learned beyond its reply: whether its failure was transient, so the
/// kernel may call again within the same Attempt, and the facts it did about its Provider that
/// the kernel records as typed Task events (ADR-0208). Neither changes the Attempt's charge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallNotes {
    /// The final native failure, carried to settlement independently of usage and prose.
    pub native_failure: Option<crate::native_failure::NativeFailureKind>,
    /// The call failed for a reason the same input may get past after a wait.
    pub transient: Option<crate::native_failure::Transient>,
    /// What the call did about its Provider without failing, in order.
    pub events: Vec<af_contracts::task::provider_event::ProviderEventV1>,
    /// The policy category of a safety-classifier refusal, when the native stream named one
    /// (`stop_details.category`).
    pub refusal_category: Option<String>,
    /// The call failed because the Provider's machine environment failed af, not the model or
    /// the login: its CLI could not start or serve its protocol, or its identity probes proved
    /// nothing within their bounded retries (ADR-0208, ADR-0257).
    pub environment: bool,
}

impl CallNotes {
    /// The notes of a call that followed `self` in the same Attempt: its events after these,
    /// and only its own transient failure.
    pub fn then(mut self, later: CallNotes) -> CallNotes {
        self.events.extend(later.events);
        self.transient = later.transient;
        self.environment = later.environment;
        self.native_failure = later.native_failure;
        self.refusal_category = later.refusal_category.or(self.refusal_category);
        self
    }
}

/// Usage and raw evidence survive both nonzero process exits and output-schema refusal.
pub struct WorkerReturn {
    pub usage_observation: Option<af_contracts::task::usage::TaskUsageObservationV1>,
    pub reply: Result<WorkerReply, String>,
    pub usage: Option<af_contracts::task::usage::TaskTokenUsageV3>,
    pub raw_artifact_ids: Vec<String>,
    pub feedback_code: Option<TaskFeedbackCodeV1>,
    /// The classified native failure the call ended with, if it failed natively.
    pub native_failure: Option<crate::native_failure::NativeFailureKind>,
    pub notes: CallNotes,
}

/// Provider framing is separate from the Worker's business contract. A model adapter returns
/// final-message bytes even when they are not a Reviewer Result; the captured Worker schema
/// performs admission afterwards. Every failure retains raw evidence and any known usage.
pub struct ModelWorkerReturn {
    /// Explicit native billing completeness when reporting was malformed or partial. None
    /// means `usage` is either complete or wholly unavailable.
    pub usage_observation: Option<af_contracts::task::usage::TaskUsageObservationV1>,
    pub message: Result<Vec<u8>, String>,
    pub usage: Option<af_contracts::task::usage::TaskTokenUsageV3>,
    pub raw_artifact_ids: Vec<String>,
    /// The classified native failure, from the client's own error fields or streams, when the
    /// native call failed. It names the cause of unknown usage (ADR-0143); the raw Provider
    /// message never travels with it.
    pub native_failure: Option<crate::native_failure::NativeFailureKind>,
    pub notes: CallNotes,
}

/// The return of a native program that could not be started at all. It received no input and
/// spent nothing, so a caller that holds another executable for the same Provider may invoke
/// that one instead.
pub struct Unstarted(pub Box<ModelWorkerReturn>);

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
    /// Edit files and run a shell inside the sandbox, so the Worker can format, lint and test
    /// what it wrote. The kernel captures the sealed tree as the candidate, minus the scratch a
    /// shell leaves: anything added beneath a top-level name the source did not have.
    WriteSourceWithShell,
}

impl WorkerAccess {
    /// Whether the adapter must let the process write inside its sandbox root.
    pub fn writes_sandbox(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    /// Whether the Worker runs a shell: its process group then dies with the Attempt.
    pub fn has_shell(self) -> bool {
        matches!(self, Self::ExecuteChecks | Self::WriteSourceWithShell)
    }
}

/// Kernel-owned remaining reservation for one native call. Never decoded from Worker input
/// or environment. Pi separately checks the safe JSON integer range of its host protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemainingTokenAllowance(u64);
impl RemainingTokenAllowance {
    pub fn new(tokens: u64) -> Result<Self, String> {
        if tokens == 0 {
            return Err("Missing or invalid remaining token allowance".into());
        }
        Ok(Self(tokens))
    }
    pub fn tokens(self) -> u64 {
        self.0
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

    /// Trusted dispatch with the actual remaining reservation. Existing adapters keep their
    /// native behavior; Pi requires this path for every paid invocation, including admission.
    #[allow(clippy::too_many_arguments)]
    fn invoke_with_allowance(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
        _allowance: RemainingTokenAllowance,
        contract: reply_schema::ReplyContract,
    ) -> Result<ModelWorkerReturn, Unstarted> {
        match contract {
            reply_schema::ReplyContract::Request => self.invoke_started(
                cas,
                workdir,
                input,
                timeout,
                access,
                cancellation,
                environment,
            ),
        }
    }

    /// Optional live metadata for an exact host-bound Attempt. Unsupported adapters ignore it;
    /// they never receive a path or permission to publish raw content through this channel.
    #[allow(clippy::too_many_arguments)]
    fn invoke_with_activity(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
        allowance: RemainingTokenAllowance,
        contract: reply_schema::ReplyContract,
        _activity: Option<crate::activity::Sink>,
    ) -> Result<ModelWorkerReturn, Unstarted> {
        self.invoke_with_allowance(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
            allowance,
            contract,
        )
    }

    /// `invoke`, with a program that could not be started told apart from every other return.
    /// An adapter that spawns a native client overrides this; the default never reports one.
    #[allow(clippy::too_many_arguments)]
    fn invoke_started(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
    ) -> Result<ModelWorkerReturn, Unstarted> {
        Ok(self.invoke(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
        ))
    }

    /// `invoke_started` with the reply natively constrained to `contract`: `invoke` and
    /// `invoke_started` are this with [`ReplyContract::Request`]. An
    /// adapter that spawns a native client overrides this and passes the contract's schema to
    /// its CLI (Claude Code `--json-schema`, Codex `--output-schema`); the default, for a double
    /// that spawns nothing, calls `invoke_started` unconstrained.
    ///
    /// [`ReplyContract::Request`]: reply_schema::ReplyContract::Request
    #[allow(clippy::too_many_arguments)]
    fn invoke_constrained(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
        _contract: reply_schema::ReplyContract,
    ) -> Result<ModelWorkerReturn, Unstarted> {
        self.invoke_started(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
        )
    }
}

impl ModelWorkerReturn {
    pub fn failed(error: RunnerError) -> Self {
        Self {
            usage_observation: None,
            message: Err(error.to_string()),
            usage: None,
            raw_artifact_ids: vec![],
            native_failure: None,
            notes: CallNotes::default(),
        }
    }
}

/// Installed domain policy for semantic calls inside one declared Worker Attempt.
/// This is runtime policy, never a Worker-controlled or persisted field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCallPolicy {
    SingleCall,
    RecoverWithinAttempt,
}

/// Validates captured context and output with the ordinary in-Attempt recovery policy.
/// Structured domains use [`invoke_model_with_policy`] to require a single call instead.
/// This entry point supplies no sandbox-local environment.
///
/// A reply that does not parse is read once more tolerantly — the first balanced JSON object of
/// a reply that carries prose around it — and, failing that, the same Worker is asked once more
/// within the same Attempt: its deadline, its `reserved_tokens` and its cancellation, with the
/// parse error and the offending excerpt as one more feedback value (ADR-0208). Both replies are
/// kept as raw evidence and named in the diagnostic.
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
    reserved_tokens: u64,
) -> WorkerReturn {
    invoke_model_with_environment(
        cas,
        workdir,
        adapter,
        contract,
        context_id,
        timeout,
        access,
        cancellation,
        reserved_tokens,
        &[],
    )
}

/// Invoke with explicit host-owned non-secret grants, retained by typed-result re-asks.
#[allow(clippy::too_many_arguments)]
pub fn invoke_model_with_environment(
    cas: &Cas,
    workdir: &Path,
    adapter: &dyn WorkerModelAdapter,
    contract: &WorkerContract,
    context_id: &str,
    timeout: Duration,
    access: WorkerAccess,
    cancellation: Option<&AtomicBool>,
    reserved_tokens: u64,
    environment: &[(String, String)],
) -> WorkerReturn {
    invoke_model_with_policy(
        cas,
        workdir,
        adapter,
        contract,
        context_id,
        timeout,
        access,
        cancellation,
        reserved_tokens,
        environment,
        ModelCallPolicy::RecoverWithinAttempt,
    )
}

/// Invoke under installed domain authority, including its in-Attempt model-call limit.
#[allow(clippy::too_many_arguments)]
pub fn invoke_model_with_policy(
    cas: &Cas,
    workdir: &Path,
    adapter: &dyn WorkerModelAdapter,
    contract: &WorkerContract,
    context_id: &str,
    timeout: Duration,
    access: WorkerAccess,
    cancellation: Option<&AtomicBool>,
    reserved_tokens: u64,
    environment: &[(String, String)],
    policy: ModelCallPolicy,
) -> WorkerReturn {
    invoke_model_with_activity(
        cas,
        workdir,
        adapter,
        contract,
        context_id,
        timeout,
        access,
        cancellation,
        reserved_tokens,
        environment,
        policy,
        None,
    )
}

/// Task runtime entry with an optional, exact Attempt activity binding.
#[allow(clippy::too_many_arguments)]
pub fn invoke_model_with_activity(
    cas: &Cas,
    workdir: &Path,
    adapter: &dyn WorkerModelAdapter,
    contract: &WorkerContract,
    context_id: &str,
    timeout: Duration,
    access: WorkerAccess,
    cancellation: Option<&AtomicBool>,
    reserved_tokens: u64,
    environment: &[(String, String)],
    policy: ModelCallPolicy,
    activity: Option<crate::activity::Sink>,
) -> WorkerReturn {
    let bytes = match contract.read_context(cas, context_id) {
        Ok((_, bytes)) => bytes,
        Err(error) => {
            return WorkerReturn {
                notes: Default::default(),
                usage_observation: None,
                reply: Err(error),
                usage: Some(af_contracts::task::usage::TaskTokenUsageV3::charge_only(0)),
                raw_artifact_ids: vec![],
                feedback_code: Some(TaskFeedbackCodeV1::ContextRejected),
                native_failure: None,
            };
        }
    };
    reask::invoke(
        cas,
        workdir,
        adapter,
        contract,
        bytes,
        timeout,
        access,
        cancellation,
        reserved_tokens,
        environment,
        policy,
        activity,
    )
}

mod reask;
pub use reask::{extract_object, merge_usage};

mod command;
pub use command::{invoke_command, invoke_command_bytes};

/// Admit the current tagged citation set in the domain's canonical order. Invalid members
/// remain untouched so schema admission reports them rather than erasing evidence.
fn canonical_sets(value: &mut Value) {
    if value.get("schema").and_then(Value::as_str) != Some("af.document-draft/3") {
        return;
    }
    if let Some(citations) = value.get_mut("citations")
        && let Ok(mut typed) = serde_json::from_value::<
            Vec<af_contracts::task::document::DraftCitation>,
        >(citations.clone())
    {
        typed.sort();
        typed.dedup();
        *citations = serde_json::to_value(typed).expect("citation serialization");
    }
}

#[cfg(test)]
mod canonical_set_tests {
    use super::*;

    #[test]
    fn call_notes_keep_only_the_final_native_failure_after_another_call() {
        use crate::native_failure::NativeFailureKind;
        for final_kind in [None, Some(NativeFailureKind::Network)] {
            let first = CallNotes {
                native_failure: Some(NativeFailureKind::Quota),
                ..Default::default()
            };
            let last = CallNotes {
                native_failure: final_kind,
                ..Default::default()
            };
            assert_eq!(first.then(last).native_failure, final_kind);
        }
    }

    #[test]
    fn a_drafts_sets_are_admitted_in_canonical_order_and_nothing_else_is_touched() {
        let mut draft = serde_json::json!({
            "schema": "af.document-draft/3",
            "title": "t",
            "sections": [{"heading": "Findings", "body": "b"}],
            "citations": [
                {"kind":"source","name":"r2"}, {"kind":"source","name":"r1"},
                {"kind":"source","name":"r2"}, {"kind":"source","name":"bench"},
                {"kind":"repository","location":{"path":"src/b.rs","line":3}},
                {"kind":"repository","location":{"path":"src/a.rs","line":9}},
                {"kind":"repository","location":{"path":"src/a.rs"}},
                {"kind":"repository","location":{"path":"src/a.rs","line":9}}
            ]
        });
        canonical_sets(&mut draft);
        assert_eq!(
            draft["citations"],
            serde_json::json!([
                {"kind":"source","name":"bench"}, {"kind":"source","name":"r1"},
                {"kind":"source","name":"r2"},
                {"kind":"repository","location":{"path":"src/a.rs"}},
                {"kind":"repository","location":{"path":"src/a.rs","line":9}},
                {"kind":"repository","location":{"path":"src/b.rs","line":3}}
            ])
        );
        let mut other = serde_json::json!({"schema": "af.worker-reply/1", "citations": ["b", "a"]});
        let before = other.clone();
        canonical_sets(&mut other);
        assert_eq!(other, before, "only a draft is a set-carrying reply");
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;
    use af_contracts::PortCardinality;
    use af_contracts::task::ArtifactInputV1;

    /// The plan-time cap check renders a request with [`WorkerRequest::new`]; dispatch renders
    /// the same bytes, escaping and output schemas included, so one accounting decides both.
    #[test]
    fn a_request_built_from_its_parts_is_the_one_prepare_renders() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let schemas = BTreeMap::from([(
            "answer".to_string(),
            serde_json::json!({"type": "object", "description": "a \"quoted\" schema"}),
        )]);
        let contract =
            WorkerContract::capture(&cas, serde_json::json!({"type": "object"}), schemas.clone())
                .unwrap();
        let payload = serde_json::json!({"text": "quotes \" and a line\nbreak"});
        let (id, _) = cas
            .put_artifact(
                "af/Source@1",
                Producer::KernelOperation {
                    run_id: "request-fixture".into(),
                    node_id: None,
                    operation_id: "capture@1".into(),
                },
                vec![],
                None,
                payload.clone(),
            )
            .unwrap();
        let invocation = TaskInvocationV1 {
            plan_id: cas.put(b"captured plan").unwrap(),
            node: "root.worker".into(),
            inputs: BTreeMap::from([(
                "source".to_string(),
                ArtifactInputV1 {
                    artifact_ids: vec![id.clone()],
                    artifact_type: "af/Source@1".into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: None,
                },
            )]),
        };
        let instructions = "Read \"source\".\n";
        let context = contract
            .prepare(&cas, &invocation, &[], instructions)
            .unwrap();
        let (context, rendered) = contract.read_context(&cas, &context).unwrap();
        let value = WorkerValue {
            artifact_id: id,
            artifact_type: "af/Source@1".into(),
            snapshot_id: None,
            payload,
        };
        let request = WorkerRequest::new(
            instructions,
            BTreeMap::from([("source".to_string(), vec![value])]),
            Vec::new(),
            schemas,
        );
        assert_eq!(request.rendered().unwrap(), rendered);
        assert_eq!(context.manifest.rendered_bytes, rendered.len() as u64);
        // The payload sum is not the request: wrappers, protocol fields and escaping add bytes.
        let sum = instructions.len() + serde_json::to_vec(&request.inputs).unwrap().len();
        assert!(rendered.len() > sum);
    }

    /// ADR-0254: the kernel's Diff Subject is delivered like any typed input, on its one port,
    /// and only when its document describes the Snapshot that port names.
    #[test]
    fn a_diff_subject_is_delivered_only_on_its_port_and_for_its_snapshot() {
        use af_contracts::task::diff_subject::*;
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let contract = WorkerContract::capture(
            &cas,
            serde_json::json!({"type": "object"}),
            BTreeMap::from([("answer".to_string(), serde_json::json!({"type": "object"}))]),
        )
        .unwrap();
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let document = |snapshot: char| {
            serde_json::json!({"schema": DIFF_SUBJECT_SCHEMA, "base_snapshot_id": digest('1'),
                "snapshot_id": digest(snapshot), "base": DIFF_SUBJECT_BASE,
                "changes": [{"path": "lib.rs", "before": digest('3'), "after": digest('4')}]})
        };
        let prepare = |port: &str, snapshot: char| {
            let (id, _) = cas
                .put_artifact(
                    DIFF_SUBJECT_V1,
                    Producer::KernelOperation {
                        run_id: "request-fixture".into(),
                        node_id: Some("root.judge".into()),
                        operation_id: DIFF_SUBJECT_OPERATION.into(),
                    },
                    vec![],
                    Some(digest('2')),
                    document(snapshot),
                )
                .unwrap();
            let invocation = TaskInvocationV1 {
                plan_id: cas.put(b"captured plan").unwrap(),
                node: "root.judge".into(),
                inputs: BTreeMap::from([(
                    port.to_string(),
                    ArtifactInputV1 {
                        artifact_ids: vec![id],
                        artifact_type: DIFF_SUBJECT_V1.into(),
                        cardinality: PortCardinality::One,
                        snapshot_id: Some(digest('2')),
                    },
                )]),
            };
            contract.prepare(&cas, &invocation, &[], "Judge it.")
        };
        let context = prepare(DIFF_SUBJECT_PORT, '2').unwrap();
        let (_, rendered) = contract.read_context(&cas, &context).unwrap();
        let request: WorkerRequest = serde_json::from_slice(&rendered).unwrap();
        assert_eq!(request.inputs[DIFF_SUBJECT_PORT][0].payload, document('2'));
        let elsewhere = prepare("change", '2').unwrap_err();
        assert!(elsewhere.contains("not the Diff Subject"), "{elsewhere}");
        let other = prepare(DIFF_SUBJECT_PORT, '5').unwrap_err();
        assert!(other.contains("not the Diff Subject"), "{other}");
    }

    /// ADR-0208: an admitted retry feedback that names a draft validation receipt carries it to
    /// the retried Worker beside itself; a reference of any other type does not travel.
    #[test]
    fn a_feedbacks_typed_receipt_is_rendered_beside_it() {
        use af_contracts::task::feedback::*;
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let contract = WorkerContract::capture(
            &cas,
            serde_json::json!({"type": "object"}),
            BTreeMap::from([("draft".to_string(), serde_json::json!({"type": "object"}))]),
        )
        .unwrap();
        let producer = Producer::KernelOperation {
            run_id: "feedback-fixture".into(),
            node_id: None,
            operation_id: "fixture@1".into(),
        };
        let receipt = serde_json::json!({"schema": "af/ReportDraftValidation@1", "fixture": true});
        let (receipt_id, _) = cas
            .put_artifact(
                af_contracts::task::report_domain::REPORT_DRAFT_VALIDATION_V1,
                producer.clone(),
                vec![],
                None,
                receipt.clone(),
            )
            .unwrap();
        let (other_id, _) = cas
            .put_artifact(
                "af/Other@1",
                producer.clone(),
                vec![],
                None,
                serde_json::json!({}),
            )
            .unwrap();
        let feedback = TaskRetryFeedbackV1 {
            attempt_id: "a".repeat(26),
            contract_id: contract.id().into(),
            code: TaskFeedbackCodeV1::OutputAdmissionRejected,
            compiler: None,
        };
        let (feedback_id, _) = cas
            .put_artifact(
                TASK_RETRY_FEEDBACK_V1,
                producer,
                vec![contract.id().into(), other_id, receipt_id.clone()],
                None,
                serde_json::to_value(&feedback).unwrap(),
            )
            .unwrap();
        let invocation = TaskInvocationV1 {
            plan_id: cas.put(b"captured plan").unwrap(),
            node: "root.author".into(),
            inputs: BTreeMap::new(),
        };
        let context = contract
            .prepare(&cas, &invocation, &[feedback_id.clone()], "Write it.")
            .unwrap();
        let (context, rendered) = contract.read_context(&cas, &context).unwrap();
        let request: WorkerRequest = serde_json::from_slice(&rendered).unwrap();
        let carried: Vec<_> = request
            .feedback
            .iter()
            .map(|value| (value.artifact_id.clone(), value.artifact_type.clone()))
            .collect();
        assert_eq!(
            carried,
            [
                (feedback_id.clone(), TASK_RETRY_FEEDBACK_V1.to_string()),
                (
                    receipt_id.clone(),
                    af_contracts::task::report_domain::REPORT_DRAFT_VALIDATION_V1.to_string()
                ),
            ]
        );
        assert_eq!(request.feedback[1].payload, receipt);
        assert_eq!(context.feedback_ids, [feedback_id]);
        assert!(
            context
                .manifest
                .entries
                .iter()
                .any(
                    |entry| entry.artifact_id.as_deref() == Some(receipt_id.as_str())
                        && entry.required_by == "admitted retry feedback evidence"
                )
        );
    }
}
