//! Implementation domain handlers over the common Task runtime. Checks and evaluation refer
//! to the sealed Snapshot; negative checks produce a result without an evaluator dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_check::{CheckDefinition, CheckResult, CheckRunner, CheckStatus};
use review_core::PortCardinality;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::*;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::runtime::{
    TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeEvidenceV1, TaskRuntimeSpanKindV1, TaskRuntimeSpanV1,
};
use review_core::task::verification::*;
use review_core::task::*;
use review_graph::task::{CompiledOperator, CompiledTask, OperatorAttemptCost, OperatorSignature};
use review_graph::{NodeOutcome, RunReport};
use review_sandbox::toolchain::{ToolchainLimits, snapshot_toolchain};
use review_sandbox::{Mode, Policy, Sandbox};
use review_source_git::task::{CANDIDATE_TREE_V1, SOURCE_TREE_V1, source_tree};
use review_store::Cas;
use review_store::store::task::TaskProjection;
use review_store::store::task::execution::PreparedTaskAttempt;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::host::TaskDomain;
use super::source::{
    invocation_producer, seal_candidate, source_input, source_snapshot, validate_seal,
};
use super::{TaskOperatorHost, TaskWorkOutput, envelope};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeTaskPolicy {
    pub schema: String,
    pub checks: BTreeMap<String, CheckDefinition>,
    pub check_wall_ms: u64,
    /// Optional per-process cap within the aggregate check Attempt, so one slow check cannot
    /// borrow another's allowance when one Attempt owns several named checks.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub check_process_wall_ms: Option<u64>,
    pub require_container: bool,
    /// Optional named native checks whose Rust tools come from an explicit machine mapping.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub rust_toolchain: Option<RustToolchainRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustToolchainRequest {
    pub version: String,
    pub host: String,
    pub components: BTreeSet<String>,
    pub checks: BTreeSet<String>,
}

impl RustToolchainRequest {
    fn validate(&self, checks: &BTreeMap<String, CheckDefinition>) -> Result<(), String> {
        let version = self.version.split('.').collect::<Vec<_>>();
        if version.len() != 3
            || version
                .iter()
                .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
            || self.host.is_empty()
            || self.host.len() > 128
            || !self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || self
                .components
                .iter()
                .any(|name| !matches!(name.as_str(), "rustfmt" | "clippy"))
            || self.checks.is_empty()
            || self.checks.iter().any(|name| !checks.contains_key(name))
        {
            return Err("Rust toolchain request needs an exact version, host, supported components and named checks".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineToolchainPolicy {
    version: u32,
    rust: MachineRustToolchain,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineRustToolchain {
    version: String,
    host: String,
    components: BTreeSet<String>,
    source: PathBuf,
    expected_digest: String,
    max_bytes: u64,
    max_entries: u64,
    max_copy_bytes: u64,
}

fn native_host_matches(host: &str) -> bool {
    let architecture = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => {
            host == format!("{architecture}-unknown-linux-gnu")
                || host == format!("{architecture}-unknown-linux-musl")
        }
        "macos" => host == format!("{architecture}-apple-darwin"),
        _ => false,
    }
}

/// Returns `None` only when the operator made no machine mapping available. A selected,
/// malformed mapping is an error before the candidate command starts.
fn machine_toolchain(
    request: &RustToolchainRequest,
    mapping_file: Option<&Path>,
) -> Result<Option<MachineRustToolchain>, String> {
    let Some(path) = mapping_file else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err("Rust toolchain mapping path must be absolute".into());
    }
    let data = review_sandbox::toolchain::read_toolchain_declaration(path, 64 * 1024)?;
    if data.len() > 64 * 1024 {
        return Err("Rust toolchain mapping exceeds 64 KiB".into());
    }
    let text =
        std::str::from_utf8(&data).map_err(|e| format!("invalid Rust toolchain mapping: {e}"))?;
    let policy: MachineToolchainPolicy =
        toml::from_str(text).map_err(|e| format!("invalid Rust toolchain mapping: {e}"))?;
    let entry = policy.rust;
    if policy.version != 1
        || entry.version != request.version
        || (request.host != "native" && entry.host != request.host)
        || (request.host == "native" && !native_host_matches(&entry.host))
        || entry.components != request.components
        || !entry.source.is_absolute()
        || !review_core::is_digest(&entry.expected_digest)
    {
        return Err("Rust toolchain mapping disagrees with captured request".into());
    }
    ToolchainLimits {
        max_bytes: entry.max_bytes,
        max_entries: entry.max_entries,
        max_copy_bytes: entry.max_copy_bytes,
    }
    .validate()?;
    Ok(Some(entry))
}

/// Verified identity and check environment; mapped source paths are never exposed.
#[derive(Debug)]
pub struct PreparedNativeRustToolchain {
    pub environment: Vec<(String, String)>,
    pub content_digest: String,
    pub resolved_host: String,
    pub verified_release: String,
}

/// Exact native preparation entry used by `CodeTaskDomain` after provider admission.
/// Operator mapping and source paths retain strict no-follow admission.
pub fn prepare_native_rust_toolchain(
    candidate: &Path,
    runtime: &Path,
    request: &RustToolchainRequest,
    mapping_file: Option<&Path>,
) -> Result<Option<PreparedNativeRustToolchain>, String> {
    let Some(mapping) = machine_toolchain(request, mapping_file)? else {
        return Ok(None);
    };
    let mut resolved = request.clone();
    resolved.host = mapping.host.clone();
    let request = &resolved;
    candidate_toolchain_matches(candidate, request)?;
    let private = runtime.join("toolchain");
    snapshot_toolchain(
        &mapping.source,
        &private,
        ToolchainLimits {
            max_bytes: mapping.max_bytes,
            max_entries: mapping.max_entries,
            max_copy_bytes: mapping.max_copy_bytes,
        },
        Some(&mapping.expected_digest),
    )?;
    verify_private_toolchain(&private, request)?;
    let cargo_home = runtime.join("cargo");
    let rustup_home = runtime.join("rustup");
    std::fs::create_dir(&cargo_home).map_err(|e| e.to_string())?;
    std::fs::create_dir(&rustup_home).map_err(|e| e.to_string())?;
    Ok(Some(PreparedNativeRustToolchain {
        content_digest: mapping.expected_digest,
        resolved_host: request.host.clone(),
        verified_release: request.version.clone(),
        environment: vec![
            (
                "PATH".into(),
                private_toolchain_path(&private, &mapping.source)?,
            ),
            ("CARGO_HOME".into(), cargo_home.display().to_string()),
            ("RUSTUP_HOME".into(), rustup_home.display().to_string()),
            (
                "RUSTUP_TOOLCHAIN".into(),
                format!("{}-{}", request.version, request.host),
            ),
        ],
    }))
}

fn private_toolchain_path(private: &Path, source: &Path) -> Result<String, String> {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(private.join("bin")).chain(
        std::env::split_paths(&inherited).filter(|path| {
            path.is_absolute()
                && !path.starts_with(source)
                && !path
                    .components()
                    .any(|component| component.as_os_str() == ".rustup")
        }),
    );
    std::env::join_paths(paths)
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|_| "native toolchain PATH is not UTF-8".into())
}

fn candidate_toolchain_matches(root: &Path, request: &RustToolchainRequest) -> Result<(), String> {
    let plain = root.join("rust-toolchain");
    let structured = root.join("rust-toolchain.toml");
    if plain.exists() && structured.exists() {
        return Err("candidate declares two Rust toolchain files".into());
    }
    let selected = if structured.exists() {
        structured
    } else {
        plain
    };
    if !selected.exists() {
        return Ok(());
    }
    let bytes = review_sandbox::toolchain::read_toolchain_declaration(&selected, 16 * 1024)?;
    if bytes.len() > 16 * 1024 {
        return Err("candidate Rust toolchain declaration is too large".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let channel = if selected.extension().is_some() {
        let value: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        if let Some(components) = value.get("toolchain").and_then(|t| t.get("components")) {
            let components = components
                .as_array()
                .ok_or("invalid candidate components")?;
            if components
                .iter()
                .any(|c| c.as_str().is_none_or(|c| !request.components.contains(c)))
            {
                return Err("candidate requires uncaptured Rust components".into());
            }
        }
        if value
            .get("toolchain")
            .and_then(|t| t.get("targets"))
            .is_some_and(|targets| targets.as_array().is_none_or(|t| !t.is_empty()))
        {
            return Err(
                "additional Rust targets are not supported by native toolchain snapshots".into(),
            );
        }
        value
            .get("toolchain")
            .and_then(|table| table.get("channel"))
            .and_then(toml::Value::as_str)
            .ok_or("candidate Rust toolchain has no channel")?
            .to_owned()
    } else {
        text.trim().to_owned()
    };
    if channel != request.version && channel != format!("{}-{}", request.version, request.host) {
        return Err("candidate Rust toolchain differs from captured request".into());
    }
    Ok(())
}

fn verify_private_toolchain(root: &Path, request: &RustToolchainRequest) -> Result<(), String> {
    for name in ["rustc", "cargo"] {
        if !root.join("bin").join(name).is_file() {
            return Err(format!("private Rust toolchain lacks {name}"));
        }
    }
    for component in &request.components {
        let names = if component == "clippy" {
            ["clippy-driver", "cargo-clippy"]
        } else {
            ["rustfmt", "cargo-fmt"]
        };
        for name in names {
            if !root.join("bin").join(name).is_file() {
                return Err(format!(
                    "private Rust toolchain lacks {component} executable {name}"
                ));
            }
        }
    }
    let mut command = std::process::Command::new(root.join("bin/rustc"));
    command.arg("--version").arg("--verbose").env_clear();
    let output = review_process::run_supervised_with_policy(
        &mut command,
        None,
        Duration::from_secs(10),
        review_process::ExitPolicy::KillProcessGroup,
    )
    .map_err(|e| format!("probing private rustc: {e}"))?;
    let stdout = std::str::from_utf8(&output.stdout).map_err(|e| e.to_string())?;
    if !output.status.success()
        || !stdout
            .lines()
            .any(|line| line == format!("release: {}", request.version))
        || !stdout
            .lines()
            .any(|line| line == format!("host: {}", request.host))
    {
        return Err("private rustc release or host differs from captured request".into());
    }
    Ok(())
}

impl CodeTaskPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.code-task-policy/1"
            || self.checks.is_empty()
            || self.checks.len() > 32
            || self.check_wall_ms == 0
            || self.check_wall_ms
                > self.check_process_wall_ms.map_or(3_600_000, |per_check| {
                    per_check
                        .saturating_mul(self.checks.len() as u64)
                        .max(3_600_000)
                })
            || self
                .check_process_wall_ms
                .is_some_and(|ms| ms == 0 || ms > 3_600_000 || ms > self.check_wall_ms)
            || !self.checks.values().any(|check| check.required)
            || self.checks.iter().any(|(name, check)| {
                !is_name(name) || name != &check.name || check.command.resolve().is_err()
            })
            || self.rust_toolchain.as_ref().is_some_and(|request| {
                self.require_container || request.validate(&self.checks).is_err()
            })
        {
            return Err(
                "Code Task requires bounded named checks and at least one required verifier".into(),
            );
        }
        Ok(())
    }
    pub fn isolation(&self) -> Policy {
        if self.require_container {
            Policy::safe()
        } else {
            Policy::trusted_local()
        }
    }
}

fn port(artifact_type: &str, affinity: PortAffinityV1) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: artifact_type.into(),
        cardinality: PortCardinality::One,
        optional: false,
        affinity,
        root_default: None,
        covers: BTreeSet::new(),
    }
}
fn same(input: &str) -> PortAffinityV1 {
    PortAffinityV1::SameAs {
        input: input.into(),
    }
}

pub fn code_signatures(
    policy_id: &str,
    policy: &CodeTaskPolicy,
) -> Result<BTreeMap<String, OperatorSignature>, String> {
    policy.validate()?;
    if !review_core::is_digest(policy_id) {
        return Err("Code policy identity is invalid".into());
    }
    let signature =
        |inputs, outputs, effects, evidence, retains, attempt, outcome_port| OperatorSignature {
            contract: PipelineContractV1 { inputs, outputs },
            effects,
            evidence,
            retains,
            roles: BTreeSet::new(),
            worker_input_type: None,
            worker_output_type: None,
            outcome_port,
            attempt,
        };
    let seal = signature(
        BTreeMap::from([(
            "candidate".into(),
            port(CANDIDATE_TREE_V1, PortAffinityV1::Unbound {}),
        )]),
        BTreeMap::from([(
            "snapshot".into(),
            port(
                SOURCE_TREE_V1,
                PortAffinityV1::DerivedFrom {
                    input: "candidate".into(),
                },
            ),
        )]),
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        None,
        None,
    );
    let check = signature(
        BTreeMap::from([(
            "source".into(),
            port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
        )]),
        BTreeMap::from([("result".into(), port(TASK_CHECK_RECEIPT_V1, same("source")))]),
        BTreeSet::from(["execute-checks".into()]),
        BTreeMap::from([("result".into(), BTreeSet::from([policy_id.into()]))]),
        BTreeMap::new(),
        Some(OperatorAttemptCost {
            tokens: 0,
            wall_ms: policy.check_wall_ms,
        }),
        Some("result".into()),
    );
    let mut evaluator = port(TASK_EVALUATION_V1, same("source"));
    evaluator.optional = true;
    let accept = signature(
        BTreeMap::from([
            (
                "source".into(),
                port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
            ),
            ("checks".into(), port(TASK_CHECK_RECEIPT_V1, same("source"))),
            ("evaluation".into(), evaluator),
        ]),
        BTreeMap::from([
            (
                "result".into(),
                port(VERIFICATION_RESULT_V1, same("source")),
            ),
            ("snapshot".into(), port(SOURCE_TREE_V1, same("source"))),
        ]),
        BTreeSet::new(),
        BTreeMap::from([("result".into(), BTreeSet::from([policy_id.into()]))]),
        BTreeMap::from([
            (
                "result".into(),
                BTreeSet::from(["checks".into(), "evaluation".into()]),
            ),
            (
                "snapshot".into(),
                BTreeSet::from(["checks".into(), "evaluation".into()]),
            ),
        ]),
        None,
        Some("result".into()),
    );
    let mut signatures = BTreeMap::from([
        ("operator/seal".into(), seal),
        ("operator/check".into(), check.clone()),
        ("operator/accept".into(), accept),
    ]);
    for name in policy.checks.keys() {
        signatures.insert(format!("operator/check/{name}"), check.clone());
    }
    Ok(signatures)
}

pub struct CodeTaskDomain {
    policy_id: String,
    policy: CodeTaskPolicy,
    graph: CompiledTask,
    rust_toolchain_mapping: Option<PathBuf>,
}

impl CodeTaskDomain {
    pub fn captured(cas: &Cas, policy_id: &str, graph: CompiledTask) -> Result<Self, String> {
        let policy: CodeTaskPolicy =
            serde_json::from_value(cas.get_json(policy_id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let installed = code_signatures(policy_id, &policy)?;
        for node in graph.nodes.values() {
            if let CompiledOperator::Primitive {
                operator,
                signature,
            } = &node.operator
            {
                if matches!(
                    operator,
                    TaskOperatorV1::Seal {}
                        | TaskOperatorV1::Check { .. }
                        | TaskOperatorV1::Accept {}
                ) && installed
                    .get(signature)
                    .is_none_or(|s| s.contract != node.contract)
                {
                    return Err("Compiled code operator changed its installed contract".into());
                }
            }
        }
        Ok(Self {
            policy_id: policy_id.into(),
            policy,
            graph,
            rust_toolchain_mapping: None,
        })
    }

    /// Machine-local coordinator selection, never captured candidate authority.
    pub fn with_rust_toolchain_mapping(mut self, mapping: Option<PathBuf>) -> Self {
        self.rust_toolchain_mapping = mapping;
        self
    }

    fn operator(&self, input: &TaskInvocationV1) -> Result<&TaskOperatorV1, String> {
        match &self
            .graph
            .nodes
            .get(&input.node)
            .ok_or("Unknown code operator")?
            .operator
        {
            CompiledOperator::Primitive { operator, .. } => Ok(operator),
            _ => Err("Not a domain operator".into()),
        }
    }

    fn check_outcome(
        &self,
        cas: &Cas,
        receipt: &TaskCheckReceiptV1,
    ) -> Result<ReceiptOutcomeV1, String> {
        receipt.validate()?;
        if receipt.policy_id != self.policy_id {
            return Err("Check receipt uses another verifier policy".into());
        }
        let mut failed = false;
        let mut unavailable = false;
        let mut required = false;
        for (name, id) in &receipt.checks {
            let definition = self
                .policy
                .checks
                .get(name)
                .ok_or("Receipt names an unconfigured check")?;
            let result: CheckResult =
                serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if result.name != *name
                || result.required != definition.required
                || result.program.as_ref() != Some(&definition.command.program)
                || result.args != definition.command.args
            {
                return Err("Check result changed its captured definition".into());
            }
            for id in result.stdout.iter().chain(result.stderr.iter()) {
                cas.verify(id).map_err(|e| e.to_string())?;
            }
            if definition.required {
                required = true;
                failed |= result.status == CheckStatus::Failed;
                unavailable |= result.status == CheckStatus::NotRun;
            }
        }
        Ok(if !required || unavailable {
            ReceiptOutcomeV1::Inconclusive
        } else if failed {
            ReceiptOutcomeV1::Failed
        } else {
            ReceiptOutcomeV1::Passed
        })
    }

    fn checks(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        names: &BTreeSet<String>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(ArtifactInputV1, String), String> {
        let source = input.inputs.get("source").ok_or("Check needs source")?;
        let (snapshot_id, _, manifest) = source_snapshot(cas, source)?;
        let mut checks = BTreeMap::new();
        let mut spans = Vec::new();
        for name in names {
            let definition = self
                .policy
                .checks
                .get(name)
                .ok_or("Named check is not captured")?;
            let sandbox =
                Sandbox::materialize(&manifest, cas, Mode::ReadOnly).map_err(|e| e.to_string())?;
            review_sandbox::admit(self.policy.isolation(), &sandbox).map_err(|e| e.to_string())?;
            let runtime = tempfile::tempdir().map_err(|e| e.to_string())?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64;
            let remaining = attempt.reservation().deadline_unix_ms.saturating_sub(now);
            let remaining = self
                .policy
                .check_process_wall_ms
                .map_or(remaining, |limit| limit.min(remaining));
            let mut runner = CheckRunner::new(cas, sandbox.root())
                .with_cancellation(cancellation)
                .with_timeout(Duration::from_millis(remaining))
                .with_env("HOME", runtime.path().display().to_string())
                .with_env(
                    "XDG_CACHE_HOME",
                    runtime.path().join("cache").display().to_string(),
                )
                .with_env(
                    "CARGO_TARGET_DIR",
                    runtime.path().join("target").display().to_string(),
                );
            let mut toolchain_evidence = None;
            if let Some(request) = self
                .policy
                .rust_toolchain
                .as_ref()
                .filter(|request| request.checks.contains(name))
            {
                // Only this kernel-owned materialized root is canonicalized. Operator
                // mapping and seed paths retain their no-follow ancestor admission.
                let candidate_root = sandbox.root().canonicalize().map_err(|e| e.to_string())?;
                toolchain_evidence = Some(json!({"schema":"af.native-rust-toolchain/1",
                    "version":request.version,"requested_host":request.host,
                    "components":request.components,"materialization":"cold"}));
                if let Some(prepared) = prepare_native_rust_toolchain(
                    &candidate_root,
                    runtime.path(),
                    request,
                    self.rust_toolchain_mapping.as_deref(),
                )? {
                    toolchain_evidence = Some(json!({"schema":"af.native-rust-toolchain/1",
                        "content_digest":prepared.content_digest,"version":request.version,
                        "verified_release":prepared.verified_release,
                        "requested_host":request.host,"resolved_host":prepared.resolved_host,
                        "components":request.components,"materialization":"private_copy"}));
                    for (key, value) in prepared.environment {
                        runner = runner.with_env(key, value);
                    }
                }
            }
            // Preparation is charged to the same bounded check Attempt.
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64;
            let remaining =
                remaining.min(attempt.reservation().deadline_unix_ms.saturating_sub(now));
            runner = runner.with_timeout(Duration::from_millis(remaining));
            let (mut result, timing) = if remaining > 0 {
                let execution = runner.run_observed(definition);
                (
                    execution.result,
                    Some((execution.started_unix_ms, execution.elapsed_ms)),
                )
            } else {
                (
                    CheckResult {
                        name: name.clone(),
                        status: CheckStatus::NotRun,
                        exit_code: None,
                        reason: Some("Task check deadline expired".into()),
                        program: Some(definition.command.program.clone()),
                        args: definition.command.args.clone(),
                        stdout: None,
                        stderr: None,
                        required: definition.required,
                    },
                    None,
                )
            };
            if let Some((started_unix_ms, elapsed_ms)) = timing {
                let span_id = cas
                    .put_json(&json!([
                        attempt.task_id(),
                        attempt.id(),
                        input.node,
                        name,
                        started_unix_ms,
                        elapsed_ms
                    ]))
                    .map_err(|e| e.to_string())?;
                spans.push(TaskRuntimeSpanV1 {
                    span_id,
                    kind: TaskRuntimeSpanKindV1::Check,
                    label: name.clone(),
                    started_unix_ms,
                    elapsed_ms,
                });
            }
            if let Some(evidence) = toolchain_evidence {
                let mut diagnostic = match &result.stderr {
                    Some(id) => cas.get(id).map_err(|e| e.to_string())?,
                    None => Vec::new(),
                };
                diagnostic.extend_from_slice(
                    b"
AF_TOOLCHAIN_SNAPSHOT ",
                );
                diagnostic.extend_from_slice(evidence.to_string().as_bytes());
                diagnostic.push(10);
                result.stderr = Some(cas.put(&diagnostic).map_err(|e| e.to_string())?);
            }
            let sealed = sandbox.seal().map_err(|e| e.to_string())?;
            if !sealed.unchanged() {
                result.status = CheckStatus::Failed;
                result.reason = Some("Check mutated its input Snapshot".into());
            }
            let id = cas
                .put_json(&serde_json::to_value(result).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            checks.insert(name.clone(), id);
        }
        let mut receipt = TaskCheckReceiptV1 {
            plan_id: input.plan_id.clone(),
            snapshot_id: snapshot_id.clone(),
            policy_id: self.policy_id.clone(),
            outcome: ReceiptOutcomeV1::Inconclusive,
            checks,
        };
        receipt.outcome = self.check_outcome(cas, &receipt)?;
        let refs = receipt
            .checks
            .values()
            .cloned()
            .chain(source.artifact_ids.iter().cloned())
            .chain([input.plan_id.clone(), self.policy_id.clone()])
            .collect();
        let id = cas
            .put_artifact(
                TASK_CHECK_RECEIPT_V1,
                invocation_producer(cas, input, Some(attempt))?,
                refs,
                Some(snapshot_id.clone()),
                serde_json::to_value(receipt).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        let evidence = TaskRuntimeEvidenceV1 {
            task_id: attempt.task_id().into(),
            attempt_id: attempt.id().into(),
            node: input.node.clone(),
            context_id: attempt.context_id().into(),
            spans,
            caches: vec![],
        };
        evidence.validate()?;
        let evidence_id = cas
            .put_artifact(
                TASK_RUNTIME_EVIDENCE_V1,
                invocation_producer(cas, input, Some(attempt))?,
                std::iter::once(attempt.context_id().to_owned())
                    .chain(evidence.spans.iter().map(|span| span.span_id.clone()))
                    .collect(),
                Some(snapshot_id.clone()),
                serde_json::to_value(evidence).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        Ok((
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: TASK_CHECK_RECEIPT_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: Some(snapshot_id),
            },
            evidence_id,
        ))
    }

    fn verification(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<VerificationResultV1, String> {
        let snapshot_id = source_input(
            cas,
            input
                .inputs
                .get("source")
                .ok_or("Verification needs source")?,
        )?;
        let checks = input
            .inputs
            .get("checks")
            .ok_or("Verification needs exact checks")?;
        let check_receipt_id = checks
            .artifact_ids
            .first()
            .ok_or("Empty check receipt")?
            .clone();
        let artifact = envelope(cas, &check_receipt_id)?;
        if artifact.artifact_type != TASK_CHECK_RECEIPT_V1
            || checks.snapshot_id.as_ref() != Some(&snapshot_id)
        {
            return Err("Verification checks have stale type or Snapshot".into());
        }
        let receipt: TaskCheckReceiptV1 =
            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
        if receipt.snapshot_id != snapshot_id
            || receipt.plan_id != input.plan_id
            || receipt.outcome != self.check_outcome(cas, &receipt)?
        {
            return Err("Verification check receipt has stale or invalid authority".into());
        }
        // All mandatory policy checks must be retained, even if a Pipeline names a subset.
        if self
            .policy
            .checks
            .iter()
            .any(|(name, definition)| definition.required && !receipt.checks.contains_key(name))
        {
            return Err("Verification omitted a required check".into());
        }
        let evaluation_id = input
            .inputs
            .get("evaluation")
            .and_then(|p| p.artifact_ids.first())
            .cloned();
        let outcome = match (receipt.outcome, &evaluation_id) {
            (ReceiptOutcomeV1::Passed, Some(id)) => {
                let artifact = envelope(cas, id)?;
                if artifact.artifact_type != TASK_EVALUATION_V1
                    || artifact.subject_snapshot_id.as_ref() != Some(&snapshot_id)
                {
                    return Err("Evaluator receipt is stale".into());
                }
                let evaluation: TaskEvaluationV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                evaluation.validate()?;
                evaluation.outcome
            }
            (ReceiptOutcomeV1::Passed, None) => ReceiptOutcomeV1::Inconclusive,
            (other, _) => other,
        };
        let result = VerificationResultV1 {
            plan_id: input.plan_id.clone(),
            snapshot_id,
            policy_id: self.policy_id.clone(),
            outcome,
            check_receipt_id,
            evaluation_id,
        };
        result.validate()?;
        Ok(result)
    }

    /// Review uses the same current-Snapshot check validation without requiring an additional
    /// implementation evaluator. This grants no review or generic Task acceptance by itself.
    pub(super) fn review_checks(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<ReceiptOutcomeV1, String> {
        let mut checks_only = input.clone();
        checks_only.inputs.remove("evaluation");
        let validated = self.verification(cas, &checks_only)?;
        let receipt: TaskCheckReceiptV1 =
            serde_json::from_value(envelope(cas, &validated.check_receipt_id)?.payload)
                .map_err(|e| e.to_string())?;
        Ok(receipt.outcome)
    }

    fn accept(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let result = self.verification(cas, input)?;
        let snapshot_id = result.snapshot_id.clone();
        let producer = invocation_producer(cas, input, None)?;
        let refs: Vec<_> = input
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.iter().cloned())
            .collect();
        let id = cas
            .put_artifact(
                VERIFICATION_RESULT_V1,
                producer.clone(),
                refs.clone(),
                Some(snapshot_id.clone()),
                serde_json::to_value(result).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        let snapshot = source_tree(
            cas,
            producer,
            &snapshot_id,
            refs.into_iter().chain([id.clone()]).collect(),
        )?;
        Ok(BTreeMap::from([
            ("snapshot".into(), snapshot),
            (
                "result".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![id],
                    artifact_type: VERIFICATION_RESULT_V1.into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: Some(snapshot_id),
                },
            ),
        ]))
    }

    pub fn result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let execution = state.execution.as_ref().ok_or("Task has no execution")?;
        let outputs = self
            .graph
            .outputs
            .iter()
            .filter_map(|(name, address)| {
                execution
                    .outputs
                    .get(&address.node)
                    .and_then(|(_, out)| out.outputs.get(&address.port))
                    .map(|value| (name.clone(), value.clone()))
            })
            .collect();
        let evidence = self
            .graph
            .coverage
            .values()
            .filter_map(|address| {
                execution
                    .outputs
                    .get(&address.node)
                    .and_then(|(_, out)| out.outputs.get(&address.port))
            })
            .flat_map(|port| port.artifact_ids.iter().cloned())
            .collect();
        let mut result = TaskResultV1 {
            task_revision_id: state.revision_id.clone(),
            execution: TaskExecutionV1::Completed,
            acceptance: TaskAcceptanceV1::Inconclusive,
            domain_conclusion: "incomplete".into(),
            outputs,
            evidence,
            missing_obligations: BTreeSet::new(),
        };
        if report
            .outcomes
            .iter()
            .any(|(_, outcome)| matches!(outcome, NodeOutcome::Failed { .. }))
        {
            result.execution = TaskExecutionV1::Exhausted;
        }
        self.assess(cas, &state.revision, &mut result)?;
        result.validate()?;
        Ok(result)
    }

    pub(super) fn assess(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        let mut missing = BTreeSet::new();
        let mut failed = false;
        for (name, obligation) in &task.acceptance {
            let address = self
                .graph
                .coverage
                .get(name)
                .ok_or("Code Task lacks named acceptance coverage")?;
            let origins = self.graph.evidence_origins(address)?;
            let mut found = false;
            let mut passed = false;
            for id in &result.evidence {
                let artifact = envelope(cas, id)?;
                if !matches!(&artifact.producer, review_core::Producer::KernelOperation {node_id:Some(node),..} if origins.iter().any(|a| &a.node == node))
                {
                    continue;
                }
                if found {
                    return Err("Ambiguous named code acceptance evidence".into());
                }
                found = true;
                if artifact.artifact_type != obligation.evidence_type {
                    continue;
                }
                if artifact.artifact_type != VERIFICATION_RESULT_V1 {
                    return Err("Unsupported code acceptance evidence".into());
                }
                let receipt: VerificationResultV1 =
                    serde_json::from_value(artifact.payload.clone()).map_err(|e| e.to_string())?;
                receipt.validate()?;
                self.validate_result_chain(cas, task, result, &artifact, &receipt)?;
                if receipt.policy_id != obligation.verifier_policy
                    || receipt.policy_id != self.policy_id
                {
                    return Err("Code acceptance uses another verifier policy".into());
                }
                failed |= receipt.outcome == ReceiptOutcomeV1::Failed;
                passed |= receipt.outcome == ReceiptOutcomeV1::Passed;
            }
            if !passed {
                missing.insert(name.clone());
            }
        }
        // Passed public receipts cannot make incomplete planned work a completed Task.
        // A genuine negative receipt remains Unsatisfied independently of execution status.
        // Use the same rule when Store revalidates the terminal result.
        result.acceptance = if missing.is_empty() && result.execution == TaskExecutionV1::Completed
        {
            TaskAcceptanceV1::Satisfied
        } else if failed {
            TaskAcceptanceV1::Unsatisfied
        } else {
            TaskAcceptanceV1::Inconclusive
        };
        result.domain_conclusion = match result.acceptance {
            TaskAcceptanceV1::Satisfied => "verified",
            TaskAcceptanceV1::Unsatisfied => "changes_requested",
            TaskAcceptanceV1::Inconclusive => "incomplete",
        }
        .into();
        result.missing_obligations = missing;
        Ok(())
    }

    fn validate_result_chain(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
        artifact: &review_core::ArtifactEnvelope,
        receipt: &VerificationResultV1,
    ) -> Result<(), String> {
        let plan_envelope = envelope(cas, &receipt.plan_id)?;
        if plan_envelope.artifact_type != EXECUTION_PLAN_V1 {
            return Err("Verification result has no exact ExecutionPlan".into());
        }
        let plan: ExecutionPlanV1 =
            serde_json::from_value(plan_envelope.payload).map_err(|e| e.to_string())?;
        if plan.task_revision_id != result.task_revision_id
            || plan.authority != task.authority
            || envelope(cas, &plan.compiled_graph_id)?.payload
                != serde_json::to_value(&self.graph).map_err(|e| e.to_string())?
        {
            return Err("Verification result belongs to another Task or compiled plan".into());
        }
        let run_id =
            review_store::store::task::task_run_id(&task.task_id).map_err(|e| e.to_string())?;
        let node = match &artifact.producer {
            review_core::Producer::KernelOperation {
                run_id: recorded,
                node_id: Some(node),
                ..
            } if *recorded == run_id => node,
            _ => {
                return Err(
                    "Verification result was not assembled by this Task's installed operator"
                        .into(),
                );
            }
        };
        if !matches!(
            self.graph.nodes.get(node).map(|n| &n.operator),
            Some(CompiledOperator::Primitive {
                operator: TaskOperatorV1::Accept {},
                ..
            })
        ) {
            return Err("Verification result was not produced by accept".into());
        }
        let source = result
            .outputs
            .get("snapshot")
            .ok_or("Verification result has no public Snapshot")?;
        if source_input(cas, source)? != receipt.snapshot_id
            || artifact.subject_snapshot_id.as_ref() != Some(&receipt.snapshot_id)
        {
            return Err("Verification result is stale for the public Snapshot".into());
        }
        let receipt_port = |id: &str, artifact_type: &str| ArtifactInputV1 {
            artifact_ids: vec![id.into()],
            artifact_type: artifact_type.into(),
            cardinality: PortCardinality::One,
            snapshot_id: Some(receipt.snapshot_id.clone()),
        };
        let mut inputs = BTreeMap::from([
            ("source".into(), source.clone()),
            (
                "checks".into(),
                receipt_port(&receipt.check_receipt_id, TASK_CHECK_RECEIPT_V1),
            ),
        ]);
        for (id, evaluator) in std::iter::once((&receipt.check_receipt_id, false))
            .chain(receipt.evaluation_id.iter().map(|id| (id, true)))
        {
            let evidence = envelope(cas, id)?;
            if evaluator {
                let requirements = task
                    .inputs
                    .get("requirements")
                    .ok_or("Evaluation lacks the exact Task Requirements")?;
                if requirements.artifact_type != "af/Requirements@1"
                    || requirements.artifact_ids.is_empty()
                    || requirements
                        .artifact_ids
                        .iter()
                        .any(|id| !evidence.input_artifacts.contains(id))
                {
                    return Err("Evaluation did not retain the exact Task Requirements".into());
                }
            }
            let upstream = match &evidence.producer {
                review_core::Producer::Attempt {
                    run_id: recorded,
                    node_id,
                    ..
                } if *recorded == run_id => node_id,
                _ => return Err("Verification evidence has no current Task Attempt".into()),
            };
            let operator = self.graph.nodes.get(upstream).map(|n| &n.operator);
            if if evaluator {
                !matches!(
                    operator,
                    Some(CompiledOperator::Primitive {
                        operator: TaskOperatorV1::Verify { .. },
                        ..
                    })
                )
            } else {
                !matches!(
                    operator,
                    Some(CompiledOperator::Primitive {
                        operator: TaskOperatorV1::Check { .. },
                        ..
                    })
                )
            } {
                return Err("Verification evidence came from another operator role".into());
            }
        }
        if let Some(id) = &receipt.evaluation_id {
            inputs.insert("evaluation".into(), receipt_port(id, TASK_EVALUATION_V1));
        }
        if self.verification(
            cas,
            &TaskInvocationV1 {
                plan_id: receipt.plan_id.clone(),
                node: node.clone(),
                inputs,
            },
        )? != *receipt
        {
            return Err(
                "Verification outcome contradicts its retained check/evaluator receipts".into(),
            );
        }
        Ok(())
    }
}

impl CodeTaskDomain {
    /// The built-in context bytes for this invocation. The trait entry point and the
    /// admission recheck must render identically, so both go through here.
    fn render_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        let refs = input
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.iter().cloned())
            .chain(feedback.iter().cloned())
            .chain([input.plan_id.clone(), self.policy_id.clone()])
            .collect();
        cas.put_artifact(
            "af/TaskBuiltinContext@1",
            invocation_producer(cas, input, None)?,
            refs,
            None,
            json!({"invocation":input,"feedback_ids":feedback,"policy_id":self.policy_id}),
        )
        .map(|(id, _)| id)
        .map_err(|e| e.to_string())
    }
}

impl TaskOperatorHost for CodeTaskDomain {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        attempt: &review_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.render_context(cas, input, attempt.feedback_ids())
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = super::control::check(cancellation) {
            return super::control::refused(error);
        }

        let mut raw_artifact_ids = Vec::new();
        let outputs = (|| match self.operator(input)? {
            TaskOperatorV1::Seal {} => Ok(BTreeMap::from([(
                "snapshot".into(),
                seal_candidate(cas, input)?,
            )])),
            TaskOperatorV1::Check { checks } => {
                let (receipt, evidence_id) = self.checks(
                    cas,
                    input,
                    attempt.ok_or("Check has no started Attempt")?,
                    checks,
                    cancellation,
                )?;
                raw_artifact_ids.push(evidence_id);
                Ok(BTreeMap::from([("result".into(), receipt)]))
            }
            TaskOperatorV1::Accept {} => self.accept(cas, input),
            _ => Err("Code operator requires its captured Worker or domain adapter".into()),
        })();
        TaskWorkOutput {
            usage_observation: None,
            usage: None,
            outputs,
            charged_tokens: Some(0),
            raw_artifact_ids,
            usage_id: None,
            feedback_id: None,
        }
    }
}

impl TaskDomain for CodeTaskDomain {
    fn assemble_result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        self.result(cas, state, report)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &review_store::store::task::execution::ReservedTaskAttempt,
        id: &str,
    ) -> Result<(), String> {
        let feedback = attempt.feedback_ids();
        match self.operator(input)? {
            TaskOperatorV1::Verify { .. } => {
                let result = self.verification(cas, input)?;
                let check: TaskCheckReceiptV1 =
                    serde_json::from_value(envelope(cas, &result.check_receipt_id)?.payload)
                        .map_err(|e| e.to_string())?;
                if check.outcome != ReceiptOutcomeV1::Passed {
                    return Err("Evaluator cannot dispatch before its current checks pass".into());
                }
                Ok(())
            }
            TaskOperatorV1::Worker { .. } => Ok(()),
            _ => {
                envelope(cas, id)?;
                if self.render_context(cas, input, feedback)? != id {
                    return Err("Built-in context changed its exact invocation".into());
                }
                Ok(())
            }
        }
    }
    fn validate_output(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        _definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        match self.operator(input)? {
            TaskOperatorV1::Seal {} => validate_seal(cas, input, output),
            TaskOperatorV1::Check { checks } => {
                let value = &output.outputs["result"];
                let receipt: TaskCheckReceiptV1 =
                    serde_json::from_value(envelope(cas, &value.artifact_ids[0])?.payload)
                        .map_err(|e| e.to_string())?;
                if !receipt.checks.keys().eq(checks.iter())
                    || receipt.plan_id != input.plan_id
                    || receipt.snapshot_id != source_input(cas, &input.inputs["source"])?
                    || receipt.outcome != self.check_outcome(cas, &receipt)?
                {
                    return Err("Check receipt changed its invocation or results".into());
                }
                Ok(())
            }
            TaskOperatorV1::Accept {} => {
                let actual: VerificationResultV1 = serde_json::from_value(
                    envelope(cas, &output.outputs["result"].artifact_ids[0])?.payload,
                )
                .map_err(|e| e.to_string())?;
                if actual != self.verification(cas, input)?
                    || source_input(cas, &output.outputs["snapshot"])? != actual.snapshot_id
                {
                    return Err(
                        "Acceptance output differs from its current verification evidence".into(),
                    );
                }
                Ok(())
            }
            TaskOperatorV1::Verify { .. } => {
                for port in output.outputs.values() {
                    for id in &port.artifact_ids {
                        let artifact = envelope(cas, id)?;
                        if artifact.artifact_type != TASK_EVALUATION_V1
                            || artifact.subject_snapshot_id.as_ref()
                                != input.inputs["source"].snapshot_id.as_ref()
                        {
                            return Err("Evaluation output has a stale Snapshot".into());
                        }
                        let value: TaskEvaluationV1 =
                            serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                        value.validate()?;
                    }
                }
                Ok(())
            }
            TaskOperatorV1::Worker { .. } => Ok(()),
            _ => Err("Unsupported code output admission".into()),
        }
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let mut expected = result.clone();
        self.assess(cas, task, &mut expected)?;
        if expected != *result {
            return Err("Task result changed its receipt-derived acceptance".into());
        }
        Ok(())
    }
}
