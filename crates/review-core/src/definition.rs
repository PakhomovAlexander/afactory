//! The pipeline definition: the `.af/pipelines/*.toml` shape a project writes, and the exact
//! bytes a Campaign pins as its pipeline authority (the `pipeline` file of
//! `CampaignManifest@1`).
//!
//! It lives here, beneath every executable crate, because two readers must agree on it byte
//! for byte: the loader in `review-config`, which binds it to reviewer packages and plans the
//! graph before a Campaign opens, and the event store, which re-reads the pinned artifact on
//! every replay to judge durable events against the authority they were admitted under. One
//! shape, one parser ([`PipelineDefinition::from_toml`]), one set of pure rules
//! ([`PipelineDefinition::validate`]): a field added here reaches both readers at once, so pinned
//! authority cannot stop replaying because one reader learned a field the other did not.
//!
//! Nothing here touches a filesystem, a lockfile, or the graph planner. Package resolution,
//! graph planning, and check binding belong to the loader; this module only says what a
//! pipeline *is* and which pipelines are refused outright.
//!
//! **Format note.** The design's examples are YAML; this is TOML. The shape is unchanged —
//! nodes, typed ports, edges, gated_by, checks, convergence policy — and the shape is a set of
//! serde types, so another syntax is a different `from_str` rather than a different model. The
//! reason is maintenance: `serde_yaml` is archived, its forks are uneven, and a config parser is
//! exactly the wrong place to take a dependency risk. `toml` is the ecosystem default for Rust
//! tooling configuration. Recorded rather than quietly done.
//!
//! Every struct denies unknown fields. A typo in a pipeline must be an error, not a setting that
//! silently does nothing — the same rule the JSON contracts use, for the same reason.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::broker::{BrokerCredentialModeV1, BrokerOperationPolicyV1, broker_authority_usage};
use crate::contract;
use crate::demand::DemandRequirement;
use crate::event::{PortCardinality, SnapshotAffinity};
use crate::exec::{Arg, Command, Provenance};
use crate::finding::Severity;
use crate::path::is_valid_repo_path;
use crate::slice::{CloseoutPolicyV1, SliceCoverageV1};
use crate::subject::SubjectKind;

/// The pipeline format versions this kernel understands. A version outside this range is
/// refused rather than guessed at, by the loader and by pinned-authority replay alike.
pub const SUPPORTED_VERSIONS: RangeInclusive<u32> = 1..=5;

/// Why a definition is not a pipeline this kernel can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionError {
    /// The text is not the shape: syntax, a missing field, or a field no version declares.
    Parse(String),
    UnknownVersion(u32),
    /// The shape parsed, but the pipeline it describes contradicts a rule.
    Invalid(String),
}

impl fmt::Display for DefinitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DefinitionError::Parse(message) | DefinitionError::Invalid(message) => {
                f.write_str(message)
            }
            DefinitionError::UnknownVersion(version) => write!(
                f,
                "unsupported version {version}; this kernel understands versions {} through {}",
                SUPPORTED_VERSIONS.start(),
                SUPPORTED_VERSIONS.end()
            ),
        }
    }
}

impl std::error::Error for DefinitionError {}

fn invalid(message: impl Into<String>) -> DefinitionError {
    DefinitionError::Invalid(message.into())
}

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

impl NodeKindSpec {
    /// The reviewer-capable kinds: the nodes that dispatch a Worker, and so are the only ones
    /// that bind a runner or package, an Execution Binding, a Demand classification, or their
    /// own Attempt cap.
    pub fn is_worker(self) -> bool {
        matches!(self, NodeKindSpec::Reviewer | NodeKindSpec::Scatter)
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
    pub demands: Option<DemandRequirement>,
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
    /// A reviewer package from the registries, pinned in the lockfile. The runner command
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
}

fn default_outputs() -> Vec<PortContractSpec> {
    vec![PortContractSpec::Name("out".to_string())]
}

/// A Worker node's own Attempt cap, in the pipeline's budget unit. On a Scatter it is each
/// shard's reservation; the Scatter's `fan_out` still bounds the total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeBudgetSpec {
    pub attempt: u64,
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
    pub coverage: SliceCoverageV1,
    #[serde(default = "default_true")]
    pub all_shards_required: bool,
    #[serde(default = "required_closeout")]
    pub closeout: CloseoutModeSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiver_policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiver_reason: Option<String>,
}

fn complete_coverage() -> SliceCoverageV1 {
    SliceCoverageV1::Complete
}

fn required_closeout() -> CloseoutModeSpec {
    CloseoutModeSpec::Required
}

impl SlicingSpec {
    /// The closeout policy this slicing declares, or why the declaration is contradictory.
    pub fn closeout_policy(&self) -> Result<CloseoutPolicyV1, DefinitionError> {
        match self.closeout {
            CloseoutModeSpec::Required => {
                if self.waiver_policy_id.is_some() || self.waiver_reason.is_some() {
                    return Err(invalid("required closeout cannot carry waiver authority"));
                }
                Ok(CloseoutPolicyV1::Required)
            }
            CloseoutModeSpec::Waived => {
                let policy_id = self
                    .waiver_policy_id
                    .clone()
                    .ok_or_else(|| invalid("waived closeout requires waiver_policy_id"))?;
                let reason = self
                    .waiver_reason
                    .clone()
                    .filter(|reason| !reason.trim().is_empty())
                    .ok_or_else(|| invalid("waived closeout requires waiver_reason"))?;
                let digest = policy_id.strip_prefix("sha256:").unwrap_or_default();
                if digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(invalid(
                        "waiver_policy_id must be a sha256 artifact ID from Authority Snapshot policy",
                    ));
                }
                Ok(CloseoutPolicyV1::Waived { policy_id, reason })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerExecutionSpec {
    pub credential_mode: BrokerCredentialModeV1,
    #[serde(default)]
    pub auto_apply: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<BrokerOperationPolicyV1>,
}

impl ReviewerExecutionSpec {
    /// Every operation well-formed and uniquely named, the aggregate authority within bounds,
    /// and the operation set consistent with the credential mode. Returns that aggregate
    /// authority, so a caller can hold it against the Attempt cap.
    fn validate(&self, node: &str) -> Result<u64, DefinitionError> {
        let mut names = BTreeSet::new();
        for operation in &self.operations {
            operation.validate().map_err(|error| {
                invalid(format!(
                    "reviewer `{node}` has an invalid Broker operation: {error}"
                ))
            })?;
            if !names.insert(operation.name.as_str()) {
                return Err(invalid(format!(
                    "reviewer `{node}` has duplicate Broker operation `{}`",
                    operation.name
                )));
            }
        }
        let authority = broker_authority_usage(&self.operations).map_err(|error| {
            invalid(format!(
                "reviewer `{node}` has invalid aggregate Broker authority: {error}"
            ))
        })?;
        match self.credential_mode {
            BrokerCredentialModeV1::Brokered if !self.operations.is_empty() => {}
            BrokerCredentialModeV1::CredentialFree | BrokerCredentialModeV1::TrustedUnsafe
                if self.operations.is_empty() => {}
            BrokerCredentialModeV1::Brokered => {
                return Err(invalid(format!(
                    "brokered reviewer `{node}` must declare at least one bounded operation"
                )));
            }
            _ => {
                return Err(invalid(format!(
                    "reviewer `{node}` declares Broker operations without brokered credentials"
                )));
            }
        }
        if self.auto_apply && self.credential_mode == BrokerCredentialModeV1::TrustedUnsafe {
            return Err(invalid(format!(
                "trusted_unsafe reviewer `{node}` cannot authorize auto_apply"
            )));
        }
        Ok(authority)
    }
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

/// The contract every reader derives from a port declaration. The graph planner, the wiring
/// rules below, and pinned-authority replay all read a port through these, so a name-keyed
/// port means exactly one thing everywhere.
impl PortContractSpec {
    pub fn name(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Typed(port) => &port.name,
        }
    }

    pub fn artifact_type(&self) -> &str {
        match self {
            Self::Name(_) => contract::OPAQUE_V1,
            Self::Typed(port) => &port.artifact_type,
        }
    }

    pub fn cardinality(&self) -> PortCardinality {
        match self {
            Self::Name(_) => PortCardinality::One,
            Self::Typed(port) => port.cardinality,
        }
    }

    pub fn optional(&self) -> bool {
        match self {
            Self::Name(_) => false,
            Self::Typed(port) => port.optional,
        }
    }

    pub fn snapshot_affinity(&self) -> SnapshotAffinity {
        match self {
            Self::Name(_) => SnapshotAffinity::Any,
            Self::Typed(port) => port.snapshot_affinity,
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

impl From<SeveritySpec> for Severity {
    fn from(spec: SeveritySpec) -> Severity {
        match spec {
            SeveritySpec::Blocker => Severity::Blocker,
            SeveritySpec::Major => Severity::Major,
            SeveritySpec::Minor => Severity::Minor,
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
    pub kind: SubjectKind,
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
}

impl GateExecutionSpec {
    fn validate(&self) -> Result<(), DefinitionError> {
        let unique_caches: BTreeSet<_> = self.caches.iter().copied().collect();
        if unique_caches.len() != self.caches.len() {
            return Err(invalid("Gate cache kinds must be unique"));
        }
        match self.provider {
            SandboxProviderSpec::TrustedLocal => {
                if self.required_isolation != IsolationSpec::None {
                    return Err(invalid(
                        "`trusted_local` can satisfy only explicit `required_isolation = \"none\"`",
                    ));
                }
                if self.image.is_some() {
                    return Err(invalid(
                        "`trusted_local` Gate bindings cannot declare a container image",
                    ));
                }
            }
            SandboxProviderSpec::Container => {
                let image = self.image.as_deref().ok_or_else(|| {
                    invalid("container Gate bindings require an `image` pinned by sha256 digest")
                })?;
                if !is_pinned_container_image(image) {
                    return Err(invalid(format!(
                        "container Gate image `{image}` must be a non-option OCI reference pinned as `name@sha256:<64 lowercase hex>`"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A whole pipeline definition, as a project writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineDefinition {
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

impl PipelineDefinition {
    /// Parse the shape. Syntax, a missing field, a wrong type, or a field no version declares
    /// is refused here; [`validate`](Self::validate) judges what parsed.
    pub fn from_toml(text: &str) -> Result<PipelineDefinition, DefinitionError> {
        toml::from_str(text).map_err(|error| DefinitionError::Parse(error.to_string()))
    }

    /// The Subject shape this pipeline reviews. Version 1 declares none and reviews the whole
    /// tree; every later version declares it, and [`validate`](Self::validate) refuses one that
    /// does not.
    pub fn subject_kind(&self) -> SubjectKind {
        self.subject
            .map_or(SubjectKind::WholeTree, |subject| subject.kind)
    }

    /// Every rule that can be judged from the definition alone: the version and what it must
    /// declare, the Gate binding, node identity, port wiring, budgets, convergence, Integration
    /// policy, and each node's bindings. The loader and pinned-authority replay both apply
    /// exactly this before anything else; the loader then adds package resolution and graph
    /// planning, which need the lockfile and the planner.
    ///
    /// All of it before anything runs, and all of it fatal. A pipeline that is 90% valid is not
    /// 90% of a review.
    pub fn validate(&self) -> Result<(), DefinitionError> {
        if !SUPPORTED_VERSIONS.contains(&self.version) {
            return Err(DefinitionError::UnknownVersion(self.version));
        }
        match (self.version, self.subject.is_some(), self.gate.is_some()) {
            (1, true, _) => {
                return Err(invalid(
                    "pipeline format version 1 has no `[subject]`; use version 2 to declare it",
                ));
            }
            (1 | 2, _, true) => {
                return Err(invalid(
                    "pipeline formats 1 and 2 have no `[gate]` Execution Binding; use version 3",
                ));
            }
            (1, false, false) | (2, true, false) => {}
            (_, false, _) => {
                return Err(invalid(format!(
                    "pipeline format version {} requires `[subject]`",
                    self.version
                )));
            }
            (_, true, false) => {
                return Err(invalid(format!(
                    "pipeline format version {} requires an explicit `[gate]` Execution Binding",
                    self.version
                )));
            }
            (_, true, true) => {}
        }
        let subject = self.subject_kind();

        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            if node.id.trim().is_empty() {
                return Err(invalid(
                    "a node has an empty ID; every node is addressed by its ID",
                ));
            }
            if !ids.insert(node.id.as_str()) {
                return Err(invalid(format!(
                    "node `{}` is declared twice; node IDs must be unique",
                    node.id
                )));
            }
        }

        if let Some(binding) = &self.gate {
            binding.validate()?;
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
                return Err(invalid(
                    "pipeline format version 3 requires every Gate node to be a root so every Gate Execution Binding is resolved",
                ));
            }
        }
        validate_generation_output_contracts(&self.nodes, subject, self.version)?;
        validate_disposition_wiring(&self.nodes, &self.edges)?;
        validate_dynamic_wiring(self.version, &self.nodes, &self.edges)?;
        if subject == SubjectKind::Diff {
            validate_diff_change_set_wiring(&self.nodes, &self.edges)?;
        }
        if let Some(budgets) = &self.budgets {
            if budgets.attempt == 0 || budgets.run == 0 || budgets.fan_out == Some(0) {
                return Err(invalid(
                    "a zero-token budget cap means nothing can ever dispatch; omit [budgets] to run uncapped",
                ));
            }
            if budgets.attempt > budgets.run {
                return Err(invalid(format!(
                    "the attempt cap ({}) exceeds the run cap ({}); no attempt could ever reserve",
                    budgets.attempt, budgets.run
                )));
            }
            if budgets
                .fan_out
                .is_some_and(|limit| limit < budgets.attempt || limit > budgets.run)
            {
                return Err(invalid(
                    "the fan_out cap must cover one attempt and cannot exceed the run cap",
                ));
            }
        }
        for node in &self.nodes {
            let Some(node_budget) = node.budget else {
                continue;
            };
            if !node.kind.is_worker() {
                return Err(invalid(format!(
                    "node `{}` is not a Worker but declares an Attempt cap",
                    node.id
                )));
            }
            let Some(budgets) = &self.budgets else {
                return Err(invalid(format!(
                    "node `{}` declares an Attempt cap but the pipeline has no [budgets]; a node cap refines the pipeline caps, it cannot replace them",
                    node.id
                )));
            };
            if node_budget.attempt == 0 {
                return Err(invalid(format!(
                    "node `{}` declares a zero-token Attempt cap, so it could never dispatch",
                    node.id
                )));
            }
            if node_budget.attempt > budgets.run {
                return Err(invalid(format!(
                    "node `{}` Attempt cap ({}) exceeds the run cap ({}); it could never reserve",
                    node.id, node_budget.attempt, budgets.run
                )));
            }
            if node.kind == NodeKindSpec::Scatter
                && budgets
                    .fan_out
                    .is_some_and(|fan_out| node_budget.attempt > fan_out)
            {
                return Err(invalid(format!(
                    "Scatter `{}` per-shard Attempt cap ({}) exceeds its fan_out cap",
                    node.id, node_budget.attempt
                )));
            }
        }
        if self.convergence.clean_rounds == 0 || self.convergence.max_rounds == 0 {
            return Err(invalid("convergence round counts must be positive"));
        }
        if self.convergence.clean_rounds > self.convergence.max_rounds {
            return Err(invalid(format!(
                "convergence requires {} clean rounds but permits only {} rounds",
                self.convergence.clean_rounds, self.convergence.max_rounds
            )));
        }
        if let Some(policy) = &self.integration {
            if self.version != 5 {
                return Err(invalid(
                    "automatic Integration requires pipeline format version 5",
                ));
            }
            if self.convergence.max_rounds < 2 {
                return Err(invalid(
                    "automatic Integration requires at least two Rounds for later verification",
                ));
            }
            if self.nodes.iter().all(|node| node.slicing.is_none()) {
                return Err(invalid(
                    "automatic Integration requires a captured Slicer/Scatter semantic-closure route",
                ));
            }
            if policy.post_apply_checks.is_empty() {
                return Err(invalid(
                    "automatic Integration requires at least one post-apply check",
                ));
            }
            let check_names: BTreeSet<_> = self
                .checks
                .iter()
                .map(|check| check.name.as_str())
                .collect();
            let mut selected_checks = BTreeSet::new();
            if policy.post_apply_checks.iter().any(|name| {
                !check_names.contains(name.as_str()) || !selected_checks.insert(name.as_str())
            }) {
                return Err(invalid(
                    "Integration post_apply_checks must be unique declared checks",
                ));
            }
            let mut priorities = BTreeSet::new();
            if policy
                .reviewer_priority
                .iter()
                .any(|node| !ids.contains(node.as_str()) || !priorities.insert(node.as_str()))
            {
                return Err(invalid(
                    "Integration reviewer_priority must name unique pipeline nodes",
                ));
            }
            let mut protected = BTreeSet::new();
            if policy
                .protected_paths
                .iter()
                .any(|path| !is_valid_repo_path(path) || !protected.insert(path.as_str()))
            {
                return Err(invalid(
                    "Integration protected_paths must be unique canonical repository paths",
                ));
            }
        }

        for node in &self.nodes {
            if node.kind.is_worker() {
                match (self.version, &node.execution) {
                    (4 | 5, Some(execution)) => {
                        let authority = execution.validate(&node.id)?;
                        if let Some(budgets) = &self.budgets {
                            let attempt_cap =
                                node.budget.map_or(budgets.attempt, |budget| budget.attempt);
                            if authority > attempt_cap {
                                return Err(invalid(format!(
                                    "brokered reviewer `{}` aggregate Broker authority ({authority}) exceeds its attempt cap ({attempt_cap}); the dispatch reservation would not cover its capability",
                                    node.id
                                )));
                            }
                        }
                    }
                    (4 | 5, None) => {
                        return Err(invalid(format!(
                            "pipeline format version {} requires reviewer-capable node `{}` to declare an Execution Binding",
                            self.version, node.id
                        )));
                    }
                    (_, Some(_)) => {
                        return Err(invalid(format!(
                            "reviewer `{}` cannot declare an Execution Binding before pipeline version 4",
                            node.id
                        )));
                    }
                    (_, None) => {}
                }
            } else if node.demands.is_some() {
                return Err(invalid(format!(
                    "node `{}` is not a reviewer but classifies reviewer Demands",
                    node.id
                )));
            } else if node.execution.is_some() {
                return Err(invalid(format!(
                    "node `{}` is not a reviewer but declares a reviewer Execution Binding",
                    node.id
                )));
            }
            match (node.kind.is_worker(), &node.runner, &node.package) {
                (true, None, None) => {
                    return Err(invalid(format!(
                        "reviewer node `{}` binds neither a runner nor a package; a reviewer \
                         with nothing to run would be a node that always reports nothing",
                        node.id
                    )));
                }
                (true, Some(_), Some(_)) => {
                    return Err(invalid(format!(
                        "reviewer node `{}` binds both a runner and a package; exactly one \
                         must say what runs",
                        node.id
                    )));
                }
                (true, Some(_), None) if subject == SubjectKind::Diff => {
                    return Err(invalid(format!(
                        "reviewer node `{}` uses an inline runner, which has no package \
                         manifest declaring `diff` Subject support",
                        node.id
                    )));
                }
                (false, Some(_), _) | (false, _, Some(_)) => {
                    return Err(invalid(format!(
                        "node `{}` is not a reviewer but binds a runner or package",
                        node.id
                    )));
                }
                _ => {}
            }
        }

        if self.nodes.is_empty() {
            return Err(invalid(
                "pipeline defines no nodes; an empty review cannot produce a valid round",
            ));
        }
        if !self.nodes.iter().any(|node| node.kind.is_worker()) {
            return Err(invalid(
                "pipeline defines no reviewer; a review with no reviewer cannot produce claims",
            ));
        }
        if self.check_timeout_seconds == Some(0) {
            return Err(invalid("check_timeout_seconds must be positive"));
        }
        Ok(())
    }
}

fn wired(edges: &[EdgeSpec], from: (&str, &str), to: (&str, &str)) -> bool {
    edges.iter().any(|edge| {
        edge.from.node == from.0
            && edge.from.port == from.1
            && edge.to.node == to.0
            && edge.to.port == to.1
    })
}

fn validate_diff_change_set_wiring(
    nodes: &[NodeSpec],
    edges: &[EdgeSpec],
) -> Result<(), DefinitionError> {
    let exact = |port: &PortContractSpec| {
        port.artifact_type() == contract::CHANGE_SET_V1
            && port.cardinality() == PortCardinality::One
            && !port.optional()
            && port.snapshot_affinity() == SnapshotAffinity::SameSubject
    };
    let producers: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind == NodeKindSpec::Generation)
        .flat_map(|node| {
            node.outputs
                .iter()
                .filter(|port| port.artifact_type() == contract::CHANGE_SET_V1)
                .map(move |port| (node, port))
        })
        .collect();
    let [(producer, producer_port)] = producers.as_slice() else {
        return Err(invalid(
            "a `diff` pipeline must have exactly one typed ChangeSet@1 producer",
        ));
    };
    if !exact(producer_port) {
        return Err(invalid(
            "a `diff` pipeline's ChangeSet@1 producer must be required, singular, and bound to the subject snapshot",
        ));
    }
    for reviewer in nodes.iter().filter(|node| node.kind.is_worker()) {
        let inputs: Vec<_> = reviewer
            .inputs
            .iter()
            .filter(|port| port.artifact_type() == contract::CHANGE_SET_V1)
            .collect();
        let [reviewer_port] = inputs.as_slice() else {
            return Err(invalid(format!(
                "diff reviewer `{}` must declare exactly one ChangeSet@1 input",
                reviewer.id
            )));
        };
        if !exact(reviewer_port)
            || !wired(
                edges,
                (&producer.id, producer_port.name()),
                (&reviewer.id, reviewer_port.name()),
            )
        {
            return Err(invalid(format!(
                "diff reviewer `{}` must receive generation's exact ChangeSet@1 through its typed input",
                reviewer.id
            )));
        }
    }
    Ok(())
}

fn validate_generation_output_contracts(
    nodes: &[NodeSpec],
    subject: SubjectKind,
    version: u32,
) -> Result<(), DefinitionError> {
    if version == 1 {
        return Ok(());
    }
    let mut change_sets = 0_usize;
    for node in nodes
        .iter()
        .filter(|node| node.kind == NodeKindSpec::Generation)
    {
        let mut prior_findings = 0_usize;
        for port in &node.outputs {
            match port.artifact_type() {
                contract::PRIOR_FINDINGS_V1 => prior_findings += 1,
                contract::FINDING_SET_V1 => {
                    if port.cardinality() != PortCardinality::One
                        || !port.optional()
                        || port.snapshot_affinity() != SnapshotAffinity::Any
                    {
                        return Err(invalid(format!(
                            "generation node `{}` exact FindingSet@1 output `{}` must be optional, singular, and snapshot-affinity `any`",
                            node.id,
                            port.name()
                        )));
                    }
                    prior_findings += 1;
                }
                contract::CHANGE_SET_V1 => {
                    if subject != SubjectKind::Diff {
                        return Err(invalid(format!(
                            "generation node `{}` output `{}` declares `{}`, which only a `diff` Subject can supply",
                            node.id,
                            port.name(),
                            contract::CHANGE_SET_V1,
                        )));
                    }
                    change_sets += 1;
                }
                artifact_type => {
                    return Err(invalid(format!(
                        "generation node `{}` output `{}` has unsupported type `{artifact_type}`; pipeline version 2 requires Generation outputs to use a typed port declaration for `{}`, `{}`, or `{}`",
                        node.id,
                        port.name(),
                        contract::PRIOR_FINDINGS_V1,
                        contract::FINDING_SET_V1,
                        contract::CHANGE_SET_V1,
                    )));
                }
            }
        }
        if prior_findings == 0 {
            return Err(invalid(format!(
                "generation node `{}` must emit an explicit `{}` compatibility view or exact `{}` output",
                node.id,
                contract::PRIOR_FINDINGS_V1,
                contract::FINDING_SET_V1,
            )));
        }
    }
    if subject == SubjectKind::Diff && change_sets != 1 {
        return Err(invalid(format!(
            "a `diff` pipeline must declare exactly one generation `{}` output",
            contract::CHANGE_SET_V1,
        )));
    }
    Ok(())
}

fn validate_disposition_wiring(
    nodes: &[NodeSpec],
    edges: &[EdgeSpec],
) -> Result<(), DefinitionError> {
    let finding_set_outputs: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind == NodeKindSpec::Generation)
        .flat_map(|node| {
            node.outputs
                .iter()
                .filter(|port| port.artifact_type() == contract::FINDING_SET_V1)
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
            .any(|port| port.artifact_type() == contract::REVIEWER_RESULT_V2);
        if !uses_v2 {
            continue;
        }
        let inputs: Vec<_> = reviewer
            .inputs
            .iter()
            .filter(|port| port.artifact_type() == contract::FINDING_SET_V1)
            .collect();
        let [input] = inputs.as_slice() else {
            return Err(invalid(format!(
                "ReviewerResult@2 reviewer `{}` must declare exactly one FindingSet@1 input",
                reviewer.id
            )));
        };
        if input.cardinality() != PortCardinality::One
            || !input.optional()
            || input.snapshot_affinity() != SnapshotAffinity::Any
        {
            return Err(invalid(format!(
                "ReviewerResult@2 reviewer `{}` FindingSet@1 input must be optional, singular, and snapshot-affinity `any`",
                reviewer.id
            )));
        }
        if !finding_set_outputs.iter().any(|(generation, output)| {
            wired(
                edges,
                (&generation.id, output.name()),
                (&reviewer.id, input.name()),
            )
        }) {
            return Err(invalid(format!(
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
) -> Result<(), DefinitionError> {
    let uses_dynamic = nodes.iter().any(|node| {
        matches!(node.kind, NodeKindSpec::Slicer | NodeKindSpec::Scatter)
            || node.slicing.is_some()
            || node.closeout_for.is_some()
    });
    if version != 5 {
        if uses_dynamic {
            return Err(invalid(
                "Slicer, Scatter, slicing, and closeout_for require pipeline format version 5",
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
            return Err(invalid(format!(
                "node `{}` marks closeout_for but is not a whole-Subject reviewer",
                node.id
            )));
        }
        if node.kind != NodeKindSpec::Slicer && node.slicing.is_some() {
            return Err(invalid(format!(
                "node `{}` declares slicing but is not a Slicer",
                node.id
            )));
        }
        if node.kind != NodeKindSpec::Slicer {
            continue;
        }
        let policy = node
            .slicing
            .as_ref()
            .ok_or_else(|| invalid(format!("Slicer `{}` has no slicing policy", node.id)))?;
        if policy.max_paths_per_slice == 0 || policy.max_fanout == 0 {
            return Err(invalid(format!(
                "Slicer `{}` has a zero path or fan-out bound",
                node.id
            )));
        }
        let closeout = policy.closeout_policy()?;
        if policy.coverage == SliceCoverageV1::Complete && !policy.all_shards_required {
            return Err(invalid(format!(
                "complete Slicer `{}` cannot make shards optional",
                node.id
            )));
        }
        let scatter = by_id.get(policy.scatter.as_str()).ok_or_else(|| {
            invalid(format!(
                "Slicer `{}` names absent Scatter `{}`",
                node.id, policy.scatter
            ))
        })?;
        if scatter.kind != NodeKindSpec::Scatter {
            return Err(invalid(format!(
                "Slicer `{}` target `{}` is not a Scatter",
                node.id, policy.scatter
            )));
        }
        let slice_outputs: Vec<_> = node
            .outputs
            .iter()
            .filter(|port| port.artifact_type() == contract::SLICE_SET_V1)
            .collect();
        let slice_inputs: Vec<_> = scatter
            .inputs
            .iter()
            .filter(|port| port.artifact_type() == contract::SLICE_SET_V1)
            .collect();
        let ([slice_output], [slice_input]) = (slice_outputs.as_slice(), slice_inputs.as_slice())
        else {
            return Err(invalid(format!(
                "Slicer `{}` and Scatter `{}` must expose exactly one SliceSet@1 route",
                node.id, scatter.id
            )));
        };
        if slice_output.cardinality() != PortCardinality::One
            || slice_output.optional()
            || slice_output.snapshot_affinity() != SnapshotAffinity::SameSubject
            || slice_input.cardinality() != PortCardinality::One
            || slice_input.optional()
            || slice_input.snapshot_affinity() != SnapshotAffinity::SameSubject
            || !wired(
                edges,
                (&node.id, slice_output.name()),
                (&scatter.id, slice_input.name()),
            )
        {
            return Err(invalid(format!(
                "Slicer `{}` must durably feed its required same-Subject SliceSet@1 to Scatter `{}`",
                node.id, scatter.id
            )));
        }
        let shard_outputs: Vec<_> = scatter
            .outputs
            .iter()
            .filter(|port| port.artifact_type() == contract::SHARD_SET_V1)
            .collect();
        let [shard_output] = shard_outputs.as_slice() else {
            return Err(invalid(format!(
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
                .filter(|port| port.artifact_type() == contract::SHARD_SET_V1)
                .any(|input| {
                    input.cardinality() == PortCardinality::One
                        && !input.optional()
                        && wired(
                            edges,
                            (&scatter.id, shard_output.name()),
                            (&candidate.id, input.name()),
                        )
                })
        });
        if !ledger_route {
            return Err(invalid(format!(
                "Scatter `{}` must route its lossless ShardSet@1 directly to a Ledger",
                scatter.id
            )));
        }
        let closeouts: Vec<_> = nodes
            .iter()
            .filter(|candidate| candidate.closeout_for.as_deref() == Some(scatter.id.as_str()))
            .collect();
        match closeout {
            CloseoutPolicyV1::Required => {
                let [reviewer] = closeouts.as_slice() else {
                    return Err(invalid(format!(
                        "Scatter `{}` requires exactly one whole-Subject closeout reviewer",
                        scatter.id
                    )));
                };
                let shard_inputs: Vec<_> = reviewer
                    .inputs
                    .iter()
                    .filter(|port| port.artifact_type() == contract::SHARD_SET_V1)
                    .collect();
                let [shard_input] = shard_inputs.as_slice() else {
                    return Err(invalid(format!(
                        "closeout reviewer `{}` must accept exactly one ShardSet@1",
                        reviewer.id
                    )));
                };
                if shard_output.cardinality() != PortCardinality::One
                    || shard_output.optional()
                    || shard_input.cardinality() != PortCardinality::One
                    || shard_input.optional()
                    || !wired(
                        edges,
                        (&scatter.id, shard_output.name()),
                        (&reviewer.id, shard_input.name()),
                    )
                {
                    return Err(invalid(format!(
                        "Scatter `{}` must feed its lossless ShardSet@1 to closeout reviewer `{}`",
                        scatter.id, reviewer.id
                    )));
                }
            }
            CloseoutPolicyV1::Waived { .. } if !closeouts.is_empty() => {
                return Err(invalid(format!(
                    "Scatter `{}` has both a closeout waiver and an executing closeout reviewer",
                    scatter.id
                )));
            }
            CloseoutPolicyV1::Waived { .. } => {}
        }
    }
    Ok(())
}
