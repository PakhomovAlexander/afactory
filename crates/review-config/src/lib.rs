//! The pipeline definition: a review that is configured rather than constructed.
//!
//! Until now a pipeline existed only as Rust. That is fine for proving properties and useless
//! for a project that wants to describe its own review, so this is the file format — the
//! `.af/` shape the design's project layout describes.
//!
//! **Format note.** The design's examples are YAML; this is TOML. The shape is unchanged — nodes,
//! typed ports, edges, gated_by, checks, convergence policy — and the loader is a set of serde
//! types, so another syntax is a different `from_str` rather than a different model. The reason
//! is maintenance: `serde_yaml` is archived, its forks are uneven, and a config parser is exactly
//! the wrong place to take a dependency risk. `toml` is the ecosystem default for Rust tooling
//! configuration. Recorded rather than quietly done.
//!
//! Every struct denies unknown fields. A typo in a pipeline must be an error, not a setting that
//! silently does nothing — the same rule the contracts use, for the same reason.

pub mod captured_review;
pub mod lock;
pub mod pipeline_edit;
pub mod task;

use std::collections::BTreeMap;

use review_check::CheckDefinition;
use review_core::{Arg, Command, Provenance};
use review_graph::{
    Dispatch, Node, NodeKind, Pipeline, PlanError, Planned, Port, PortCardinality, PortContract,
    RunReport, Scheduler, SnapshotAffinity,
};
use review_store::ConvergencePolicy;
use serde::{Deserialize, Serialize};

pub type InputBinding = (String, String, NodeKind);
pub type InputBindings = BTreeMap<String, BTreeMap<String, Vec<InputBinding>>>;

#[derive(Debug)]
pub enum ConfigError {
    Parse(String),
    Plan(PlanError),
    /// A reviewer node with no runner bound, or a runner bound to no node.
    Binding(String),
    UnknownVersion(u32),
    /// Package resolution failed — not locked, tampered, absent, or malformed.
    Lock(lock::LockError),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Parse(e) => write!(f, "pipeline definition: {e}"),
            ConfigError::Plan(e) => write!(f, "pipeline definition: {e}"),
            ConfigError::Binding(e) => write!(f, "pipeline definition: {e}"),
            ConfigError::UnknownVersion(v) => write!(
                f,
                "pipeline definition: unsupported version {v}; this kernel understands versions 1 through 5"
            ),
            ConfigError::Lock(e) => write!(f, "pipeline definition: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgSpec {
    pub value: String,
    /// Defaults to `literal`, because a project writing its own check command is trusted. A
    /// value derived from the change under review must say so explicitly — the safe default is
    /// the one that cannot be reached by forgetting.
    #[serde(default = "default_provenance")]
    pub provenance: ProvenanceSpec,
}

fn default_provenance() -> ProvenanceSpec {
    ProvenanceSpec::Literal
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceSpec {
    Literal,
    Untrusted,
}

impl From<ProvenanceSpec> for Provenance {
    fn from(spec: ProvenanceSpec) -> Provenance {
        match spec {
            ProvenanceSpec::Literal => Provenance::Literal,
            ProvenanceSpec::Untrusted => Provenance::Untrusted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<ArgSpec>,
}

impl CommandSpec {
    pub fn build(&self) -> Command {
        Command::new(
            &self.program,
            self.args
                .iter()
                .map(|a| Arg {
                    value: a.value.clone(),
                    provenance: a.provenance.into(),
                })
                .collect(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub name: String,
    #[serde(flatten)]
    pub command: CommandSpec,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKindSpec {
    Generation,
    Gate,
    Reviewer,
    Slicer,
    Scatter,
    Gather,
    Ledger,
}

impl From<NodeKindSpec> for NodeKind {
    fn from(spec: NodeKindSpec) -> NodeKind {
        match spec {
            NodeKindSpec::Generation => NodeKind::Generation,
            NodeKindSpec::Gate => NodeKind::Gate,
            NodeKindSpec::Reviewer => NodeKind::Reviewer,
            NodeKindSpec::Slicer => NodeKind::Slicer,
            NodeKindSpec::Scatter => NodeKind::Scatter,
            NodeKindSpec::Gather => NodeKind::Gather,
            NodeKindSpec::Ledger => NodeKind::Ledger,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    pub id: String,
    pub kind: NodeKindSpec,
    /// Pipeline-owned classification for benchmark Demands emitted by this reviewer. `None`
    /// permanently retains the pre-M4 required default for pinned authority created before the
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demands: Option<review_core::DemandRequirement>,
    #[serde(default)]
    pub inputs: Vec<PortContractSpec>,
    #[serde(default = "default_outputs")]
    pub outputs: Vec<PortContractSpec>,
    #[serde(default)]
    pub gated_by: Option<String>,
    /// An inline runner command. A reviewer binds exactly one of `runner` or `package`;
    /// meaningless on any other kind of node.
    #[serde(default)]
    pub runner: Option<CommandSpec>,
    /// A reviewer package from the registries, pinned in `review.lock`. The runner command
    /// then comes from the package's digest-verified manifest.
    #[serde(default)]
    pub package: Option<String>,
    /// Required for every reviewer in pipeline v4. Earlier formats permanently retain their
    /// pre-M6.3 credential behavior and cannot claim this binding retroactively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ReviewerExecutionSpec>,
    /// Required only on a pipeline-v5 Slicer. The outer graph stays static; this policy fixes
    /// the exact Scatter owner and every dynamic bound before the Round starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slicing: Option<SlicingSpec>,
    /// Marks a whole-Subject reviewer as the closeout for exactly one Scatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closeout_for: Option<String>,
    /// This Worker's own Attempt cap: the reservation its dispatch takes instead of the
    /// pipeline-wide `[budgets].attempt`. `None` keeps the shared cap, so every pipeline written
    /// before the field existed behaves exactly as it did. Refines `[budgets]`; requires it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<NodeBudgetSpec>,
    /// This reviewer's warm-layer policy. `None` is cold: no Notes are requested, carried or
    /// rendered, and every pipeline written before the field existed behaves exactly as it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm: Option<WarmSpec>,
}

/// A Worker node's own Attempt cap, in the pipeline's budget unit. On a Scatter it is each
/// shard's reservation; the Scatter's `fan_out` still bounds the total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeBudgetSpec {
    pub attempt: u64,
}

/// A reviewer node's warm-layer policy (package P1: Notes and Head Delta; package P2: the
/// Gate's build cache). Every layer is a declared CAS artifact in the Attempt's context
/// manifest, never ambient state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmSpec {
    /// Ask each admitted Attempt for Worker Notes and carry them, with Delta Marking against
    /// the previous head, to the next Round's Attempt of this same node. On by default once a
    /// node opts into warm layers; `notes = false` keeps such a node cold.
    #[serde(default = "default_true")]
    pub notes: bool,
    /// Byte bound for one encoded `WorkerNotes@1`. Larger notes are dropped with a recorded
    /// reason and the Attempt is still admitted.
    #[serde(default = "default_notes_max_bytes")]
    pub notes_max_bytes: u64,
    /// Build cache kinds this node's sandboxes receive from the Round's Gate, cloned from the
    /// explicitly unsafe `BuildCache@1` the Gate captured. Every kind must be declared by a
    /// `trusted_local` Gate Execution Binding; a safe pipeline refuses the handoff at load.
    #[serde(default, skip_serializing_if = "BuildCacheKindsSpec::is_empty")]
    pub build_cache: BuildCacheKindsSpec,
}

fn default_notes_max_bytes() -> u64 {
    review_core::DEFAULT_WORKER_NOTES_BYTES as u64
}

impl WarmSpec {
    fn validate(&self, node: &str, gate: Option<&GateExecutionSpec>) -> Result<(), ConfigError> {
        let maximum = review_core::MAX_WORKER_NOTES_BYTES as u64;
        if self.notes_max_bytes == 0 || self.notes_max_bytes > maximum {
            return Err(ConfigError::Binding(format!(
                "reviewer `{node}` warm.notes_max_bytes must be between 1 and {maximum}"
            )));
        }
        for kind in self.build_cache.kinds() {
            let declared = gate.is_some_and(|gate| gate.build_caches.contains(&kind));
            if !declared {
                return Err(ConfigError::Binding(format!(
                    "reviewer `{node}` warm.build_cache names `{}` but no `trusted_local` `[gate]` Execution Binding declares it in `build_caches`; a candidate-built cache is carried only from a Gate that declares the kind",
                    kind.as_str()
                )));
            }
        }
        Ok(())
    }
}

/// The closed vocabulary of build cache kinds a Gate may capture and a reviewer may receive.
/// `cargo_target` points `CARGO_TARGET_DIR` at the sandbox-local clone. The registry-only
/// `cargo` Cache Snapshot under `[gate] caches` keeps its existing meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildCacheKindSpec {
    CargoTarget,
}

impl BuildCacheKindSpec {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CargoTarget => "cargo_target",
        }
    }

    /// The kernel vocabulary this pipeline spelling names.
    pub const fn kind(self) -> review_core::BuildCacheKindV1 {
        match self {
            Self::CargoTarget => review_core::BuildCacheKindV1::CargoTarget,
        }
    }
}

/// The set of build cache kinds one reviewer node declares, written as a TOML array such as
/// `build_cache = ["cargo_target"]`. The vocabulary is closed, so the set is a fixed shape
/// rather than a list: duplicates and unknown kinds are refused at parse time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildCacheKindsSpec {
    cargo_target: bool,
}

impl BuildCacheKindsSpec {
    pub fn is_empty(&self) -> bool {
        !self.cargo_target
    }

    pub fn contains(&self, kind: BuildCacheKindSpec) -> bool {
        match kind {
            BuildCacheKindSpec::CargoTarget => self.cargo_target,
        }
    }

    /// The declared kinds in vocabulary order.
    pub fn kinds(&self) -> Vec<BuildCacheKindSpec> {
        self.cargo_target
            .then_some(BuildCacheKindSpec::CargoTarget)
            .into_iter()
            .collect()
    }

    pub fn from_kinds(kinds: &[BuildCacheKindSpec]) -> Result<Self, String> {
        let mut set = Self::default();
        for kind in kinds {
            if set.contains(*kind) {
                return Err(format!(
                    "build cache kind `{}` is declared more than once",
                    kind.as_str()
                ));
            }
            match kind {
                BuildCacheKindSpec::CargoTarget => set.cargo_target = true,
            }
        }
        Ok(set)
    }
}

impl Serialize for BuildCacheKindsSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.kinds().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BuildCacheKindsSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kinds = Vec::<BuildCacheKindSpec>::deserialize(deserializer)?;
        Self::from_kinds(&kinds).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseoutModeSpec {
    Required,
    Waived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlicingSpec {
    pub scatter: String,
    pub max_paths_per_slice: usize,
    pub max_fanout: u32,
    #[serde(default = "complete_coverage")]
    pub coverage: review_core::SliceCoverageV1,
    #[serde(default = "default_true")]
    pub all_shards_required: bool,
    #[serde(default = "required_closeout")]
    pub closeout: CloseoutModeSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiver_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiver_reason: Option<String>,
}

fn complete_coverage() -> review_core::SliceCoverageV1 {
    review_core::SliceCoverageV1::Complete
}

fn required_closeout() -> CloseoutModeSpec {
    CloseoutModeSpec::Required
}

impl SlicingSpec {
    pub fn closeout_policy(&self) -> Result<review_core::CloseoutPolicyV1, ConfigError> {
        match self.closeout {
            CloseoutModeSpec::Required => {
                if self.waiver_policy_id.is_some() || self.waiver_reason.is_some() {
                    return Err(ConfigError::Binding(
                        "required closeout cannot carry waiver authority".into(),
                    ));
                }
                Ok(review_core::CloseoutPolicyV1::Required)
            }
            CloseoutModeSpec::Waived => {
                let policy_id = self.waiver_policy_id.clone().ok_or_else(|| {
                    ConfigError::Binding("waived closeout requires waiver_policy_id".into())
                })?;
                let reason = self
                    .waiver_reason
                    .clone()
                    .filter(|reason| !reason.trim().is_empty())
                    .ok_or_else(|| {
                        ConfigError::Binding("waived closeout requires waiver_reason".into())
                    })?;
                let digest = policy_id.strip_prefix("sha256:").unwrap_or_default();
                if digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(ConfigError::Binding(
                        "waiver_policy_id must be a sha256 artifact ID from Authority Snapshot policy".into(),
                    ));
                }
                Ok(review_core::CloseoutPolicyV1::Waived { policy_id, reason })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerExecutionSpec {
    pub credential_mode: review_core::BrokerCredentialModeV1,
    #[serde(default)]
    pub auto_apply: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<review_core::BrokerOperationPolicyV1>,
}

impl ReviewerExecutionSpec {
    fn validate(&self, node: &str) -> Result<(), ConfigError> {
        let mut names = std::collections::BTreeSet::new();
        for operation in &self.operations {
            operation.validate().map_err(|error| {
                ConfigError::Binding(format!(
                    "reviewer `{node}` has an invalid Broker operation: {error}"
                ))
            })?;
            if !names.insert(operation.name.as_str()) {
                return Err(ConfigError::Binding(format!(
                    "reviewer `{node}` has duplicate Broker operation `{}`",
                    operation.name
                )));
            }
        }
        review_core::broker_authority_usage(&self.operations).map_err(|error| {
            ConfigError::Binding(format!(
                "reviewer `{node}` has invalid aggregate Broker authority: {error}"
            ))
        })?;
        match self.credential_mode {
            review_core::BrokerCredentialModeV1::Brokered if !self.operations.is_empty() => {}
            review_core::BrokerCredentialModeV1::CredentialFree
            | review_core::BrokerCredentialModeV1::TrustedUnsafe
                if self.operations.is_empty() => {}
            review_core::BrokerCredentialModeV1::Brokered => {
                return Err(ConfigError::Binding(format!(
                    "brokered reviewer `{node}` must declare at least one bounded operation"
                )));
            }
            _ => {
                return Err(ConfigError::Binding(format!(
                    "reviewer `{node}` declares Broker operations without brokered credentials"
                )));
            }
        }
        if self.auto_apply
            && self.credential_mode == review_core::BrokerCredentialModeV1::TrustedUnsafe
        {
            return Err(ConfigError::Binding(format!(
                "trusted_unsafe reviewer `{node}` cannot authorize auto_apply"
            )));
        }
        Ok(())
    }
}

fn default_outputs() -> Vec<PortContractSpec> {
    vec![PortContractSpec::Name("out".to_string())]
}

fn validate_diff_change_set_wiring(
    nodes: &[NodeSpec],
    edges: &[EdgeSpec],
) -> Result<(), ConfigError> {
    let exact = |port: &PortContract| {
        port.artifact_type == review_core::contract::CHANGE_SET_V1
            && port.cardinality == review_core::PortCardinality::One
            && !port.optional
            && port.snapshot_affinity == review_core::SnapshotAffinity::SameSubject
    };
    let producers: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind == NodeKindSpec::Generation)
        .flat_map(|node| {
            node.outputs
                .iter()
                .map(PortContractSpec::build)
                .filter(|port| port.artifact_type == review_core::contract::CHANGE_SET_V1)
                .map(move |port| (node, port))
        })
        .collect();
    let [(producer, producer_port)] = producers.as_slice() else {
        return Err(ConfigError::Binding(
            "a `diff` pipeline must have exactly one typed ChangeSet@1 producer".into(),
        ));
    };
    if !exact(producer_port) {
        return Err(ConfigError::Binding(
            "a `diff` pipeline's ChangeSet@1 producer must be required, singular, and bound to the subject snapshot"
                .into(),
        ));
    }
    for reviewer in nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKindSpec::Reviewer | NodeKindSpec::Scatter))
    {
        let inputs: Vec<_> = reviewer
            .inputs
            .iter()
            .map(PortContractSpec::build)
            .filter(|port| port.artifact_type == review_core::contract::CHANGE_SET_V1)
            .collect();
        let [reviewer_port] = inputs.as_slice() else {
            return Err(ConfigError::Binding(format!(
                "diff reviewer `{}` must declare exactly one ChangeSet@1 input",
                reviewer.id
            )));
        };
        if !exact(reviewer_port)
            || !edges.iter().any(|edge| {
                edge.from.node == producer.id
                    && edge.from.port == producer_port.name
                    && edge.to.node == reviewer.id
                    && edge.to.port == reviewer_port.name
            })
        {
            return Err(ConfigError::Binding(format!(
                "diff reviewer `{}` must receive generation's exact ChangeSet@1 through its typed input",
                reviewer.id
            )));
        }
    }
    Ok(())
}

fn validate_generation_output_contracts(
    nodes: &[NodeSpec],
    subject: review_core::SubjectKind,
    version: u32,
) -> Result<(), ConfigError> {
    if version == 1 {
        return Ok(());
    }
    let mut change_sets = 0_usize;
    for node in nodes
        .iter()
        .filter(|node| node.kind == NodeKindSpec::Generation)
    {
        let mut prior_findings = 0_usize;
        for port in node.outputs.iter().map(PortContractSpec::build) {
            match port.artifact_type.as_str() {
                review_core::contract::PRIOR_FINDINGS_V1 => prior_findings += 1,
                review_core::contract::FINDING_SET_V1 => {
                    if port.cardinality != review_core::PortCardinality::One
                        || !port.optional
                        || port.snapshot_affinity != review_core::SnapshotAffinity::Any
                    {
                        return Err(ConfigError::Binding(format!(
                            "generation node `{}` exact FindingSet@1 output `{}` must be optional, singular, and snapshot-affinity `any`",
                            node.id, port.name
                        )));
                    }
                    prior_findings += 1;
                }
                review_core::contract::CHANGE_SET_V1 => {
                    if subject != review_core::SubjectKind::Diff {
                        return Err(ConfigError::Binding(format!(
                            "generation node `{}` output `{}` declares `{}`, which only a `diff` Subject can supply",
                            node.id,
                            port.name,
                            review_core::contract::CHANGE_SET_V1,
                        )));
                    }
                    change_sets += 1;
                }
                artifact_type => {
                    return Err(ConfigError::Binding(format!(
                        "generation node `{}` output `{}` has unsupported type `{artifact_type}`; pipeline version 2 requires Generation outputs to use a typed port declaration for `{}`, `{}`, or `{}`",
                        node.id,
                        port.name,
                        review_core::contract::PRIOR_FINDINGS_V1,
                        review_core::contract::FINDING_SET_V1,
                        review_core::contract::CHANGE_SET_V1,
                    )));
                }
            }
        }
        if prior_findings == 0 {
            return Err(ConfigError::Binding(format!(
                "generation node `{}` must emit an explicit `{}` compatibility view or exact `{}` output",
                node.id,
                review_core::contract::PRIOR_FINDINGS_V1,
                review_core::contract::FINDING_SET_V1,
            )));
        }
    }
    if subject == review_core::SubjectKind::Diff && change_sets != 1 {
        return Err(ConfigError::Binding(format!(
            "a `diff` pipeline must declare exactly one generation `{}` output",
            review_core::contract::CHANGE_SET_V1,
        )));
    }
    Ok(())
}

fn validate_disposition_wiring(nodes: &[NodeSpec], edges: &[EdgeSpec]) -> Result<(), ConfigError> {
    let finding_set_outputs: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind == NodeKindSpec::Generation)
        .flat_map(|node| {
            node.outputs
                .iter()
                .map(PortContractSpec::build)
                .filter(|port| port.artifact_type == review_core::contract::FINDING_SET_V1)
                .map(move |port| (node, port))
        })
        .collect();
    for reviewer in nodes
        .iter()
        .filter(|node| node.kind == NodeKindSpec::Reviewer)
    {
        let uses_v2 = reviewer
            .outputs
            .iter()
            .map(PortContractSpec::build)
            .any(|port| port.artifact_type == review_core::contract::REVIEWER_RESULT_V2);
        if !uses_v2 {
            continue;
        }
        let inputs: Vec<_> = reviewer
            .inputs
            .iter()
            .map(PortContractSpec::build)
            .filter(|port| port.artifact_type == review_core::contract::FINDING_SET_V1)
            .collect();
        let [input] = inputs.as_slice() else {
            return Err(ConfigError::Binding(format!(
                "ReviewerResult@2 reviewer `{}` must declare exactly one FindingSet@1 input",
                reviewer.id
            )));
        };
        if input.cardinality != review_core::PortCardinality::One
            || !input.optional
            || input.snapshot_affinity != review_core::SnapshotAffinity::Any
        {
            return Err(ConfigError::Binding(format!(
                "ReviewerResult@2 reviewer `{}` FindingSet@1 input must be optional, singular, and snapshot-affinity `any`",
                reviewer.id
            )));
        }
        if !finding_set_outputs.iter().any(|(generation, output)| {
            edges.iter().any(|edge| {
                edge.from.node == generation.id
                    && edge.from.port == output.name
                    && edge.to.node == reviewer.id
                    && edge.to.port == input.name
            })
        }) {
            return Err(ConfigError::Binding(format!(
                "ReviewerResult@2 reviewer `{}` must receive generation's exact FindingSet@1",
                reviewer.id
            )));
        }
    }
    Ok(())
}

fn validate_dynamic_wiring(
    version: u32,
    nodes: &[NodeSpec],
    edges: &[EdgeSpec],
) -> Result<(), ConfigError> {
    let uses_dynamic = nodes.iter().any(|node| {
        matches!(node.kind, NodeKindSpec::Slicer | NodeKindSpec::Scatter)
            || node.slicing.is_some()
            || node.closeout_for.is_some()
    });
    if version != 5 {
        if uses_dynamic {
            return Err(ConfigError::Binding(
                "Slicer, Scatter, slicing, and closeout_for require pipeline format version 5"
                    .into(),
            ));
        }
        return Ok(());
    }

    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    for node in nodes {
        if node.closeout_for.is_some() && node.kind != NodeKindSpec::Reviewer {
            return Err(ConfigError::Binding(format!(
                "node `{}` marks closeout_for but is not a whole-Subject reviewer",
                node.id
            )));
        }
        if node.kind != NodeKindSpec::Slicer && node.slicing.is_some() {
            return Err(ConfigError::Binding(format!(
                "node `{}` declares slicing but is not a Slicer",
                node.id
            )));
        }
        if node.kind != NodeKindSpec::Slicer {
            continue;
        }
        let policy = node.slicing.as_ref().ok_or_else(|| {
            ConfigError::Binding(format!("Slicer `{}` has no slicing policy", node.id))
        })?;
        if policy.max_paths_per_slice == 0 || policy.max_fanout == 0 {
            return Err(ConfigError::Binding(format!(
                "Slicer `{}` has a zero path or fan-out bound",
                node.id
            )));
        }
        let closeout = policy.closeout_policy()?;
        if policy.coverage == review_core::SliceCoverageV1::Complete && !policy.all_shards_required
        {
            return Err(ConfigError::Binding(format!(
                "complete Slicer `{}` cannot make shards optional",
                node.id
            )));
        }
        let scatter = by_id.get(policy.scatter.as_str()).ok_or_else(|| {
            ConfigError::Binding(format!(
                "Slicer `{}` names absent Scatter `{}`",
                node.id, policy.scatter
            ))
        })?;
        if scatter.kind != NodeKindSpec::Scatter {
            return Err(ConfigError::Binding(format!(
                "Slicer `{}` target `{}` is not a Scatter",
                node.id, policy.scatter
            )));
        }
        let slice_outputs: Vec<_> = node
            .outputs
            .iter()
            .map(PortContractSpec::build)
            .filter(|port| port.artifact_type == review_core::contract::SLICE_SET_V1)
            .collect();
        let slice_inputs: Vec<_> = scatter
            .inputs
            .iter()
            .map(PortContractSpec::build)
            .filter(|port| port.artifact_type == review_core::contract::SLICE_SET_V1)
            .collect();
        let ([slice_output], [slice_input]) = (slice_outputs.as_slice(), slice_inputs.as_slice())
        else {
            return Err(ConfigError::Binding(format!(
                "Slicer `{}` and Scatter `{}` must expose exactly one SliceSet@1 route",
                node.id, scatter.id
            )));
        };
        if slice_output.cardinality != PortCardinality::One
            || slice_output.optional
            || slice_output.snapshot_affinity != SnapshotAffinity::SameSubject
            || slice_input.cardinality != PortCardinality::One
            || slice_input.optional
            || slice_input.snapshot_affinity != SnapshotAffinity::SameSubject
            || !edges.iter().any(|edge| {
                edge.from.node == node.id
                    && edge.from.port == slice_output.name
                    && edge.to.node == scatter.id
                    && edge.to.port == slice_input.name
            })
        {
            return Err(ConfigError::Binding(format!(
                "Slicer `{}` must durably feed its required same-Subject SliceSet@1 to Scatter `{}`",
                node.id, scatter.id
            )));
        }
        let shard_outputs: Vec<_> = scatter
            .outputs
            .iter()
            .map(PortContractSpec::build)
            .filter(|port| port.artifact_type == review_core::contract::SHARD_SET_V1)
            .collect();
        let [shard_output] = shard_outputs.as_slice() else {
            return Err(ConfigError::Binding(format!(
                "Scatter `{}` must expose exactly one ShardSet@1 output",
                scatter.id
            )));
        };
        let ledger_route = nodes.iter().any(|candidate| {
            if candidate.kind != NodeKindSpec::Ledger {
                return false;
            }
            candidate
                .inputs
                .iter()
                .map(PortContractSpec::build)
                .filter(|port| port.artifact_type == review_core::contract::SHARD_SET_V1)
                .any(|input| {
                    input.cardinality == PortCardinality::One
                        && !input.optional
                        && edges.iter().any(|edge| {
                            edge.from.node == scatter.id
                                && edge.from.port == shard_output.name
                                && edge.to.node == candidate.id
                                && edge.to.port == input.name
                        })
                })
        });
        if !ledger_route {
            return Err(ConfigError::Binding(format!(
                "Scatter `{}` must route its lossless ShardSet@1 directly to a Ledger",
                scatter.id
            )));
        }
        let closeouts: Vec<_> = nodes
            .iter()
            .filter(|candidate| candidate.closeout_for.as_deref() == Some(scatter.id.as_str()))
            .collect();
        match closeout {
            review_core::CloseoutPolicyV1::Required => {
                let [reviewer] = closeouts.as_slice() else {
                    return Err(ConfigError::Binding(format!(
                        "Scatter `{}` requires exactly one whole-Subject closeout reviewer",
                        scatter.id
                    )));
                };
                let shard_inputs: Vec<_> = reviewer
                    .inputs
                    .iter()
                    .map(PortContractSpec::build)
                    .filter(|port| port.artifact_type == review_core::contract::SHARD_SET_V1)
                    .collect();
                let [shard_input] = shard_inputs.as_slice() else {
                    return Err(ConfigError::Binding(format!(
                        "closeout reviewer `{}` must accept exactly one ShardSet@1",
                        reviewer.id
                    )));
                };
                if shard_output.cardinality != PortCardinality::One
                    || shard_output.optional
                    || shard_input.cardinality != PortCardinality::One
                    || shard_input.optional
                    || !edges.iter().any(|edge| {
                        edge.from.node == scatter.id
                            && edge.from.port == shard_output.name
                            && edge.to.node == reviewer.id
                            && edge.to.port == shard_input.name
                    })
                {
                    return Err(ConfigError::Binding(format!(
                        "Scatter `{}` must feed its lossless ShardSet@1 to closeout reviewer `{}`",
                        scatter.id, reviewer.id
                    )));
                }
            }
            review_core::CloseoutPolicyV1::Waived { .. } if !closeouts.is_empty() => {
                return Err(ConfigError::Binding(format!(
                    "Scatter `{}` has both a closeout waiver and an executing closeout reviewer",
                    scatter.id
                )));
            }
            review_core::CloseoutPolicyV1::Waived { .. } => {}
        }
    }
    Ok(())
}

/// A port declaration. The string arm keeps v1 pipeline files readable and expands to an
/// explicit opaque/one/required/any contract. It remains valid for non-Generation nodes;
/// built-in Generation outputs require the typed arm because execution dispatches by contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PortContractSpec {
    Name(String),
    Typed(TypedPortSpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedPortSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub cardinality: PortCardinality,
    #[serde(default)]
    pub optional: bool,
    pub snapshot_affinity: SnapshotAffinity,
}

impl PortContractSpec {
    fn build(&self) -> PortContract {
        match self {
            Self::Name(name) => PortContract::opaque(name),
            Self::Typed(port) => {
                let contract = PortContract::new(&port.name, &port.artifact_type)
                    .with_cardinality(port.cardinality)
                    .with_snapshot_affinity(port.snapshot_affinity);
                if port.optional {
                    contract.optional()
                } else {
                    contract
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortSpec {
    pub node: String,
    pub port: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeSpec {
    pub from: PortSpec,
    pub to: PortSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvergenceSpec {
    #[serde(default = "one")]
    pub clean_rounds: u32,
    #[serde(default = "three")]
    pub max_rounds: u32,
    #[serde(default = "major")]
    pub gate: SeveritySpec,
}

fn one() -> u32 {
    1
}
fn three() -> u32 {
    3
}
fn major() -> SeveritySpec {
    SeveritySpec::Major
}

impl Default for ConvergenceSpec {
    fn default() -> Self {
        ConvergenceSpec {
            clean_rounds: 1,
            max_rounds: 3,
            gate: SeveritySpec::Major,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeveritySpec {
    Blocker,
    Major,
    Minor,
}

impl From<SeveritySpec> for review_core::Severity {
    fn from(spec: SeveritySpec) -> review_core::Severity {
        match spec {
            SeveritySpec::Blocker => review_core::Severity::Blocker,
            SeveritySpec::Major => review_core::Severity::Major,
            SeveritySpec::Minor => review_core::Severity::Minor,
        }
    }
}

/// The budget section: the owner's spend policy, in the pipeline definition where it is
/// versioned and reviewed. Absent means uncapped — budgets are a thing a pipeline declares,
/// not a default it inherits invisibly.
///
/// Owner decision, updated 2026-08-25: tokens are the unit; the shipped heavy policy reserves
/// 300k per attempt and caps a run at 1M. A fenced attempt still charges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSpec {
    /// Explicit so the file records what the numbers mean. Only `tokens` exists.
    pub unit: BudgetUnit,
    /// Cap per attempt — also the amount reserved before each dispatch.
    pub attempt: u64,
    /// Cap per run, across every attempt including fenced ones.
    pub run: u64,
    /// Aggregate cap for every dynamic shard owned by one Scatter. Absent retains the run cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fan_out: Option<u64>,
}

/// Captured opt-in policy for internal automatic Integration. Proposal nomination alone never
/// enables this path; the reviewer Execution Binding must independently grant `auto_apply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationSpec {
    #[serde(default)]
    pub protected_paths: Vec<String>,
    pub post_apply_checks: Vec<String>,
    #[serde(default)]
    pub reviewer_priority: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetUnit {
    Tokens,
}

/// The immutable Subject shape this pipeline is defined to review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectSpec {
    pub kind: review_core::SubjectKind,
}

/// The machine-local provider selected by a Gate Execution Binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProviderSpec {
    TrustedLocal,
    Container,
}

/// The weakest isolation a pipeline is willing to accept for its Gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationSpec {
    None,
    Container,
}

/// M6.1 deliberately exposes one Gate mode. Adding another is a policy change, not a free-form
/// string that a misspelled configuration may silently downgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateModeSpec {
    EphemeralWrite,
}

/// A cache request names behavior the kernel understands, never a host path or arbitrary
/// environment variable. New package managers add a closed variant with their own safe target
/// and offline controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheKindSpec {
    Cargo,
}

fn is_pinned_container_image(image: &str) -> bool {
    let Some((name, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    !name.is_empty()
        && !name.starts_with('-')
        && !name.chars().any(char::is_whitespace)
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The Gate slice of an Execution Binding. Reviewer bindings remain unchanged until their own
/// milestone; this format makes the Gate's provider, isolation requirement, and write policy
/// explicit and pins them inside the captured pipeline artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateExecutionSpec {
    pub provider: SandboxProviderSpec,
    pub required_isolation: IsolationSpec,
    pub mode: GateModeSpec,
    /// Required for the container provider and forbidden for trusted-local execution. The
    /// digest pin is execution authority; a moving tag could otherwise change the Gate without
    /// changing the captured pipeline artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Symbolic cache kinds resolved only through machine-local administrator policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caches: Vec<CacheKindSpec>,
    /// Build cache kinds this Gate captures after its checks pass, as explicitly unsafe
    /// `BuildCache@1` artifacts for reviewer nodes that declare the same kind. Candidate code
    /// produces these bytes, so the declaration is admitted only under `trusted_local`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_caches: Vec<BuildCacheKindSpec>,
    /// Byte bound one build cache capture applies; the kernel default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_cache_max_bytes: Option<u64>,
    /// Filesystem-entry bound (directories included) one build cache capture applies; the
    /// kernel default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_cache_max_entries: Option<u64>,
}

impl GateExecutionSpec {
    /// The limits every build cache capture of this Gate applies: declared bounds over the
    /// kernel defaults, with the fixed depth and path bounds of the closed layout.
    pub fn build_cache_limits(&self) -> review_core::BuildCacheLimitsV1 {
        let defaults = review_core::BuildCacheLimitsV1::default_v1();
        review_core::BuildCacheLimitsV1 {
            max_bytes: self.build_cache_max_bytes.unwrap_or(defaults.max_bytes),
            max_entries: self.build_cache_max_entries.unwrap_or(defaults.max_entries),
            ..defaults
        }
    }
}

/// A whole pipeline definition, as a project writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub version: u32,
    #[serde(default)]
    pub subject: Option<SubjectSpec>,
    #[serde(default)]
    pub checks: Vec<CheckSpec>,
    /// One pinned wall-clock bound for every gate check. The generous default is resolved by
    /// the authority layer and persisted in CampaignManifest@1.
    #[serde(default)]
    pub check_timeout_seconds: Option<u64>,
    /// Required by pipeline format v3. Formats v1/v2 permanently retain their legacy local,
    /// read-only Gate behavior so pinned Campaign replay does not acquire new execution policy.
    #[serde(default)]
    pub gate: Option<GateExecutionSpec>,
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub edges: Vec<EdgeSpec>,
    #[serde(default)]
    pub convergence: ConvergenceSpec,
    #[serde(default)]
    pub budgets: Option<BudgetSpec>,
    #[serde(default)]
    pub integration: Option<IntegrationSpec>,
}

/// A validated definition: the plan, the checks, and the reviewer bindings.
pub struct Loaded {
    version: u32,
    subject: SubjectSpec,
    plan: Planned,
    checks: Vec<CheckDefinition>,
    check_timeout_seconds: u64,
    gate: Option<GateExecutionSpec>,
    reviewers: BTreeMap<String, Command>,
    demand_requirements: BTreeMap<String, review_core::DemandRequirement>,
    /// Package-backed reviewers, by node: name, exact version, digest, verified root. What a
    /// run manifest records so replay can prove which reviewer bytes were used.
    packages: BTreeMap<String, std::sync::Arc<lock::ResolvedReviewer>>,
    reviewer_execution: BTreeMap<String, ReviewerExecutionSpec>,
    slicing: BTreeMap<String, SlicingSpec>,
    closeouts: BTreeMap<String, String>,
    convergence: ConvergencePolicy,
    budgets: Option<BudgetSpec>,
    /// Worker nodes that declared their own Attempt cap. Absent nodes reserve `[budgets].attempt`.
    node_attempt_caps: BTreeMap<String, u64>,
    integration: Option<IntegrationSpec>,
    /// Reviewer nodes that declared a warm-layer policy. Absent nodes run cold.
    warm: BTreeMap<String, WarmSpec>,
}

/// A dispatcher that declares the Subject semantics it actually executes.
pub trait SubjectDispatch: Dispatch + Sync {
    fn subject_kind(&self) -> review_core::SubjectKind;

    fn reviewer_credential_mode(&self, _node: &str) -> Option<review_core::BrokerCredentialModeV1> {
        None
    }

    fn broker_provider_available(&self, _node: &str) -> bool {
        false
    }
}

impl Loaded {
    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn subject_kind(&self) -> review_core::SubjectKind {
        self.subject.kind
    }

    pub fn checks(&self) -> &[CheckDefinition] {
        &self.checks
    }

    pub fn check_timeout_seconds(&self) -> u64 {
        self.check_timeout_seconds
    }

    pub fn gate_execution(&self) -> Option<&GateExecutionSpec> {
        self.gate.as_ref()
    }

    pub fn reviewers(&self) -> &BTreeMap<String, Command> {
        &self.reviewers
    }

    pub fn demand_requirements(&self) -> &BTreeMap<String, review_core::DemandRequirement> {
        &self.demand_requirements
    }

    pub fn packages(&self) -> &BTreeMap<String, std::sync::Arc<lock::ResolvedReviewer>> {
        &self.packages
    }

    pub fn reviewer_execution(&self) -> &BTreeMap<String, ReviewerExecutionSpec> {
        &self.reviewer_execution
    }

    pub fn slicing(&self) -> &BTreeMap<String, SlicingSpec> {
        &self.slicing
    }

    pub fn closeouts(&self) -> &BTreeMap<String, String> {
        &self.closeouts
    }

    pub fn convergence(&self) -> &ConvergencePolicy {
        &self.convergence
    }

    pub fn budgets(&self) -> Option<&BudgetSpec> {
        self.budgets.as_ref()
    }

    /// Worker nodes that declared their own Attempt cap, by node ID.
    pub fn node_attempt_caps(&self) -> &BTreeMap<String, u64> {
        &self.node_attempt_caps
    }

    /// The reservation one Attempt of `node` takes: its own cap, else the pipeline's. `None`
    /// when the pipeline is uncapped.
    pub fn attempt_cap_for(&self, node: &str) -> Option<u64> {
        self.node_attempt_caps
            .get(node)
            .copied()
            .or_else(|| self.budgets.as_ref().map(|budgets| budgets.attempt))
    }

    pub fn integration(&self) -> Option<&IntegrationSpec> {
        self.integration.as_ref()
    }

    /// Reviewer nodes with a declared warm-layer policy, by node ID.
    pub fn warm_policies(&self) -> &BTreeMap<String, WarmSpec> {
        &self.warm
    }

    pub fn plan_order(&self) -> &[String] {
        &self.plan.order
    }

    /// Borrow the validated topology for installed compatibility compilation. This does not
    /// dispatch it, expose mutable authority, or bypass captured package/manifest admission.
    pub fn planned(&self) -> &review_graph::Planned {
        &self.plan
    }

    pub fn node_is_gated(&self, node: &str) -> bool {
        !self.plan.gates_for(node).is_empty()
    }

    pub fn node_receives_port(&self, node: &str, port: &str) -> bool {
        self.plan
            .dependencies_of(node)
            .iter()
            .any(|edge| edge.to.name == port)
    }

    pub fn node_kind_has_output_type(&self, kind: NodeKind, artifact_type: &str) -> bool {
        self.plan.nodes.values().any(|node| {
            node.kind == kind
                && node
                    .outputs
                    .iter()
                    .any(|output| output.artifact_type == artifact_type)
        })
    }

    /// Exact upstream node ids for every input port, captured from the validated graph.
    pub fn input_sources(&self) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
        let mut sources: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
        for edge in &self.plan.edges {
            sources
                .entry(edge.to.node.clone())
                .or_default()
                .entry(edge.to.name.clone())
                .or_default()
                .push(edge.from.node.clone());
        }
        for ports in sources.values_mut() {
            for nodes in ports.values_mut() {
                nodes.sort();
            }
        }
        sources
    }

    /// Exact upstream node, output port, and kind for every input port. Canonical reducers use
    /// this validated provenance instead of guessing an artifact's producer from its contents.
    pub fn input_bindings(&self) -> InputBindings {
        let mut bindings = InputBindings::new();
        for edge in &self.plan.edges {
            let kind = self.plan.nodes[&edge.from.node].kind;
            bindings
                .entry(edge.to.node.clone())
                .or_default()
                .entry(edge.to.name.clone())
                .or_default()
                .push((edge.from.node.clone(), edge.from.name.clone(), kind));
        }
        for ports in bindings.values_mut() {
            for sources in ports.values_mut() {
                sources.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
            }
        }
        bindings
    }

    /// Schedule only through a dispatcher whose execution semantics match this definition.
    pub fn run(&self, dispatcher: &impl SubjectDispatch) -> Result<RunReport, ConfigError> {
        if dispatcher.subject_kind() != self.subject.kind {
            return Err(ConfigError::Binding(format!(
                "pipeline declares `{}` but its dispatcher executes `{}`",
                self.subject.kind,
                dispatcher.subject_kind()
            )));
        }
        for (node, expected) in &self.reviewer_execution {
            let actual = dispatcher.reviewer_credential_mode(node).ok_or_else(|| {
                ConfigError::Binding(format!(
                    "reviewer `{node}` has no runtime adapter for its v4 Execution Binding"
                ))
            })?;
            if actual != expected.credential_mode {
                return Err(ConfigError::Binding(format!(
                    "reviewer `{node}` requires {:?} credentials but its runtime adapter is {:?}",
                    expected.credential_mode, actual
                )));
            }
            if expected.credential_mode == review_core::BrokerCredentialModeV1::Brokered
                && !dispatcher.broker_provider_available(node)
            {
                return Err(ConfigError::Binding(format!(
                    "brokered reviewer `{node}` has no machine-local Broker provider"
                )));
            }
        }
        Ok(Scheduler::new(&self.plan).run(dispatcher))
    }
}

impl Definition {
    pub fn from_toml(text: &str) -> Result<Definition, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// Validate everything: the schema, the graph, and the bindings.
    ///
    /// All of it before anything runs, and all of it fatal. A pipeline that is 90% valid is not
    /// 90% of a review. A definition that names packages cannot load this way — resolving one
    /// requires the lockfile, and [`load_with`](Self::load_with) is how it is provided.
    pub fn load(self) -> Result<Loaded, ConfigError> {
        self.load_inner(None)
    }

    /// [`load`](Self::load), with package resolution: every `package = "name"` reviewer is
    /// located in the registries, digest-verified against the lockfile, and bound to the
    /// runner its verified manifest declares.
    pub fn load_with(
        self,
        lockfile: &lock::Lockfile,
        registry: &lock::Registry,
    ) -> Result<Loaded, ConfigError> {
        self.load_inner(Some((lockfile, registry)))
    }

    fn load_inner(
        self,
        resolver: Option<(&lock::Lockfile, &lock::Registry)>,
    ) -> Result<Loaded, ConfigError> {
        let integration = self.integration.clone();
        let (subject, gate) = match (self.version, self.subject, self.gate) {
            (1, None, None) => (
                SubjectSpec {
                    kind: review_core::SubjectKind::WholeTree,
                },
                None,
            ),
            (1, Some(_), _) => {
                return Err(ConfigError::Binding(
                    "pipeline format version 1 has no `[subject]`; use version 2 to declare it"
                        .to_string(),
                ));
            }
            (1 | 2, _, Some(_)) => {
                return Err(ConfigError::Binding(
                    "pipeline formats 1 and 2 have no `[gate]` Execution Binding; use version 3"
                        .to_string(),
                ));
            }
            (2, Some(subject), None) => (subject, None),
            (2..=5, None, _) => {
                return Err(ConfigError::Binding(format!(
                    "pipeline format version {} requires `[subject]`",
                    self.version
                )));
            }
            (3..=5, Some(_), None) => {
                return Err(ConfigError::Binding(format!(
                    "pipeline format version {} requires an explicit `[gate]` Execution Binding",
                    self.version
                )));
            }
            (3..=5, Some(subject), Some(gate)) => (subject, Some(gate)),
            (version, _, _) => return Err(ConfigError::UnknownVersion(version)),
        };
        if let Some(binding) = &gate {
            let unique_caches: std::collections::BTreeSet<_> =
                binding.caches.iter().copied().collect();
            if unique_caches.len() != binding.caches.len() {
                return Err(ConfigError::Binding(
                    "Gate cache kinds must be unique".to_string(),
                ));
            }
            let unique_build_caches: std::collections::BTreeSet<_> =
                binding.build_caches.iter().copied().collect();
            if unique_build_caches.len() != binding.build_caches.len() {
                return Err(ConfigError::Binding(
                    "Gate build cache kinds must be unique".to_string(),
                ));
            }
            if !binding.build_caches.is_empty()
                && (binding.provider != SandboxProviderSpec::TrustedLocal
                    || binding.required_isolation != IsolationSpec::None)
            {
                return Err(ConfigError::Binding(
                    "`[gate] build_caches` is refused under the safe policy: a candidate-built cache carries no administrator approval and is admitted only from a `trusted_local` Gate with `required_isolation = \"none\"`"
                        .to_string(),
                ));
            }
            if binding.build_cache_max_bytes.is_some() || binding.build_cache_max_entries.is_some()
            {
                if binding.build_caches.is_empty() {
                    return Err(ConfigError::Binding(
                        "`[gate]` build cache limits require `build_caches`".to_string(),
                    ));
                }
                binding
                    .build_cache_limits()
                    .validate()
                    .map_err(|error| ConfigError::Binding(format!("`[gate]` {error}")))?;
            }
            match binding.provider {
                SandboxProviderSpec::TrustedLocal => {
                    if binding.required_isolation != IsolationSpec::None {
                        return Err(ConfigError::Binding(
                            "`trusted_local` can satisfy only explicit `required_isolation = \"none\"`"
                                .to_string(),
                        ));
                    }
                    if binding.image.is_some() {
                        return Err(ConfigError::Binding(
                            "`trusted_local` Gate bindings cannot declare a container image"
                                .to_string(),
                        ));
                    }
                }
                SandboxProviderSpec::Container => {
                    let image = binding.image.as_deref().ok_or_else(|| {
                        ConfigError::Binding(
                            "container Gate bindings require an `image` pinned by sha256 digest"
                                .to_string(),
                        )
                    })?;
                    if !is_pinned_container_image(image) {
                        return Err(ConfigError::Binding(format!(
                            "container Gate image `{image}` must be a non-option OCI reference pinned as `name@sha256:<64 lowercase hex>`"
                        )));
                    }
                }
            }
        }
        if gate.is_some() {
            let gates: Vec<_> = self
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKindSpec::Gate)
                .collect();
            if gates.is_empty()
                || gates
                    .iter()
                    .any(|node| !node.inputs.is_empty() || node.gated_by.is_some())
            {
                return Err(ConfigError::Binding(
                    "pipeline format version 3 requires every Gate node to be a root so every Gate Execution Binding is resolved"
                        .to_string(),
                ));
            }
        }
        validate_generation_output_contracts(&self.nodes, subject.kind, self.version)?;
        validate_disposition_wiring(&self.nodes, &self.edges)?;
        validate_dynamic_wiring(self.version, &self.nodes, &self.edges)?;
        if subject.kind == review_core::SubjectKind::Diff {
            validate_diff_change_set_wiring(&self.nodes, &self.edges)?;
        }
        if let Some(budgets) = &self.budgets {
            if budgets.attempt == 0 || budgets.run == 0 || budgets.fan_out == Some(0) {
                return Err(ConfigError::Binding(
                    "a zero-token budget cap means nothing can ever dispatch; omit [budgets] to run uncapped"
                        .to_string(),
                ));
            }
            if budgets.attempt > budgets.run {
                return Err(ConfigError::Binding(format!(
                    "the attempt cap ({}) exceeds the run cap ({}); no attempt could ever reserve",
                    budgets.attempt, budgets.run
                )));
            }
            if budgets
                .fan_out
                .is_some_and(|limit| limit < budgets.attempt || limit > budgets.run)
            {
                return Err(ConfigError::Binding(
                    "the fan_out cap must cover one attempt and cannot exceed the run cap".into(),
                ));
            }
        }
        for node in &self.nodes {
            let Some(node_budget) = node.budget else {
                continue;
            };
            if !matches!(node.kind, NodeKindSpec::Reviewer | NodeKindSpec::Scatter) {
                return Err(ConfigError::Binding(format!(
                    "node `{}` is not a Worker but declares an Attempt cap",
                    node.id
                )));
            }
            let Some(budgets) = &self.budgets else {
                return Err(ConfigError::Binding(format!(
                    "node `{}` declares an Attempt cap but the pipeline has no [budgets]; a node cap refines the pipeline caps, it cannot replace them",
                    node.id
                )));
            };
            if node_budget.attempt == 0 {
                return Err(ConfigError::Binding(format!(
                    "node `{}` declares a zero-token Attempt cap, so it could never dispatch",
                    node.id
                )));
            }
            if node_budget.attempt > budgets.run {
                return Err(ConfigError::Binding(format!(
                    "node `{}` Attempt cap ({}) exceeds the run cap ({}); it could never reserve",
                    node.id, node_budget.attempt, budgets.run
                )));
            }
            if matches!(node.kind, NodeKindSpec::Scatter)
                && budgets
                    .fan_out
                    .is_some_and(|fan_out| node_budget.attempt > fan_out)
            {
                return Err(ConfigError::Binding(format!(
                    "Scatter `{}` per-shard Attempt cap ({}) exceeds its fan_out cap",
                    node.id, node_budget.attempt
                )));
            }
        }
        if self.convergence.clean_rounds == 0 || self.convergence.max_rounds == 0 {
            return Err(ConfigError::Binding(
                "convergence round counts must be positive".to_string(),
            ));
        }
        if self.convergence.clean_rounds > self.convergence.max_rounds {
            return Err(ConfigError::Binding(format!(
                "convergence requires {} clean rounds but permits only {} rounds",
                self.convergence.clean_rounds, self.convergence.max_rounds
            )));
        }
        if let Some(policy) = &integration {
            if self.version != 5 {
                return Err(ConfigError::Binding(
                    "automatic Integration requires pipeline format version 5".into(),
                ));
            }
            if self.convergence.max_rounds < 2 {
                return Err(ConfigError::Binding(
                    "automatic Integration requires at least two Rounds for later verification"
                        .into(),
                ));
            }
            if self.nodes.iter().all(|node| node.slicing.is_none()) {
                return Err(ConfigError::Binding(
                    "automatic Integration requires a captured Slicer/Scatter semantic-closure route"
                        .into(),
                ));
            }
            if policy.post_apply_checks.is_empty() {
                return Err(ConfigError::Binding(
                    "automatic Integration requires at least one post-apply check".into(),
                ));
            }
            let check_names: std::collections::BTreeSet<_> = self
                .checks
                .iter()
                .map(|check| check.name.as_str())
                .collect();
            let mut selected_checks = std::collections::BTreeSet::new();
            if policy.post_apply_checks.iter().any(|name| {
                !check_names.contains(name.as_str()) || !selected_checks.insert(name.as_str())
            }) {
                return Err(ConfigError::Binding(
                    "Integration post_apply_checks must be unique declared checks".into(),
                ));
            }
            let node_ids: std::collections::BTreeSet<_> =
                self.nodes.iter().map(|node| node.id.as_str()).collect();
            let mut priorities = std::collections::BTreeSet::new();
            if policy
                .reviewer_priority
                .iter()
                .any(|node| !node_ids.contains(node.as_str()) || !priorities.insert(node.as_str()))
            {
                return Err(ConfigError::Binding(
                    "Integration reviewer_priority must name unique pipeline nodes".into(),
                ));
            }
            let mut protected = std::collections::BTreeSet::new();
            if policy.protected_paths.iter().any(|path| {
                !review_core::is_valid_repo_path(path) || !protected.insert(path.as_str())
            }) {
                return Err(ConfigError::Binding(
                    "Integration protected_paths must be unique canonical repository paths".into(),
                ));
            }
        }

        let mut pipeline = Pipeline::default();
        let mut reviewers = BTreeMap::new();
        let mut demand_requirements = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut reviewer_execution = BTreeMap::new();
        let mut slicing = BTreeMap::new();
        let mut closeouts = BTreeMap::new();
        let mut warm = BTreeMap::new();
        let mut resolved_packages: BTreeMap<String, std::sync::Arc<lock::ResolvedReviewer>> =
            BTreeMap::new();
        for spec in &self.nodes {
            let reviewer_like = matches!(spec.kind, NodeKindSpec::Reviewer | NodeKindSpec::Scatter);
            if let Some(policy) = &spec.warm {
                if !reviewer_like {
                    return Err(ConfigError::Binding(format!(
                        "node `{}` is not a reviewer but declares a warm-layer policy",
                        spec.id
                    )));
                }
                policy.validate(&spec.id, gate.as_ref())?;
                warm.insert(spec.id.clone(), *policy);
            }
            if reviewer_like {
                demand_requirements.insert(
                    spec.id.clone(),
                    spec.demands
                        .unwrap_or(review_core::DemandRequirement::Required),
                );
                match (self.version, &spec.execution) {
                    (4 | 5, Some(execution)) => {
                        execution.validate(&spec.id)?;
                        if let Some(budgets) = &self.budgets {
                            let authority =
                                review_core::broker_authority_usage(&execution.operations)
                                    .expect("validated Broker authority");
                            let attempt_cap =
                                spec.budget.map_or(budgets.attempt, |budget| budget.attempt);
                            if authority > attempt_cap {
                                return Err(ConfigError::Binding(format!(
                                    "brokered reviewer `{}` aggregate Broker authority ({authority}) exceeds its attempt cap ({attempt_cap}); the dispatch reservation would not cover its capability",
                                    spec.id
                                )));
                            }
                        }
                        reviewer_execution.insert(spec.id.clone(), execution.clone());
                    }
                    (4 | 5, None) => {
                        return Err(ConfigError::Binding(format!(
                            "pipeline format version {} requires reviewer-capable node `{}` to declare an Execution Binding",
                            self.version, spec.id
                        )));
                    }
                    (_, Some(_)) => {
                        return Err(ConfigError::Binding(format!(
                            "reviewer `{}` cannot declare an Execution Binding before pipeline version 4",
                            spec.id
                        )));
                    }
                    (_, None) => {}
                }
            } else if spec.demands.is_some() {
                return Err(ConfigError::Binding(format!(
                    "node `{}` is not a reviewer but classifies reviewer Demands",
                    spec.id
                )));
            } else if spec.execution.is_some() {
                return Err(ConfigError::Binding(format!(
                    "node `{}` is not a reviewer but declares a reviewer Execution Binding",
                    spec.id
                )));
            }
            if let Some(policy) = &spec.slicing {
                slicing.insert(spec.id.clone(), policy.clone());
            }
            if let Some(scatter) = &spec.closeout_for {
                closeouts.insert(scatter.clone(), spec.id.clone());
            }
            let mut node = Node::new(&spec.id, spec.kind.into())
                .accepting_contracts(spec.inputs.iter().map(PortContractSpec::build).collect())
                .emitting_contracts(spec.outputs.iter().map(PortContractSpec::build).collect());
            if let Some(gate) = &spec.gated_by {
                node = node.gated_by(gate);
            }
            pipeline = pipeline.node(node);

            match (spec.kind, &spec.runner, &spec.package) {
                (NodeKindSpec::Reviewer | NodeKindSpec::Scatter, None, None) => {
                    return Err(ConfigError::Binding(format!(
                        "reviewer node `{}` binds neither a runner nor a package; a reviewer \
                         with nothing to run would be a node that always reports nothing",
                        spec.id
                    )));
                }
                (NodeKindSpec::Reviewer | NodeKindSpec::Scatter, Some(_), Some(_)) => {
                    return Err(ConfigError::Binding(format!(
                        "reviewer node `{}` binds both a runner and a package; exactly one \
                         must say what runs",
                        spec.id
                    )));
                }
                (NodeKindSpec::Reviewer | NodeKindSpec::Scatter, Some(command), None) => {
                    if subject.kind == review_core::SubjectKind::Diff {
                        return Err(ConfigError::Binding(format!(
                            "reviewer node `{}` uses an inline runner, which has no package \
                             manifest declaring `diff` Subject support",
                            spec.id
                        )));
                    }
                    reviewers.insert(spec.id.clone(), command.build());
                }
                (NodeKindSpec::Reviewer | NodeKindSpec::Scatter, None, Some(package)) => {
                    let Some((lockfile, registry)) = resolver else {
                        return Err(ConfigError::Binding(format!(
                            "reviewer node `{}` names package `{package}`, which needs the \
                             lockfile; load this definition with load_with",
                            spec.id
                        )));
                    };
                    let resolved = match resolved_packages.get(package) {
                        Some(resolved) => std::sync::Arc::clone(resolved),
                        None => {
                            let resolved = std::sync::Arc::new(
                                lockfile
                                    .resolve_for_subject(package, registry, subject.kind)
                                    .map_err(ConfigError::Lock)?,
                            );
                            resolved_packages
                                .insert(package.clone(), std::sync::Arc::clone(&resolved));
                            resolved
                        }
                    };
                    reviewers.insert(spec.id.clone(), resolved.runner.clone());
                    packages.insert(spec.id.clone(), resolved);
                }
                (_, Some(_), _) | (_, _, Some(_)) => {
                    return Err(ConfigError::Binding(format!(
                        "node `{}` is not a reviewer but binds a runner or package",
                        spec.id
                    )));
                }
                (_, None, None) => {}
            }
        }

        for edge in &self.edges {
            pipeline = pipeline.edge(
                Port::new(&edge.from.node, &edge.from.port),
                Port::new(&edge.to.node, &edge.to.port),
            );
        }

        if self.nodes.is_empty() {
            return Err(ConfigError::Binding(
                "pipeline defines no nodes; an empty review cannot produce a valid round"
                    .to_string(),
            ));
        }
        if reviewers.is_empty() {
            return Err(ConfigError::Binding(
                "pipeline defines no reviewer; a review with no reviewer cannot produce claims"
                    .to_string(),
            ));
        }

        let check_timeout_seconds = self.check_timeout_seconds.unwrap_or(3600);
        if check_timeout_seconds == 0 {
            return Err(ConfigError::Binding(
                "check_timeout_seconds must be positive".to_string(),
            ));
        }
        let plan = pipeline.plan().map_err(ConfigError::Plan)?;
        let checks = self
            .checks
            .iter()
            .map(|c| {
                let definition = CheckDefinition::new(&c.name, c.command.build());
                if c.required {
                    definition
                } else {
                    definition.optional()
                }
            })
            .collect();

        let node_attempt_caps = self
            .nodes
            .iter()
            .filter_map(|node| node.budget.map(|budget| (node.id.clone(), budget.attempt)))
            .collect();
        Ok(Loaded {
            version: self.version,
            subject,
            plan,
            checks,
            check_timeout_seconds,
            gate,
            reviewers,
            demand_requirements,
            packages,
            reviewer_execution,
            slicing,
            closeouts,
            budgets: self.budgets,
            node_attempt_caps,
            integration,
            warm,
            convergence: ConvergencePolicy {
                clean_rounds: self.convergence.clean_rounds,
                max_rounds: self.convergence.max_rounds,
                gate: self.convergence.gate.into(),
            },
        })
    }
}
