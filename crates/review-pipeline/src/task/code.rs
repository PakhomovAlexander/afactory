//! Implementation domain handlers over the common Task runtime. Checks and evaluation refer
//! to the sealed Snapshot; negative checks produce a result without an evaluator dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_check::{CheckDefinition, CheckResult, CheckRunner, CheckStatus};
use review_core::PortCardinality;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::measurement::{
    ComparisonObjective, MEASUREMENT_COMPARISON_V1, MEASUREMENT_V1, MeasureDefinitionV1,
    MeasureObjectiveV1, MeasurementV1, compare_measurements,
};
use review_core::task::pipeline::*;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::remote_check::{
    PUBLISH_GATE_EFFECT, REMOTE_CHECK_EVIDENCE_V1, RemoteCheckEvidenceV1, RemoteCheckReasonV1,
    github_of_destination,
};
use review_core::task::runtime::{
    TASK_RUNTIME_EVIDENCE_V1, TaskRuntimeCheckOutcomeV1, TaskRuntimeCheckV1, TaskRuntimeEvidenceV1,
    TaskRuntimeSpanKindV1, TaskRuntimeSpanV1,
};
use review_core::task::verification::*;
use review_core::task::*;
use review_graph::task::{
    CompiledOperator, CompiledTask, OperatorAttemptCost, OperatorSignature,
    REMOTE_CHECK_SIGNATURE_PREFIX,
};
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
use super::remote_check::github_pr::{self, RemotePhase};
use super::remote_check::{
    GithubPrTarget, MAPPING_KNOB, RemoteCheckHost, RemoteCheckMapping, RemoteCheckOutcome,
    RemoteCheckRequest, result_matches_evidence,
};
use super::source::{
    invocation_producer, seal_candidate, source_input, source_snapshot, validate_seal,
};
use super::warm_check::{
    CacheSourceResolver, CodeWarmPolicy, DEADLINE_EXHAUSTED, Excess, PreparedCheck, RustupHome,
    ToolchainDeclaration, WARM_CACHE_BOUND_EXCEEDED, WARM_CACHE_SUSPECT, WarmCheckHost,
    WarmSession,
};
use super::{TaskOperatorHost, TaskWorkOutput, envelope};

mod measure;

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
    /// The Warm Check Cache (ADR-0131). Absent, every check builds cold and the captured policy
    /// is byte-identical to one written before the table existed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub warm: Option<CodeWarmPolicy>,
    /// Declared measured commands (ADR-0132). Absent, the captured policy is byte-identical to
    /// one written before the table existed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub measures: BTreeMap<String, MeasureDefinitionV1>,
    /// Declared objectives a `compare` node folds two Measurements under.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub objectives: BTreeMap<String, MeasureObjectiveV1>,
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
        for (name, check) in &self.checks {
            if let Some(remote) = &check.remote {
                remote
                    .validate()
                    .map_err(|error| format!("Code policy check {name}: {error}"))?;
            }
        }
        if let Some(warm) = &self.warm {
            if self.require_container {
                return Err(
                    "Code policy declares [warm] together with require_container = true: a Warm \
                     Check Cache is machine-local, candidate-built state that only trusted_local \
                     checks may use, so a container policy refuses it"
                        .into(),
                );
            }
            warm.validate()?;
        }
        if self.measures.len() > 32 || self.objectives.len() > 32 {
            return Err("Code policy declares at most 32 measures and 32 objectives".into());
        }
        for (name, measure) in &self.measures {
            if !is_name(name) {
                return Err(format!("Code policy measure name {name:?} is invalid"));
            }
            measure
                .validate()
                .map_err(|error| format!("Code policy measure {name}: {error}"))?;
            if measure.warm
                && !self.warm.as_ref().is_some_and(|warm| {
                    warm.build_cache
                        .contains(&super::warm_check::WarmBuildCacheKind::CargoTarget)
                })
            {
                return Err(format!(
                    "Code policy measure {name} declares warm = true, which needs [warm] \
                     build_cache to declare cargo_target"
                ));
            }
        }
        for (name, objective) in &self.objectives {
            if !is_name(name) {
                return Err(format!("Code policy objective name {name:?} is invalid"));
            }
            objective
                .validate(&self.measures)
                .map_err(|error| format!("Code policy objective {name}: {error}"))?;
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

/// The reason every declared warm kind of a remote check records (ADR-0140).
const REMOTE_SKIP: &str = "remote";

/// The Task's captured source for `candidate`: its root ancestor along `parent_snapshot_id`,
/// or the candidate itself when it has no parent.
fn root_snapshot(
    cas: &Cas,
    candidate_id: &str,
    candidate: &review_source_git::task::TaskSnapshot,
) -> Result<(String, review_source_git::Manifest), String> {
    let mut id = candidate_id.to_owned();
    let mut parent = candidate.parent_snapshot_id.clone();
    let mut manifest = None;
    for _ in 0..4096 {
        let Some(next) = parent else {
            let manifest = match manifest {
                Some(manifest) => manifest,
                None => review_source_git::task::read_snapshot(cas, &id)?.1,
            };
            return Ok((id, manifest));
        };
        let (snapshot, read) = review_source_git::task::read_snapshot(cas, &next)?;
        id = next;
        parent = snapshot.parent_snapshot_id;
        manifest = Some(read);
    }
    Err("Task Snapshot ancestry is deeper than 4096 Snapshots".into())
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
    for (name, definition) in &policy.checks {
        signatures.insert(format!("operator/check/{name}"), check.clone());
        // A check declared with a `remote` table installs its remote form, which a check node
        // may list in `remote_checks` (ADR-0140). Its effect names what running it there does,
        // so a Planner, offered only what the Task's authority already permits, never sees it.
        if definition.remote.is_some() {
            let mut remote = check.clone();
            remote.effects.insert(PUBLISH_GATE_EFFECT.into());
            signatures.insert(format!("{REMOTE_CHECK_SIGNATURE_PREFIX}{name}"), remote);
        }
    }
    signatures.extend(measure_signatures(policy));
    Ok(signatures)
}

fn measurement_port(name: &str) -> (String, PipelinePortV1) {
    (name.into(), port(MEASUREMENT_V1, same("source")))
}

/// The installed measure and compare operators of a policy that declares measures (ADR-0132).
/// `operator/measure` carries the one Attempt's wall, the captured `check_wall_ms`, and every
/// declared measure's output; `operator/measure/<name>` carries that measure's one output and
/// its repetition budget, which the compiler sums per node. `operator/compare/<objective>`
/// installs an objective, and `operator/compare/<objective>/<measure>` the one measure it
/// compares. A policy without measures installs nothing, so its signatures are unchanged.
fn measure_signatures(policy: &CodeTaskPolicy) -> BTreeMap<String, OperatorSignature> {
    let mut signatures = BTreeMap::new();
    if policy.measures.is_empty() {
        return signatures;
    }
    let measure =
        |outputs: BTreeMap<String, PipelinePortV1>, wall_ms, outcome_port| OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([(
                    "source".into(),
                    port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
                )]),
                outputs,
            },
            effects: BTreeSet::from(["execute-checks".into()]),
            evidence: BTreeMap::new(),
            retains: BTreeMap::new(),
            roles: BTreeSet::new(),
            worker_input_type: None,
            worker_output_type: None,
            outcome_port,
            attempt: Some(OperatorAttemptCost { tokens: 0, wall_ms }),
        };
    signatures.insert(
        "operator/measure".into(),
        measure(
            policy
                .measures
                .keys()
                .map(|name| measurement_port(name))
                .collect(),
            policy.check_wall_ms,
            None,
        ),
    );
    for (name, definition) in &policy.measures {
        signatures.insert(
            format!("operator/measure/{name}"),
            measure(
                BTreeMap::from([measurement_port(name)]),
                definition.budget_ms().unwrap_or(u64::MAX),
                Some(name.clone()),
            ),
        );
    }
    if policy.objectives.is_empty() {
        return signatures;
    }
    let compare = OperatorSignature {
        contract: PipelineContractV1 {
            inputs: BTreeMap::from([
                (
                    "baseline".into(),
                    port(MEASUREMENT_V1, PortAffinityV1::Unbound {}),
                ),
                (
                    "candidate".into(),
                    port(MEASUREMENT_V1, PortAffinityV1::Unbound {}),
                ),
            ]),
            outputs: BTreeMap::from([(
                "result".into(),
                port(MEASUREMENT_COMPARISON_V1, same("candidate")),
            )]),
        },
        effects: BTreeSet::new(),
        evidence: BTreeMap::new(),
        retains: BTreeMap::from([(
            "result".into(),
            BTreeSet::from(["baseline".into(), "candidate".into()]),
        )]),
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        outcome_port: Some("result".into()),
        attempt: None,
    };
    signatures.insert("operator/compare".into(), compare.clone());
    for (name, objective) in &policy.objectives {
        signatures.insert(format!("operator/compare/{name}"), compare.clone());
        signatures.insert(
            format!("operator/compare/{name}/{}", objective.measure),
            compare.clone(),
        );
    }
    signatures
}

/// The contract a compiled measure node must carry: the installed source input and one
/// Measurement per named measure.
fn measure_contract(
    installed: &BTreeMap<String, OperatorSignature>,
    measures: &BTreeSet<String>,
) -> Option<PipelineContractV1> {
    let base = installed.get("operator/measure")?;
    let mut outputs = BTreeMap::new();
    for name in measures {
        outputs.extend(
            installed
                .get(&format!("operator/measure/{name}"))?
                .contract
                .outputs
                .clone(),
        );
    }
    Some(PipelineContractV1 {
        inputs: base.contract.inputs.clone(),
        outputs,
    })
}

pub struct CodeTaskDomain {
    policy_id: String,
    policy: CodeTaskPolicy,
    graph: CompiledTask,
    warm: WarmCheckHost,
    rust_toolchain_mapping: Option<PathBuf>,
    remote: RemoteCheckHost,
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
                        | TaskOperatorV1::Compare { .. }
                ) && installed
                    .get(signature)
                    .is_none_or(|s| s.contract != node.contract)
                {
                    return Err("Compiled code operator changed its installed contract".into());
                }
                if let TaskOperatorV1::Compare { objective } = operator
                    && !policy.objectives.contains_key(objective)
                {
                    return Err("Compiled comparison names an objective the policy lacks".into());
                }
                if let TaskOperatorV1::Check {
                    checks,
                    remote_checks,
                } = operator
                    && (!checks.is_disjoint(remote_checks)
                        || remote_checks.iter().any(|name| {
                            policy
                                .checks
                                .get(name)
                                .is_none_or(|check| check.remote.is_none())
                        }))
                {
                    return Err(
                        "Compiled check node names a remote check the policy does not declare \
                         with a `remote` table"
                            .into(),
                    );
                }
                if let TaskOperatorV1::Measure { measures } = operator
                    && measure_contract(&installed, measures).as_ref() != Some(&node.contract)
                {
                    return Err("Compiled measure changed its installed contract".into());
                }
            }
        }
        Ok(Self {
            policy_id: policy_id.into(),
            policy,
            graph,
            warm: WarmCheckHost::default(),
            rust_toolchain_mapping: None,
            remote: RemoteCheckHost::default(),
        })
    }

    /// Resolve a `[warm] caches` kind through the machine's cache policy, as a Gate resolves
    /// `[gate] caches`. Without a resolver a declared Cache Snapshot is unavailable and its check
    /// does not run.
    pub fn with_cache_source_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(
                review_sandbox::CacheKind,
            ) -> Result<review_sandbox::CacheSource, review_sandbox::CacheError>
            + Send
            + Sync
            + 'static,
    {
        self.warm.resolver =
            Some(std::sync::Arc::new(resolver) as std::sync::Arc<CacheSourceResolver>);
        self
    }

    /// Keep warm build directories below `root` instead of `$XDG_CACHE_HOME/af/task-build-cache`.
    pub fn with_task_build_cache_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.warm.root = Some(root.into());
        self
    }

    /// How long a check waits for another holder of its warm directory before it runs cold.
    pub fn with_task_build_cache_lock_wait(mut self, wait: Duration) -> Self {
        self.warm.lock_wait = wait;
        self
    }

    /// Machine-local coordinator selection, never captured candidate authority.
    pub fn with_rust_toolchain_mapping(mut self, mapping: Option<PathBuf>) -> Self {
        self.rust_toolchain_mapping = mapping;
        self
    }

    /// Machine-local Remote Check configuration (ADR-0140): the operator's mapping, the Task
    /// owner resolver and the executor's settings. Never captured candidate authority.
    pub fn with_remote_checks(mut self, remote: RemoteCheckHost) -> Self {
        self.remote = remote;
        self
    }

    /// The push target of a check node with remote checks, read again at run time (ADR-0140).
    /// The pipeline chose the checks and the plan recorded the destination its developer
    /// confirmed; the mapping must still name exactly that `github` for the source Snapshot's
    /// repository. Anything else ends the Attempt before any check starts, so nothing is pushed.
    /// A node without remote checks never calls this, and never reads the mapping.
    fn remote_target(
        &self,
        cas: &Cas,
        plan_id: &str,
        snapshot: &review_source_git::task::TaskSnapshot,
    ) -> Result<GithubPrTarget, String> {
        let plan: ExecutionPlanV1 =
            serde_json::from_value(envelope(cas, plan_id)?.payload).map_err(|e| e.to_string())?;
        let recorded: Vec<&str> = plan
            .authority
            .data_destinations
            .iter()
            .filter_map(|destination| github_of_destination(destination))
            .collect();
        let ([recorded], true) = (
            recorded.as_slice(),
            plan.authority.allowed_effects.contains(PUBLISH_GATE_EFFECT),
        ) else {
            return Err(format!(
                "A check node with remote checks needs a plan whose authority carries \
                 `{PUBLISH_GATE_EFFECT}` and one `github:` destination; plan the Task again"
            ));
        };
        let origin = review_source_git::task::read_origin(cas, &snapshot.origin_id)?;
        let repository = origin.repository_id();
        let mapping = match &self.remote.mapping {
            Some(path) => RemoteCheckMapping::read(path)?,
            None => None,
        };
        let target = mapping
            .as_ref()
            .and_then(|mapping| mapping.target(repository))
            .ok_or_else(|| {
                format!(
                    "{MAPPING_KNOB} no longer names a push target for repository {repository}, \
                     which this plan publishes to github:{recorded}; nothing was pushed. \
                     Restore its [[github_pr]] entry, or plan the Task again"
                )
            })?;
        if target.github != *recorded {
            return Err(format!(
                "{MAPPING_KNOB} now names github {} for repository {repository}, but the plan \
                 recorded github:{recorded}, the destination its developer confirmed; nothing \
                 was pushed. Restore the entry, or plan the Task again",
                target.github
            ));
        }
        Ok(target.clone())
    }

    /// The reader of one recorded check receipt: every result validated against its captured
    /// definition — a local result by its command, a remote result by its evidence — and the
    /// outcome those results derive.
    pub fn check_receipt_outcome(
        &self,
        cas: &Cas,
        receipt: &TaskCheckReceiptV1,
    ) -> Result<ReceiptOutcomeV1, String> {
        self.check_outcome(cas, receipt)
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
                || !result.has_one_shape()
            {
                return Err("Check result changed its captured definition".into());
            }
            match &result.remote {
                None => {
                    if result.program.as_ref() != Some(&definition.command.program)
                        || result.args != definition.command.args
                    {
                        return Err("Check result changed its captured definition".into());
                    }
                }
                Some(evidence_id) => {
                    // A remote result is validated by its evidence, never by a command: the
                    // definition must declare `remote`, the evidence must be exactly that
                    // declaration's for this Snapshot, and the status the one it derives.
                    let declared = definition
                        .remote
                        .as_ref()
                        .ok_or("Check result changed its captured definition")?;
                    let artifact = envelope(cas, evidence_id)?;
                    if artifact.artifact_type != REMOTE_CHECK_EVIDENCE_V1
                        || artifact.subject_snapshot_id.as_ref() != Some(&receipt.snapshot_id)
                    {
                        return Err("Remote check evidence has another type or Snapshot".into());
                    }
                    let evidence: RemoteCheckEvidenceV1 =
                        serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                    evidence.validate()?;
                    // Only jobs can have left a log: an excerpt beside evidence that observed
                    // none is a result nobody recorded.
                    if result.stdout.is_some()
                        && evidence.state
                            != review_core::task::remote_check::RemoteCheckStateV1::Observed
                    {
                        return Err("Check result changed its captured definition".into());
                    }
                    // The plan recorded where its remote checks publish, and its developer
                    // confirmed that destination: evidence gathered at any other repository
                    // is not this plan's, whatever it says about the Snapshot.
                    let plan: ExecutionPlanV1 =
                        serde_json::from_value(envelope(cas, &receipt.plan_id)?.payload)
                            .map_err(|e| e.to_string())?;
                    let destinations: Vec<&str> = plan
                        .authority
                        .data_destinations
                        .iter()
                        .filter_map(|destination| github_of_destination(destination))
                        .collect();
                    if destinations.as_slice() != [evidence.github.as_str()] {
                        return Err(
                            "Remote check evidence names another repository than its plan's \
                             recorded destination"
                                .into(),
                        );
                    }
                    if evidence.declaration() != *declared
                        || evidence.snapshot_id != receipt.snapshot_id
                        || !result_matches_evidence(
                            result.status,
                            result.reason.as_deref(),
                            &evidence,
                        )
                    {
                        return Err("Check result changed its captured definition".into());
                    }
                }
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
        local: &BTreeSet<String>,
        remote_names: &BTreeSet<String>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(ArtifactInputV1, Vec<String>), String> {
        let source = input.inputs.get("source").ok_or("Check needs source")?;
        let (snapshot_id, snapshot, manifest) = source_snapshot(cas, source)?;
        // The pipeline chose the remote checks (ADR-0140); their target is settled before any
        // check starts: a mapping that no longer names the plan's destination, or a Task whose
        // Store identity cannot be read, ends the Attempt here.
        let remote = if remote_names.is_empty() {
            None
        } else {
            let target = self.remote_target(cas, &input.plan_id, &snapshot)?;
            let resolver = self
                .remote
                .owner
                .as_ref()
                .ok_or("Remote checks need the Task's Store identity, which this host lacks")?;
            Some((target, remote_names, resolver(attempt.task_id())?))
        };
        let mut local_failed = Vec::new();
        let mut session = match &self.policy.warm {
            Some(warm) => Some(WarmSession::new(
                &self.warm,
                warm,
                review_source_git::task::read_origin(cas, &snapshot.origin_id)?.repository_id(),
                ToolchainDeclaration::from_manifest(cas, &manifest)?,
            )),
            None => None,
        };
        // Under [warm] the probe and the check see the kernel's rustup home, so an installed
        // toolchain answers at once and a missing one fails instead of downloading.
        let rustup = session.as_ref().map(|_| RustupHome::of_kernel());
        let mut checks = BTreeMap::new();
        let mut spans = Vec::new();
        // Under [warm] every check, started or not, has one group naming it, holding its span
        // and its cache observations, so a reader never has to guess which check an
        // observation belongs to.
        let mut warm_evidence = Vec::new();
        // Local checks first, in name order; a remote check never executes candidate code on
        // this machine.
        for name in local {
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
            let runner = CheckRunner::new(cas, sandbox.root())
                .with_cancellation(cancellation)
                .with_timeout(Duration::from_millis(remaining))
                .with_env("HOME", runtime.path().display().to_string())
                .with_env(
                    "XDG_CACHE_HOME",
                    runtime.path().join("cache").display().to_string(),
                );
            let mut runner = rustup
                .iter()
                .flat_map(RustupHome::environment)
                .fold(runner, |runner, (key, value)| runner.with_env(key, value));
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
            let mut prepared = match session.as_mut() {
                Some(session) if remaining > 0 => Some(session.prepare(
                    cas,
                    runner.local_environment(),
                    sandbox.root(),
                    runtime.path(),
                    cancellation,
                    Duration::from_millis(remaining),
                )?),
                // The deadline ran out before this check could prepare: it keeps its name and
                // one observation per declared kind.
                Some(session) => Some(PreparedCheck {
                    key_lock: None,
                    environment: Vec::new(),
                    directories: Vec::new(),
                    observations: session.skipped(cas, DEADLINE_EXHAUSTED)?,
                    refusal: None,
                }),
                None => None,
            };
            // Preparation — the toolchain probe, the lock wait, a Cache Snapshot copy — spent
            // Attempt time. The check runs against what is left now, not what was left before.
            let prepared_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64;
            let exhausted_before = remaining == 0;
            let remaining = attempt
                .reservation()
                .deadline_unix_ms
                .saturating_sub(prepared_at)
                .min(remaining);
            let runner = runner.with_timeout(Duration::from_millis(remaining));
            let exhausted_preparing = prepared.is_some() && !exhausted_before && remaining == 0;
            let runner = match &prepared {
                None => runner.with_env(
                    "CARGO_TARGET_DIR",
                    runtime.path().join("target").display().to_string(),
                ),
                Some(prepared) => prepared
                    .environment
                    .iter()
                    .fold(runner, |runner, (key, value)| {
                        runner.with_env(key.clone(), value.clone())
                    }),
            };
            let refusal = prepared.as_ref().and_then(|p| p.refusal.clone());
            let started = remaining > 0 && refusal.is_none();
            let mut exceeded: Option<Excess> = None;
            let (mut result, timing) = if started {
                let execution = match (
                    session.as_ref(),
                    prepared.as_ref().and_then(|p| {
                        p.key_lock
                            .as_ref()
                            .map(|key| (key, p.directories.as_slice()))
                    }),
                ) {
                    // Whenever the key is held — even when every declared kind is superseded and
                    // nothing is bound — the check runs monitored, so the key is measured during
                    // and after it and judged before the check is accepted.
                    (Some(session), Some((key, directories))) => {
                        let (execution, over) = session.run_monitored(
                            runner,
                            definition,
                            key,
                            directories,
                            cancellation,
                        );
                        exceeded = over;
                        execution
                    }
                    _ => runner.run_observed(definition),
                };
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
                        reason: Some(refusal.unwrap_or_else(|| {
                            if exhausted_preparing {
                                "Task check deadline expired during warm preparation".into()
                            } else {
                                "Task check deadline expired".into()
                            }
                        })),
                        program: Some(definition.command.program.clone()),
                        args: definition.command.args.clone(),
                        stdout: None,
                        stderr: None,
                        required: definition.required,
                        remote: None,
                    },
                    None,
                )
            };
            let mut observations = Vec::new();
            if let (Some(session), Some(prepared)) = (session.as_ref(), prepared.take()) {
                observations = prepared.observations;
                if !started {
                    // Nothing prepared was used; the locks are released without a removal.
                    drop(prepared.directories);
                    if exhausted_preparing {
                        observations =
                            session.not_started(cas, observations, DEADLINE_EXHAUSTED)?;
                    }
                } else {
                    // `fails` is the reason the check fails with, when the excess ends it: the
                    // hard bound or suspicion. Above only `max_bytes` the directories are
                    // evicted and the check's own result stands (ADR-0135).
                    let (fails, why, bound) = match &exceeded {
                        Some(Excess::Suspect(detail)) => {
                            eprintln!(
                                "warm check cache diagnostic: suspect after the check: {detail}"
                            );
                            (Some(WARM_CACHE_SUSPECT), "suspect", None)
                        }
                        Some(Excess::Evict(_)) => (
                            None,
                            "bound_exceeded",
                            Some(review_core::task::runtime::TaskCacheBoundV1::MaxBytes),
                        ),
                        Some(Excess::Bound(_)) => (
                            Some(WARM_CACHE_BOUND_EXCEEDED),
                            "bound_exceeded",
                            Some(review_core::task::runtime::TaskCacheBoundV1::HardMaxBytes),
                        ),
                        None => (None, "bound_exceeded", None),
                    };
                    // Every kind below the key goes, held by this check or left by another; the
                    // key stays locked until the last one is removed.
                    let excess = exceeded.is_some();
                    let evicted =
                        session.finish(prepared.key_lock, prepared.directories, exceeded)?;
                    if let Some(reason) = fails {
                        // Above the hard bound or suspect once the check ended, however fast it
                        // was and whether or not anything was left to remove — a check that
                        // deleted its own warm root is suspect too: the check fails.
                        result.status = CheckStatus::Failed;
                        result.exit_code = None;
                        result.reason = Some(reason.into());
                    }
                    if excess {
                        // Each declared kind's eviction lands on the record that measured it
                        // before the check, with the bound that acted.
                        let base_of =
                            |kind: &str| kind.split(':').next().unwrap_or_default().to_string();
                        // Every declared observation of this check — a warm kind, superseded or
                        // not, and a Cache Snapshot — carries the eviction and its cause; a kind
                        // that had no entry below the key records zero bytes.
                        for observation in observations.iter_mut() {
                            let base = base_of(&observation.kind);
                            let bytes = evicted
                                .iter()
                                .find(|(kind, _)| *kind == base)
                                .map_or(0, |(_, bytes)| *bytes);
                            observation.evicted_bytes = Some(bytes);
                            observation.evicted_reason = Some(why.into());
                            observation.bound = bound;
                        }
                        for (kind, bytes) in &evicted {
                            if !observations.iter().any(|o| base_of(&o.kind) == *kind) {
                                // An entry this check never declared has no observation to carry
                                // its removal; the key's business, logged only.
                                eprintln!(
                                    "warm check cache diagnostic: removed undeclared `{kind}` ({bytes} bytes)"
                                );
                            }
                        }
                    }
                }
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
            let span = match timing {
                Some((started_unix_ms, elapsed_ms)) => {
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
                    Some(TaskRuntimeSpanV1 {
                        span_id,
                        kind: TaskRuntimeSpanKindV1::Check,
                        label: name.clone(),
                        started_unix_ms,
                        elapsed_ms,
                    })
                }
                None => None,
            };
            let sealed = sandbox.seal().map_err(|e| e.to_string())?;
            if !sealed.unchanged() {
                result.status = CheckStatus::Failed;
                result.reason = Some("Check mutated its input Snapshot".into());
            }
            match &rustup {
                Some(rustup) => {
                    let caches = observations
                        .into_iter()
                        .map(|observation| {
                            observation
                                .record(cas, [attempt.task_id(), attempt.id(), &input.node, name])
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let check = TaskRuntimeCheckV1 {
                        name: name.clone(),
                        outcome: match result.status {
                            CheckStatus::Passed => TaskRuntimeCheckOutcomeV1::Passed,
                            CheckStatus::Failed => TaskRuntimeCheckOutcomeV1::Failed,
                            CheckStatus::NotRun => TaskRuntimeCheckOutcomeV1::NotRun,
                        },
                        rustup_home: rustup.source,
                    };
                    warm_evidence.push((check, span, caches));
                }
                None => spans.extend(span),
            }
            if definition.required && result.status != CheckStatus::Passed {
                local_failed.push(name.clone());
            }
            let id = cas
                .put_json(&serde_json::to_value(result).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            checks.insert(name.clone(), id);
        }
        let mut remote_evidence = Vec::new();
        if let Some((target, selected, owner)) = &remote {
            let (source_id, source_manifest) = root_snapshot(cas, &snapshot_id, &snapshot)?;
            let requests: Vec<RemoteCheckRequest> = selected
                .iter()
                .map(|name| {
                    let definition = self
                        .policy
                        .checks
                        .get(name)
                        .ok_or("Named check is not captured")?;
                    Ok(RemoteCheckRequest {
                        name: name.clone(),
                        declaration: definition
                            .remote
                            .clone()
                            .ok_or("Remote check lost its remote table")?,
                    })
                })
                .collect::<Result<_, String>>()?;
            let outcomes = if local_failed.is_empty() {
                // One clock for the whole remote phase: it ends at the earlier of the per-check
                // wall on this timer and the Attempt's deadline.
                let started = std::time::Instant::now();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_millis() as u64;
                let remaining = attempt.reservation().deadline_unix_ms.saturating_sub(now);
                let limit = self
                    .policy
                    .check_process_wall_ms
                    .map_or(remaining, |limit| limit.min(remaining));
                github_pr::run(
                    &RemotePhase {
                        cas,
                        task_id: attempt.task_id(),
                        owner,
                        candidate_id: &snapshot_id,
                        candidate: &manifest,
                        source_id: &source_id,
                        source: &source_manifest,
                        target,
                        mapping: self.remote.mapping.as_deref(),
                        checks: &requests,
                        deadline: started + Duration::from_millis(limit),
                        cancellation,
                    },
                    &self.remote.github_pr,
                )?
            } else {
                let base = super::remote_check::EvidenceBase {
                    github: &target.github,
                    snapshot_id: &snapshot_id,
                    source_snapshot_id: &source_id,
                };
                let failed = local_failed
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                requests
                    .iter()
                    .map(|request| {
                        base.refused(
                            request,
                            RemoteCheckReasonV1::RemoteSkippedLocalFailed,
                            format!(
                                "remote check `{}` was not dispatched because required local \
                                 check(s) {failed} did not pass; fix the local failure and run \
                                 the checks again",
                                request.name
                            ),
                            None,
                        )
                    })
                    .collect::<Vec<RemoteCheckOutcome>>()
            };
            if outcomes.len() != requests.len()
                || outcomes
                    .iter()
                    .zip(&requests)
                    .any(|(o, r)| o.name != r.name)
            {
                return Err("The remote executor did not conclude every remote check".into());
            }
            for outcome in outcomes {
                outcome.evidence.validate()?;
                let definition = self
                    .policy
                    .checks
                    .get(&outcome.name)
                    .ok_or("Named check is not captured")?;
                let evidence_id = cas
                    .put_artifact(
                        REMOTE_CHECK_EVIDENCE_V1,
                        invocation_producer(cas, input, Some(attempt))?,
                        vec![snapshot_id.clone(), source_id.clone()],
                        Some(snapshot_id.clone()),
                        serde_json::to_value(&outcome.evidence).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?
                    .0;
                let (status, reason) = outcome.result(definition);
                // The log excerpt of the jobs that did not succeed is the result's `stdout`,
                // where a local failure's output is.
                let log = match (&outcome.log, status) {
                    (Some(log), CheckStatus::Failed | CheckStatus::NotRun) => {
                        Some(cas.put(log).map_err(|e| e.to_string())?)
                    }
                    _ => None,
                };
                let result =
                    CheckResult::remote(definition, status, reason, evidence_id.clone(), log);
                if let (Some(session), Some(rustup)) = (session.as_ref(), &rustup) {
                    // Under [warm] a remote check keeps its evidence group: every declared kind
                    // skipped for the reason `remote`, and no span — it ran nothing here.
                    let caches = session
                        .skipped(cas, REMOTE_SKIP)?
                        .into_iter()
                        .map(|observation| {
                            observation.record(
                                cas,
                                [attempt.task_id(), attempt.id(), &input.node, &outcome.name],
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    warm_evidence.push((
                        TaskRuntimeCheckV1 {
                            name: outcome.name.clone(),
                            outcome: match status {
                                CheckStatus::Passed => TaskRuntimeCheckOutcomeV1::Passed,
                                CheckStatus::Failed => TaskRuntimeCheckOutcomeV1::Failed,
                                CheckStatus::NotRun => TaskRuntimeCheckOutcomeV1::NotRun,
                            },
                            rustup_home: rustup.source,
                        },
                        None,
                        caches,
                    ));
                }
                let id = cas
                    .put_json(&serde_json::to_value(result).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                checks.insert(outcome.name.clone(), id);
                remote_evidence.push(evidence_id);
            }
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
            .chain(remote_evidence)
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
        let evidence = |check, spans: Vec<TaskRuntimeSpanV1>, caches| TaskRuntimeEvidenceV1 {
            task_id: attempt.task_id().into(),
            attempt_id: attempt.id().into(),
            node: input.node.clone(),
            context_id: attempt.context_id().into(),
            check,
            spans,
            caches,
        };
        let groups = if session.is_some() {
            warm_evidence
                .into_iter()
                .map(|(check, span, caches)| {
                    evidence(Some(check), span.into_iter().collect(), caches)
                })
                .collect::<Vec<_>>()
        } else if local.is_empty() && !remote_names.is_empty() {
            // A node whose checks all ran remotely ran nothing on this machine: there is no
            // span to group, and its remote evidence is already in the receipt (ADR-0140).
            Vec::new()
        } else {
            vec![evidence(None, spans, vec![])]
        };
        if groups.is_empty() && remote_names.is_empty() {
            evidence(None, vec![], vec![]).validate()?;
        }
        let mut evidence_ids = Vec::with_capacity(groups.len());
        for evidence in groups {
            evidence.validate()?;
            let evidence_id = cas
                .put_artifact(
                    TASK_RUNTIME_EVIDENCE_V1,
                    invocation_producer(cas, input, Some(attempt))?,
                    std::iter::once(attempt.context_id().to_owned())
                        .chain(evidence.spans.iter().map(|span| span.span_id.clone()))
                        .chain(evidence.caches.iter().flat_map(|cache| {
                            [cache.observation_id.clone(), cache.source_digest.clone()]
                        }))
                        .collect(),
                    Some(snapshot_id.clone()),
                    serde_json::to_value(evidence).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
                .0;
            evidence_ids.push(evidence_id);
        }
        Ok((
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: TASK_CHECK_RECEIPT_V1.into(),
                cardinality: PortCardinality::One,
                snapshot_id: Some(snapshot_id),
            },
            evidence_ids,
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
            TaskOperatorV1::Check {
                checks,
                remote_checks,
            } => {
                let (receipt, evidence_ids) = self.checks(
                    cas,
                    input,
                    attempt.ok_or("Check has no started Attempt")?,
                    checks,
                    remote_checks,
                    cancellation,
                )?;
                raw_artifact_ids.extend(evidence_ids);
                Ok(BTreeMap::from([("result".into(), receipt)]))
            }
            TaskOperatorV1::Accept {} => self.accept(cas, input),
            TaskOperatorV1::Measure { measures } => self.measure(
                cas,
                input,
                attempt.ok_or("Measure has no started Attempt")?,
                measures,
                cancellation,
            ),
            TaskOperatorV1::Compare { objective } => self.compare(cas, input, objective),
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
            unknown_usage_cause: None,
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
            TaskOperatorV1::Check {
                checks,
                remote_checks,
            } => {
                let value = &output.outputs["result"];
                let receipt: TaskCheckReceiptV1 =
                    serde_json::from_value(envelope(cas, &value.artifact_ids[0])?.payload)
                        .map_err(|e| e.to_string())?;
                if !receipt
                    .checks
                    .keys()
                    .eq(checks.union(remote_checks).collect::<BTreeSet<_>>())
                    || receipt.plan_id != input.plan_id
                    || receipt.snapshot_id != source_input(cas, &input.inputs["source"])?
                    || receipt.outcome != self.check_outcome(cas, &receipt)?
                {
                    return Err("Check receipt changed its invocation or results".into());
                }
                // Each check ran where its node said: a `checks` result is local, a
                // `remote_checks` result names its evidence (ADR-0140).
                for (name, id) in &receipt.checks {
                    let result: CheckResult =
                        serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())?;
                    if result.remote.is_some() != remote_checks.contains(name) {
                        return Err("Check receipt ran a check where its node did not say".into());
                    }
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
            TaskOperatorV1::Measure { measures } => {
                let source = source_input(cas, &input.inputs["source"])?;
                if !output.outputs.keys().eq(measures.iter()) {
                    return Err("Measure output names other measures".into());
                }
                for (name, value) in &output.outputs {
                    let [id] = value.artifact_ids.as_slice() else {
                        return Err("Measure output is not one Measurement".into());
                    };
                    let measurement = self.measurement(cas, input, id, Some(name))?;
                    if measurement.snapshot_id != source
                        || value.snapshot_id.as_ref() != Some(&source)
                    {
                        return Err("Measurement names another Snapshot".into());
                    }
                }
                Ok(())
            }
            TaskOperatorV1::Compare { objective } => {
                if self.compare(cas, input, objective)? != output.outputs {
                    return Err("Comparison differs from its exact deterministic fold".into());
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
