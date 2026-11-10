//! One AF-owned Pi 1.4.0 SDK process per Attempt. Credentials stay in the embedded host.
//!
//! The CLI supplies machine bindings and native Task integration. This adapter implements
//! `af_runner::task::WorkerModelAdapter`; `af-runtime` owns
//! execution and admission. `af-contracts` supplies Pi authority and usage evidence, `af-runner`
//! supplies Worker contracts and transport, `af-process` supervises the confined host, and
//! `af-store` retains safe evidence. Application bindings use `af_runner::models` before
//! dispatch; Pi-specific catalog and host identity checks remain here. The embedded `host.mjs` owns Pi SDK
//! interaction and credential access. There is no dependency on another Provider adapter,
//! the CLI, or the Scenario and Project libraries.
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use af_contracts::task::plan::{PiAuthorityV3, PiIdentityModeV1};
use af_contracts::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use af_process::{ExitPolicy, SupervisedError};
use af_runner::task::{
    ModelWorkerReturn, RemainingTokenAllowance, WorkerAccess, WorkerModelAdapter,
};
use af_store::Cas;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const HOST: &str = include_str!("host.mjs");
pub const PROTOCOL: &str = "af.pi-host/7";
pub const SDK_VERSION: &str = "1.4.0";
const MAX_RECORD: usize = 4 * 1024 * 1024;
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiConfig {
    pub backend: String,
    pub sdk_entrypoint: PathBuf,
}

impl PiConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !exact_id(&self.backend) {
            return Err("Pi requires an exact SDK/profile provider ID".into());
        }
        if !self.sdk_entrypoint.is_absolute() {
            return Err("Pi SDK entrypoint must be an absolute installed dist/index.js".into());
        }
        if self.sdk_entrypoint.to_str().is_none() {
            return Err("Pi SDK entrypoint must be valid UTF-8".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub efforts: Vec<String>,
    pub identity: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    pub catalog_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub principal: Option<String>,
    pub auth_method: String,
    pub backend: String,
    pub version: String,
    pub models: Vec<Model>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "af_contracts::task::present_option"
    )]
    pub next_offset: Option<usize>,
}
impl Ready {
    fn validate(&self) -> Result<(), String> {
        if !af_contracts::is_digest(&self.catalog_id)
            || !exact_id(&self.backend)
            || (self.backend == "openai-codex"
                && !self
                    .principal
                    .as_deref()
                    .is_some_and(af_contracts::is_digest))
            || (self.backend != "openai-codex" && self.principal.is_some())
            || !["oauth", "api_key", "context"].contains(&self.auth_method.as_str())
            || (self.backend == "openai-codex" && self.auth_method != "oauth")
            || self
                .next_offset
                .is_some_and(|offset| offset == 0 || offset >= 4096)
            || self.version != SDK_VERSION
            || self.models.is_empty()
            || self.models.len() > 64
            || self.models.iter().any(|model| {
                !exact_id(&model.id)
                    || !af_contracts::is_digest(&model.identity)
                    || model.efforts.is_empty()
                    || model.efforts.len() > EFFORTS.len()
                    || model
                        .efforts
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != model.efforts.len()
                    || model
                        .efforts
                        .iter()
                        .any(|level| !EFFORTS.contains(&level.as_str()))
            })
        {
            return Err("Pi host identity/catalog is incompatible".into());
        }
        let unique: std::collections::BTreeSet<_> =
            self.models.iter().map(|model| &model.id).collect();
        if unique.len() != self.models.len() {
            return Err("Pi catalog duplicates a model".into());
        }
        Ok(())
    }
    pub fn model(&self, id: &str, effort: &str) -> Result<&Model, String> {
        self.models.iter().find(|model| model.id == id && model.efforts.iter().any(|level| level == effort))
            .ok_or_else(|| "Pi requires an exact catalog model and supported thinking level; run `af provider models ID` (token-free)".into())
    }
}
pub use af_contracts::task::plan::{PI_EFFORTS as EFFORTS, pi_identifier as exact_id};
pub fn static_rules(model: &str, effort: &str) -> Result<(), String> {
    if !exact_id(model) || !EFFORTS.contains(&effort) {
        return Err("Pi requires an exact catalog model ID and thinking level off, minimal, low, medium, high, xhigh or max".into());
    }
    Ok(())
}
fn digest(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}
// Versioned AF token-budget conventions, not provider invoices. These identifiers spell
// out both completeness and charging semantics; changing either must change the policy.
#[derive(Clone, Copy)]
enum Accounting {
    OpenAi,
    Total,
}
impl Accounting {
    fn identity(self) -> &'static str {
        match self {
            Self::OpenAi => {
                "af.pi-budget/openai-input-minus-cache-read-plus-output@1;required=input,output,cache_read;reasoning-and-cache-write-included"
            }
            Self::Total => {
                "af.pi-budget/native-total-input-plus-output@1;required=input,output;no-cache-discount;reasoning-and-cache-write-included"
            }
        }
    }
}
fn accounting(backend: &str) -> Option<Accounting> {
    exact_id(backend).then_some(if backend == "openai-codex" {
        Accounting::OpenAi
    } else {
        Accounting::Total
    })
}
pub fn policy_id() -> String {
    let mut policy = b"af.pi-adapter-policy@4\0".to_vec();
    for (scope, convention) in [
        ("openai-codex", Accounting::OpenAi),
        ("all-other-sdk-chat", Accounting::Total),
    ] {
        policy.extend_from_slice(scope.as_bytes());
        policy.push(0);
        policy.extend_from_slice(convention.identity().as_bytes());
        policy.push(0);
    }
    policy.extend_from_slice(HOST.as_bytes());
    digest(&policy)
}

/// No ambient module resolution: callers supply a resolved Node executable and installed SDK.
/// The script is materialized outside the candidate and never loaded from a repository.
fn command(node: &Path, staging: &Path) -> Result<std::process::Command, String> {
    let script = staging.join("host.mjs");
    std::fs::write(&script, HOST).map_err(|_| "Cannot materialize embedded Pi host")?;
    let home = staging.join("home");
    std::fs::create_dir(&home).map_err(|_| "Cannot create private Pi HOME")?;
    let mut command = std::process::Command::new(node);
    command
        .arg("--no-addons")
        .arg(&script)
        .current_dir(staging)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("XDG_CACHE_HOME", &home)
        .env("PI_OFFLINE", "1")
        .env("PI_TELEMETRY", "off");
    Ok(command)
}
#[allow(clippy::too_many_arguments)]
fn init(
    config: &PiConfig,
    auth: &Path,
    auth_context_id: &str,
    cwd: &Path,
    home: &Path,
    principal: Option<&str>,
    writable: bool,
    probe: bool,
    readable: &[PathBuf],
) -> Value {
    json!({"schema":PROTOCOL,"type":"init","sdk":config.sdk_entrypoint,"backend":config.backend,
        "auth_dir":auth,"auth_context_id":auth_context_id,"principal":principal,"cwd":cwd,"home":home,"readable":readable,
        "writable":if writable { vec![cwd] } else { vec![] },"probe":probe})
}
fn write_record(writer: &mut dyn Write, record: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, record)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Token-free, bounded identity/catalog probe. Only the safe host response is read by Rust.
pub fn probe(
    node: &Path,
    config: &PiConfig,
    auth: &Path,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
) -> Result<Ready, String> {
    let start = Instant::now();
    let mut ready = probe_page(node, config, auth, timeout, cancellation, 0)?;
    let mut seen = ready
        .models
        .iter()
        .map(|m| m.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    while let Some(offset) = ready.next_offset {
        if offset != ready.models.len() || offset >= 4096 || start.elapsed() >= timeout {
            return Err("Pi catalog pagination exceeds its bound or deadline".into());
        }
        let page = probe_page(
            node,
            config,
            auth,
            timeout.saturating_sub(start.elapsed()),
            cancellation,
            offset,
        )?;
        if page.catalog_id != ready.catalog_id
            || page.backend != ready.backend
            || page.auth_method != ready.auth_method
            || page.principal != ready.principal
            || page.models.iter().any(|m| !seen.insert(m.id.clone()))
        {
            return Err("Pi identity/catalog changed during pagination".into());
        }
        ready.next_offset = page.next_offset;
        ready.models.extend(page.models);
    }
    Ok(ready)
}

fn probe_page(
    node: &Path,
    config: &PiConfig,
    auth: &Path,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
    offset: usize,
) -> Result<Ready, String> {
    let started = Instant::now();
    config.validate()?;
    let auth = std::fs::canonicalize(auth).map_err(|_| "Cannot resolve Pi auth context")?;
    auth.to_str().ok_or("Pi auth context must be UTF-8")?;
    let auth_context = digest(auth.as_os_str().as_encoded_bytes());
    let staging = tempfile::tempdir().map_err(|_| "Cannot create Pi probe runtime")?;
    let mut command = command(node, staging.path())?;
    let mut request = init(
        config,
        &auth,
        &auth_context,
        staging.path(),
        &staging.path().join("home"),
        None,
        false,
        true,
        &[],
    );
    request["catalog_offset"] = json!(offset);
    let input = format!("{request}\n").into_bytes();
    let capture = af_process::run_supervised_captured_bounded_observed(
        &mut command,
        Some(input),
        timeout.saturating_sub(started.elapsed()),
        ExitPolicy::KillProcessGroup,
        cancellation,
        None,
        64 * 1024,
    );
    let mut state = State::default();
    for record in capture
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        state.record(record)?;
    }
    if !capture.status.is_ok_and(|status| status.success())
        || capture.truncation.is_truncated()
        || capture.stderr_held
    {
        return Err(state.error.unwrap_or_else(|| "Pi host could not establish compatibility/authentication; requires Node >=22.19, installed Pi SDK 1.4.0 and explicitly configured credentials".into()));
    }
    if state.error.is_some() || !capture.stdout.ends_with(b"\n") {
        return Err(state.error.unwrap_or_else(|| "Pi host probe failed".into()));
    }
    let ready = state
        .ready
        .ok_or("Pi host did not establish identity/catalog")?;
    if ready.backend != config.backend {
        return Err("Pi host backend differs from registered authority".into());
    }
    Ok(ready)
}

pub fn authority(
    node: &Path,
    config: &PiConfig,
    auth: &Path,
    model: &Model,
    auth_method: &str,
) -> Result<PiAuthorityV3, String> {
    config.validate()?;
    let auth = std::fs::canonicalize(auth).map_err(|_| "Cannot resolve Pi auth context")?;
    auth.to_str().ok_or("Pi auth context must be UTF-8")?;
    Ok(PiAuthorityV3 {
        identity_mode: if config.backend != "openai-codex" {
            PiIdentityModeV1::Unavailable
        } else {
            PiIdentityModeV1::Account
        },
        backend: config.backend.clone(),
        auth_method: auth_method.into(),
        sdk_version: SDK_VERSION.into(),
        sdk_entrypoint: config.sdk_entrypoint.to_string_lossy().into(),
        node_program: node.to_string_lossy().into(),
        auth_context_id: digest(auth.as_os_str().as_encoded_bytes()),
        model_config_id: model.identity.clone(),
        adapter_policy_id: policy_id(),
    })
}

#[derive(Default)]
struct State {
    ready: Option<Ready>,
    prepared: Option<(String, String, String)>,
    accepted: bool,
    prompt_sent: bool,
    prompt_nonce: Option<String>,
    settled: bool,
    message: Option<Vec<u8>>,
    usage: BTreeMap<u64, Usage>,
    tool_counts: BTreeMap<String, u64>,
    error: Option<String>,
    inference_failure: Option<InferenceFailure>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InferenceFailure {
    diagnostic: String,
    failure_kind: InferenceFailureKind,
    accounting_complete: bool,
}
// The host classifies original provider text before withholding private material.
// Deserialize a closed category; never infer the category again from redacted text.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum InferenceFailureKind {
    AuthMissing,
    AuthRevoked,
    AuthExpired,
    AuthRefreshContended,
    AuthRefreshFailed,
    AuthRejected,
    Quota,
    Capacity,
    ModelUnavailable,
    ModelRefused,
    Network,
    Environment,
}
impl InferenceFailure {
    fn kind(&self) -> review_runner::native_failure::NativeFailureKind {
        use review_runner::native_failure::NativeFailureKind;
        match self.failure_kind {
            InferenceFailureKind::AuthMissing => NativeFailureKind::AuthMissing,
            InferenceFailureKind::AuthRevoked => NativeFailureKind::AuthRevoked,
            InferenceFailureKind::AuthExpired => NativeFailureKind::AuthExpired,
            InferenceFailureKind::AuthRefreshContended => NativeFailureKind::AuthRefreshContended,
            InferenceFailureKind::AuthRefreshFailed => NativeFailureKind::AuthRefreshFailed,
            InferenceFailureKind::AuthRejected => NativeFailureKind::AuthRejected,
            InferenceFailureKind::Quota => NativeFailureKind::Quota,
            InferenceFailureKind::Capacity => NativeFailureKind::Capacity,
            InferenceFailureKind::ModelUnavailable => NativeFailureKind::ModelUnavailable,
            InferenceFailureKind::ModelRefused => NativeFailureKind::ModelRefused,
            InferenceFailureKind::Network => NativeFailureKind::Network,
            InferenceFailureKind::Environment => NativeFailureKind::Environment,
        }
    }
}
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Usage {
    inference: u64,
    complete: bool,
    observations: u64,
    #[serde(deserialize_with = "counter")]
    input: Option<u64>,
    #[serde(deserialize_with = "counter")]
    output: Option<u64>,
    #[serde(deserialize_with = "counter")]
    cache_read: Option<u64>,
    #[serde(deserialize_with = "counter")]
    cache_write: Option<u64>,
    #[serde(deserialize_with = "counter")]
    reasoning: Option<u64>,
}
fn counter<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|v| v > 9_007_199_254_740_991) {
        return Err(serde::de::Error::custom(
            "Pi counter exceeds safe JSON integer range",
        ));
    }
    Ok(value)
}

/// Bounded diagnostic snapshot, separate from the Attempt's authoritative accounting.
/// Its closed reader accepts only this release's evidence version, never raw SDK bodies.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiInferenceEvidenceV2 {
    schema: String,
    backend: String,
    remaining_tokens: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    request_nonce: Option<String>,
    adapter_policy_id: String,
    provenance: String,
    inferences: Vec<Usage>,
    started_inferences: usize,
    completed_inferences: usize,
    tool_counts: BTreeMap<String, u64>,
    settled: bool,
    successful: bool,
    aggregate: TaskUsageObservationV1,
}
impl PiInferenceEvidenceV2 {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 256 * 1024 {
            return Err("Pi safe evidence exceeds its byte bound".into());
        }
        let evidence: Self =
            serde_json::from_slice(bytes).map_err(|_| "Invalid Pi safe evidence")?;
        if evidence.schema != "af/pi-inference-evidence@2"
            || accounting(&evidence.backend).is_none()
            || RemainingTokenAllowance::new(evidence.remaining_tokens).is_err()
            || evidence.remaining_tokens > 9_007_199_254_740_991
            || !af_contracts::is_digest(&evidence.adapter_policy_id)
            || evidence
                .request_nonce
                .as_ref()
                .is_some_and(|nonce| !af_contracts::is_digest(nonce))
            || (!evidence.inferences.is_empty() && evidence.request_nonce.is_none())
            || evidence.provenance != "native-per-inference-cumulative;aggregate-af-charge"
            || evidence.inferences.len() > 256
            || evidence.started_inferences != evidence.inferences.len()
            || evidence.completed_inferences
                != evidence.inferences.iter().filter(|u| u.complete).count()
            || evidence
                .inferences
                .iter()
                .enumerate()
                .any(|(i, u)| u.inference != i as u64 + 1 || u.observations > 128)
            || evidence.tool_counts.iter().any(|(name, count)| {
                !["read", "ls", "find", "grep", "write", "edit"].contains(&name.as_str())
                    || *count == 0
                    || *count > 4096
            })
            || evidence.tool_counts.values().sum::<u64>() > 4096
            || (evidence.successful && !evidence.settled)
        {
            return Err("Unsupported or invalid Pi safe evidence".into());
        }
        evidence.aggregate.validate()?;
        Ok(evidence)
    }
}

impl State {
    fn record(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut value: Value =
            serde_json::from_slice(bytes).map_err(|_| "Malformed Pi host JSONL")?;
        if value["schema"] != PROTOCOL {
            return Err("Unsupported Pi host protocol".into());
        }
        let object = value.as_object_mut().ok_or("Invalid Pi host record")?;
        object.remove("schema");
        let kind = object
            .remove("type")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or("Pi host record has no type")?;
        if self.settled || self.error.is_some() {
            return Err("Pi host data after terminal outcome".into());
        }
        match kind.as_str() {
            "ready" if self.ready.is_none() && !self.accepted => {
                let ready: Ready =
                    serde_json::from_value(value).map_err(|_| "Invalid Pi host identity")?;
                ready.validate()?;
                self.ready = Some(ready);
            }
            "prepared" if self.ready.is_some() && self.prepared.is_none() => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Prepared {
                    model: String,
                    effort: String,
                    model_identity: String,
                }
                let p: Prepared =
                    serde_json::from_value(value).map_err(|_| "Invalid Pi prepared response")?;
                self.prepared = Some((p.model, p.effort, p.model_identity));
            }
            "accepted" if self.prepared.is_some() && self.prompt_sent && !self.accepted => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Accepted {
                    prompt_nonce: String,
                }
                let accepted: Accepted = serde_json::from_value(value)
                    .map_err(|_| "Invalid Pi prompt acknowledgement")?;
                if self.prompt_nonce.as_ref() != Some(&accepted.prompt_nonce) {
                    return Err("Pi host acknowledged a prompt it was not delivered".into());
                }
                self.accepted = true;
            }
            "usage" if self.accepted => {
                let usage: Usage = serde_json::from_value(value).map_err(|_| "Invalid Pi usage")?;
                if !self.usage.contains_key(&usage.inference)
                    && usage.inference != self.usage.len() as u64 + 1
                {
                    return Err("Pi skipped an inference identity".into());
                }
                if usage.inference == 0 || usage.inference > 256 || usage.observations > 128 {
                    return Err("Invalid Pi inference identity".into());
                }
                if let Some(previous) = self.usage.get(&usage.inference)
                    && (previous.complete
                        || previous.observations > usage.observations
                        || [
                            (previous.input, usage.input),
                            (previous.output, usage.output),
                            (previous.cache_read, usage.cache_read),
                            (previous.cache_write, usage.cache_write),
                            (previous.reasoning, usage.reasoning),
                        ]
                        .iter()
                        .any(|(old, new)| old.is_some_and(|old| new.is_none_or(|new| new < old))))
                {
                    // A duplicate finalized observation is only an echo if its counters match.
                    if previous.complete == usage.complete
                        && previous.observations == usage.observations
                        && previous.input == usage.input
                        && previous.output == usage.output
                        && previous.cache_read == usage.cache_read
                        && previous.cache_write == usage.cache_write
                        && previous.reasoning == usage.reasoning
                    {
                        return Ok(());
                    }
                    return Err("Pi usage changed a finalized or cumulative counter".into());
                }
                if usage
                    .input
                    .zip(usage.cache_read)
                    .is_some_and(|(input, read)| read > input)
                    || usage
                        .input
                        .zip(usage.cache_write)
                        .is_some_and(|(input, write)| write > input)
                    || usage
                        .input
                        .zip(usage.cache_read.zip(usage.cache_write))
                        .is_some_and(|(input, (read, write))| {
                            u128::from(read) + u128::from(write) > u128::from(input)
                        })
                    || usage
                        .output
                        .zip(usage.reasoning)
                        .is_some_and(|(output, reasoning)| reasoning > output)
                {
                    return Err("Pi usage components exceed their totals".into());
                }
                if !self.usage.contains_key(&usage.inference) {
                    self.message = None;
                }
                self.usage.insert(usage.inference, usage);
            }
            "tool_count" if self.accepted => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct ToolCount {
                    name: String,
                    count: u64,
                }
                let count: ToolCount =
                    serde_json::from_value(value).map_err(|_| "Invalid Pi tool count")?;
                if !["read", "ls", "find", "grep", "write", "edit"].contains(&count.name.as_str())
                    || count.count != self.tool_counts.get(&count.name).copied().unwrap_or(0) + 1
                    || count.count > 4096
                {
                    return Err("Invalid Pi tool count".into());
                }
                if self.tool_counts.values().sum::<u64>() >= 4096 {
                    return Err("Pi tool evidence capacity exhausted".into());
                }
                self.tool_counts.insert(count.name, count.count);
            }
            "message" if self.accepted => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Message {
                    text: String,
                }
                let m: Message = serde_json::from_value(value).map_err(|_| "Invalid Pi message")?;
                self.message = Some(m.text.into_bytes());
            }
            "settled" if self.accepted && object.is_empty() => {
                // A terminal message cannot erase an unobserved or unfinished inference.
                // Counter values may remain unknown; completion is a lifecycle guarantee.
                if self.usage.is_empty() || self.usage.values().any(|usage| !usage.complete) {
                    return Err("Pi settled without complete inference observations".into());
                }
                self.settled = true;
            }
            "error" => {
                if object.get("code").and_then(Value::as_str) == Some("inference_failed") {
                    object.remove("code");
                    let mut failure: InferenceFailure = serde_json::from_value(value)
                        .map_err(|_| "Invalid Pi inference failure")?;
                    if failure.diagnostic.is_empty()
                        || failure.diagnostic.len() > 240
                        || !failure
                            .diagnostic
                            .bytes()
                            .all(|b| (b' '..=b'~').contains(&b))
                        || (failure.accounting_complete
                            && (!self.accepted || !self.complete_usage()))
                    {
                        return Err("Invalid Pi inference failure or accounting claim".into());
                    }
                    // Defense in depth: only the host knows literal credential values,
                    // and Rust additionally applies the common native-output redactor.
                    failure.diagnostic =
                        review_runner::cli::stderr_excerpt(failure.diagnostic.as_bytes())
                            .unwrap_or_else(|| "Pi SDK supplied no terminal diagnostic".into());
                    let kind = failure.kind();
                    let context = if kind.is_auth() {
                        "Pi Provider authentication failed"
                    } else {
                        "Pi SDK session failed"
                    };
                    self.error = Some(format!(
                        "{context}: {}; {}",
                        kind.diagnostic(),
                        failure.diagnostic
                    ));
                    self.inference_failure = Some(failure);
                    return Ok(());
                }
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Error {
                    code: String,
                }
                let e: Error =
                    serde_json::from_value(value).map_err(|_| "Invalid Pi error record")?;
                let message = match e.code.as_str() {
                    "host_refused" => {
                        "Pi SDK host could not load or serve its pinned protocol; check the installed SDK entrypoint"
                    }
                    "unsupported_auth" => {
                        "Pi credential mode or fields are invalid; use stored OAuth, a literal API key or an explicit credential context; OpenAI Codex requires a routing-checked accountId"
                    }
                    "identity_changed" => {
                        "Pi account differs from the captured principal; replan with the intended login"
                    }
                    "sdk_version" => "Pi requires the official SDK exactly 1.4.0",
                    "node_version" => "Pi requires Node >=22.19",
                    "unsupported_backend" => {
                        "Pi credential access differs from the explicitly selected SDK provider"
                    }
                    "unsafe_model_configuration" => {
                        "Pi sampling configuration cannot override model, context/session, tools, transport or token allowance; remove controlled fields from models.json"
                    }
                    "unsupported_transport" => {
                        "Pi model uses a transport without safe native usage accounting"
                    }
                    "profile_authority" => {
                        "Pi profile files must be private owned regular files; check af-context.json and its explicit environment"
                    }
                    "helper_authority" => {
                        "Pi command-valued keys/headers require an explicitly pinned installed provider extension; commands are never evaluated from data"
                    }
                    "extension_authority" => {
                        "Pi extension requires an installed external entrypoint and exact sha256 in af-context.json"
                    }
                    "configuration_changed" => {
                        "Pi explicit profile configuration changed before inference; replan"
                    }
                    "provider_configuration" => {
                        "Pi provider is absent or its profile catalog/configuration is invalid; configure models.json or a pinned installed extension"
                    }
                    "no_chat_models" => {
                        "Pi selected provider has no chat models; image and classifier models cannot run coding Workers"
                    }
                    "missing_credentials" => {
                        "Pi selected provider has no usable credentials; configure its stored key/OAuth or explicit af-context.json credential environment"
                    }
                    "model_changed" => "Pi exact model or endpoint configuration changed",
                    "settings_changed" => {
                        "Pi effective model, thinking or tool policy differs from captured authority"
                    }
                    _ => return Err("Unsupported Pi error code".into()),
                };
                self.error = Some(message.into());
            }
            _ => return Err("Pi host lifecycle violation".into()),
        }
        Ok(())
    }
    fn complete_usage(&self) -> bool {
        let convention = self
            .ready
            .as_ref()
            .and_then(|ready| accounting(&ready.backend));
        !self.usage.is_empty()
            && self.usage.values().all(|u| {
                u.complete
                    && u.observations > 0
                    && u.input.is_some()
                    && u.output.is_some()
                    && match convention {
                        Some(Accounting::Total) => true,
                        Some(Accounting::OpenAi) => u.cache_read.is_some(),
                        None => false,
                    }
            })
    }
    fn observation(&self) -> TaskUsageObservationV1 {
        if !self.accepted && !self.prompt_sent {
            return TaskUsageObservationV1 {
                reported_usage: Some(TaskTokenUsageV3::charge_only(0)),
                charge_complete: true,
            };
        }
        if self.usage.is_empty() {
            return TaskUsageObservationV1 {
                reported_usage: None,
                charge_complete: false,
            };
        }
        if self.usage.values().all(|u| {
            [u.input, u.output, u.cache_read, u.cache_write, u.reasoning]
                .into_iter()
                .all(|counter| counter.is_none())
        }) {
            return TaskUsageObservationV1 {
                reported_usage: None,
                charge_complete: false,
            };
        }
        let sum =
            |get: fn(&Usage) -> Option<u64>| -> Option<af_contracts::task::usage::DecimalU128> {
                self.usage
                    .values()
                    .try_fold(0u128, |sum, u| Some(sum + u128::from(get(u)?)))
                    .map(Into::into)
            };
        let convention = self
            .ready
            .as_ref()
            .and_then(|ready| accounting(&ready.backend));
        // Charge each inference once under its captured backend convention. Kimi charges
        // total prompt + completion even when cache counters are present. OpenAI keeps its
        // cache discount. Reasoning/cache writes are already included in those totals.
        // Unknown counters remain unknown; only observed contributions form the floor.
        let charge = self.usage.values().fold(0u128, |sum, u| {
            sum + u128::from(match convention {
                Some(Accounting::Total) => u.input.unwrap_or(0),
                Some(Accounting::OpenAi) => u
                    .input
                    .zip(u.cache_read)
                    .map_or(0, |(input, read)| input.saturating_sub(read)),
                None => 0,
            }) + u128::from(u.output.unwrap_or(0))
        });
        let complete = ((self.settled && self.error.is_none())
            || self
                .inference_failure
                .as_ref()
                .is_some_and(|f| f.accounting_complete))
            && self.complete_usage();
        TaskUsageObservationV1 {
            charge_complete: complete,
            reported_usage: Some(TaskTokenUsageV3 {
                input_tokens: sum(|u| u.input),
                output_tokens: sum(|u| u.output),
                cache_read_tokens: sum(|u| u.cache_read),
                cache_write_tokens: sum(|u| u.cache_write),
                reasoning_tokens: sum(|u| u.reasoning),
                chargeable_tokens: charge.into(),
            }),
        }
    }
}

pub struct PiTaskAdapter {
    pub node: PathBuf,
    pub config: PiConfig,
    pub auth_dir: PathBuf,
    pub principal: Option<String>,
    pub model: String,
    pub effort: String,
    pub captured: PiAuthorityV3,
}
impl WorkerModelAdapter for PiTaskAdapter {
    fn provider_kind(&self) -> &'static str {
        "pi"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some((self.model.clone(), self.effort.clone()))
    }
    fn invoke(
        &self,
        _cas: &Cas,
        _workdir: &Path,
        _input: Vec<u8>,
        _timeout: Duration,
        _access: WorkerAccess,
        _cancellation: Option<&AtomicBool>,
        _environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        ModelWorkerReturn {
            message: Err("Pi requires a trusted remaining token allowance".into()),
            usage: Some(TaskTokenUsageV3::charge_only(0)),
            usage_observation: None,
            raw_artifact_ids: vec![],
            native_failure: None,
            notes: Default::default(),
        }
    }
    fn invoke_with_allowance(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
        allowance: RemainingTokenAllowance,
        contract: af_runner::task::reply_schema::ReplyContract,
    ) -> Result<ModelWorkerReturn, af_runner::task::Unstarted> {
        if allowance.tokens() > 9_007_199_254_740_991 {
            return Ok(self.invoke(
                cas,
                workdir,
                input,
                timeout,
                access,
                cancellation,
                environment,
            ));
        }
        Ok(self.invoke_bounded(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
            allowance,
            contract,
        ))
    }
}
impl PiTaskAdapter {
    #[allow(clippy::too_many_arguments)]
    fn invoke_bounded(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&AtomicBool>,
        environment: &[(String, String)],
        allowance: RemainingTokenAllowance,
        contract: af_runner::task::reply_schema::ReplyContract,
    ) -> ModelWorkerReturn {
        let started = Instant::now();
        let state = Mutex::new(State::default());
        let stopped = AtomicBool::new(false);
        let run = || -> Result<(), String> {
            let reply_schema = contract.transport(&input)?;
            if timeout.is_zero() || cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return Err("Pi Attempt ended before starting".into());
            }
            static_rules(&self.model, &self.effort)?;
            self.config.validate()?;
            if self.captured.adapter_policy_id != policy_id() {
                return Err("Pi adapter policy changed".into());
            }
            let staging = tempfile::tempdir().map_err(|_| "Cannot create Pi Attempt runtime")?;
            let mut command = command(&self.node, staging.path())?;
            let mut readable = vec![workdir.to_path_buf()];
            // Only explicit kernel grants extend read authority; never the sandbox's parent.
            for (name, value) in environment {
                if name == "AF_PI_READ_ROOT" {
                    readable.push(PathBuf::from(value));
                }
            }
            let mut request = init(
                &self.config,
                &self.auth_dir,
                &self.captured.auth_context_id,
                workdir,
                &staging.path().join("home"),
                self.principal.as_deref(),
                matches!(
                    access,
                    WorkerAccess::WriteSource | WorkerAccess::WriteSourceWithShell
                ),
                false,
                &readable,
            );
            request["model"] = json!(self.model);
            let (send, receive) = mpsc::channel::<Value>();
            let writer = |writer: &mut dyn Write| -> std::io::Result<()> {
                write_record(writer, &request)?;
                for mut record in receive {
                    if started.elapsed() >= timeout
                        || stopped.load(Ordering::Acquire)
                        || cancellation.is_some_and(|flag| flag.load(Ordering::Acquire))
                    {
                        stopped.store(true, Ordering::Release);
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "Pi Attempt ended before input",
                        ));
                    }
                    if record["type"] == "prompt" {
                        // Generate the challenge here, never in init/prepare or a host-visible
                        // file. Echoing it proves the host read this complete prompt record.
                        let mut random = [0; 32];
                        std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
                        let nonce = digest(&random);
                        record["prompt_nonce"] = json!(nonce);
                        let mut state = state.lock().unwrap();
                        state.prompt_nonce = Some(nonce);
                        // Publish before writing: an immediate legitimate acknowledgement
                        // may arrive before write_record returns. A failed/partial write
                        // conservatively leaves usage unknown, never a known zero.
                        state.prompt_sent = true;
                    }
                    write_record(writer, &record)?;
                    if record["type"] == "prompt" {
                        break;
                    }
                }
                Ok(())
            };
            let shared = &state;
            let protocol_stop = &stopped;
            let input_ref = &input;
            let reader = move |reader: &mut dyn std::io::Read| {
                let state = shared;
                let stopped = protocol_stop;
                let input = input_ref;
                let mut reader = BufReader::new(reader);
                let mut total = 0;
                let mut line = Vec::new();
                loop {
                    line.clear();
                    // take bounds the allocation even when the peer never supplies LF.
                    let read = reader
                        .by_ref()
                        .take((MAX_RECORD + 1) as u64)
                        .read_until(b'\n', &mut line);
                    match read {
                        Ok(0) => break,
                        Ok(n) => {
                            total += n;
                            if n > MAX_RECORD || total > MAX_OUTPUT || !line.ends_with(b"\n") {
                                state.lock().unwrap().error =
                                    Some("Pi host output limit or partial EOF".into());
                                stopped.store(true, Ordering::Release);
                                break;
                            }
                        }
                        Err(_) => {
                            state.lock().unwrap().error = Some("Pi host output read failed".into());
                            stopped.store(true, Ordering::Release);
                            break;
                        }
                    }
                    let mut state = state.lock().unwrap();
                    if let Err(error) = state.record(&line) {
                        state.error = Some(error);
                        stopped.store(true, Ordering::Release);
                        break;
                    }
                    if state.error.is_some() {
                        stopped.store(true, Ordering::Release);
                        break;
                    }
                    if let Some(ready) = &state.ready
                        && state.prepared.is_none()
                    {
                        let valid = ready.backend == self.config.backend
                            && ready.principal == self.principal
                            && ready.model(&self.model, &self.effort).is_ok_and(|model| {
                                authority(
                                    &self.node,
                                    &self.config,
                                    &self.auth_dir,
                                    model,
                                    &ready.auth_method,
                                )
                                .is_ok_and(|authority| authority == self.captured)
                            });
                        if !valid {
                            state.error =
                                Some("Pi identity/model configuration changed before input".into());
                            stopped.store(true, Ordering::Release);
                            break;
                        }
                        let _ = send.send(json!({"schema":PROTOCOL,"type":"prepare","remaining_tokens":allowance.tokens(),"reply_schema":reply_schema,"model":self.model,"effort":self.effort,"model_identity":self.captured.model_config_id}));
                    } else if let Some(prepared) = &state.prepared
                        && !state.accepted
                        && state.message.is_none()
                    {
                        if prepared
                            != &(
                                self.model.clone(),
                                self.effort.clone(),
                                self.captured.model_config_id.clone(),
                            )
                        {
                            state.error = Some("Pi effective settings changed before input".into());
                            stopped.store(true, Ordering::Release);
                            break;
                        }
                        let Ok(text) = std::str::from_utf8(input) else {
                            state.error = Some("Pi input is not UTF-8".into());
                            stopped.store(true, Ordering::Release);
                            break;
                        };
                        let _ = send.send(json!({"schema":PROTOCOL,"type":"prompt","text":text}));
                    }
                    if state.error.is_some() {
                        stopped.store(true, Ordering::Release);
                        break;
                    }
                }
            };
            let mut signals = vec![&stopped];
            signals.extend(cancellation);
            let output = af_process::run_supervised_duplex_signals(
                &mut command,
                timeout.saturating_sub(started.elapsed()),
                ExitPolicy::KillProcessGroup,
                &signals,
                writer,
                reader,
            );
            match output {
                Ok(output)
                    if output.status.success()
                        && output.input.is_ok()
                        && !output.stderr_held
                        && !output.truncation.is_truncated() =>
                {
                    Ok(())
                }
                Err(SupervisedError::Spawn(_)) => Err("Pi Node process could not start".into()),
                _ => Err(
                    "Pi process failed, was cancelled, exceeded its deadline or output limit"
                        .into(),
                ),
            }
        };
        let ending = run();
        let mut state = state.into_inner().unwrap();
        if let Err(error) = ending {
            state.error.get_or_insert(error);
        }
        let observation = state.observation();
        // Safe, bounded snapshots, not a second source of spend. Repeated observations
        // replace each inference above; only observation() supplies aggregate AF charge.
        let evidence = PiInferenceEvidenceV2 {
            schema: "af/pi-inference-evidence@2".into(),
            backend: self.config.backend.clone(),
            remaining_tokens: allowance.tokens(),
            request_nonce: state.prompt_nonce.clone(),
            adapter_policy_id: policy_id(),
            provenance: "native-per-inference-cumulative;aggregate-af-charge".into(),
            inferences: state.usage.values().cloned().collect(),
            started_inferences: state.usage.len(),
            completed_inferences: state.usage.values().filter(|u| u.complete).count(),
            tool_counts: state.tool_counts.clone(),
            settled: state.settled,
            successful: state.settled && state.error.is_none(),
            aggregate: observation.clone(),
        };
        let retained = serde_json::to_vec(&evidence)
            .map_err(|_| "Cannot encode Pi safe evidence")
            .and_then(|bytes| {
                cas.put(&bytes)
                    .map_err(|_| "Cannot retain Pi safe evidence")
            });
        let mut raw_artifact_ids = match retained {
            Ok(id) => vec![id],
            Err(error) => {
                state.error.get_or_insert(error.into());
                vec![]
            }
        };
        if let Some(failure) = &state.inference_failure {
            // Retain only the sanitized terminal protocol record, never raw SDK output.
            // The accounting evidence above keeps its existing counters and shape.
            let record = json!({"schema":PROTOCOL,"type":"error","code":"inference_failed",
                "diagnostic":failure.diagnostic,"failure_kind":failure.failure_kind,
                "accounting_complete":failure.accounting_complete});
            match cas.put(&serde_json::to_vec(&record).expect("Pi terminal record is JSON")) {
                Ok(id) => raw_artifact_ids.push(id),
                Err(_) => {
                    state
                        .error
                        .get_or_insert("Cannot retain Pi terminal diagnostic".into());
                }
            }
        }
        let native_failure = state.inference_failure.as_ref().map(InferenceFailure::kind);
        let message = if state.settled && state.error.is_none() {
            state
                .message
                .ok_or_else(|| "Pi settled without a final assistant message".into())
        } else {
            Err(state
                .error
                .unwrap_or_else(|| "Pi ended without terminal settlement".into()))
        };
        ModelWorkerReturn {
            message,
            usage: observation.reported_usage.clone(),
            usage_observation: (!observation.charge_complete).then_some(observation),
            raw_artifact_ids,
            native_failure,
            notes: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests;
