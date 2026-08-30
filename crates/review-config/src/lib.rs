//! The pipeline definition: a review that is configured rather than constructed.
//!
//! Until now a pipeline existed only as Rust. That is fine for proving properties and useless
//! for a project that wants to describe its own review, so this is the file format — the
//! `.review/` shape the design's project layout describes.
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

pub mod lock;
pub mod pipeline_edit;

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
                "pipeline definition: unsupported version {v}; this kernel understands versions 1, 2, and 3"
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
    Gather,
    Ledger,
}

impl From<NodeKindSpec> for NodeKind {
    fn from(spec: NodeKindSpec) -> NodeKind {
        match spec {
            NodeKindSpec::Generation => NodeKind::Generation,
            NodeKindSpec::Gate => NodeKind::Gate,
            NodeKindSpec::Reviewer => NodeKind::Reviewer,
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
        .filter(|node| node.kind == NodeKindSpec::Reviewer)
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
    convergence: ConvergencePolicy,
    budgets: Option<BudgetSpec>,
}

/// A dispatcher that declares the Subject semantics it actually executes.
pub trait SubjectDispatch: Dispatch + Sync {
    fn subject_kind(&self) -> review_core::SubjectKind;
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

    pub fn convergence(&self) -> &ConvergencePolicy {
        &self.convergence
    }

    pub fn budgets(&self) -> Option<&BudgetSpec> {
        self.budgets.as_ref()
    }

    pub fn plan_order(&self) -> &[String] {
        &self.plan.order
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
            (2 | 3, None, _) => {
                return Err(ConfigError::Binding(format!(
                    "pipeline format version {} requires `[subject]`",
                    self.version
                )));
            }
            (3, Some(_), None) => {
                return Err(ConfigError::Binding(
                    "pipeline format version 3 requires an explicit `[gate]` Execution Binding"
                        .to_string(),
                ));
            }
            (3, Some(subject), Some(gate)) => (subject, Some(gate)),
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
        if subject.kind == review_core::SubjectKind::Diff {
            validate_diff_change_set_wiring(&self.nodes, &self.edges)?;
        }
        if let Some(budgets) = &self.budgets {
            if budgets.attempt == 0 || budgets.run == 0 {
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

        let mut pipeline = Pipeline::default();
        let mut reviewers = BTreeMap::new();
        let mut demand_requirements = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut resolved_packages: BTreeMap<String, std::sync::Arc<lock::ResolvedReviewer>> =
            BTreeMap::new();
        for spec in &self.nodes {
            if spec.kind == NodeKindSpec::Reviewer {
                demand_requirements.insert(
                    spec.id.clone(),
                    spec.demands
                        .unwrap_or(review_core::DemandRequirement::Required),
                );
            } else if spec.demands.is_some() {
                return Err(ConfigError::Binding(format!(
                    "node `{}` is not a reviewer but classifies reviewer Demands",
                    spec.id
                )));
            }
            let mut node = Node::new(&spec.id, spec.kind.into())
                .accepting_contracts(spec.inputs.iter().map(PortContractSpec::build).collect())
                .emitting_contracts(spec.outputs.iter().map(PortContractSpec::build).collect());
            if let Some(gate) = &spec.gated_by {
                node = node.gated_by(gate);
            }
            pipeline = pipeline.node(node);

            match (spec.kind, &spec.runner, &spec.package) {
                (NodeKindSpec::Reviewer, None, None) => {
                    return Err(ConfigError::Binding(format!(
                        "reviewer node `{}` binds neither a runner nor a package; a reviewer \
                         with nothing to run would be a node that always reports nothing",
                        spec.id
                    )));
                }
                (NodeKindSpec::Reviewer, Some(_), Some(_)) => {
                    return Err(ConfigError::Binding(format!(
                        "reviewer node `{}` binds both a runner and a package; exactly one \
                         must say what runs",
                        spec.id
                    )));
                }
                (NodeKindSpec::Reviewer, Some(command), None) => {
                    if subject.kind == review_core::SubjectKind::Diff {
                        return Err(ConfigError::Binding(format!(
                            "reviewer node `{}` uses an inline runner, which has no package \
                             manifest declaring `diff` Subject support",
                            spec.id
                        )));
                    }
                    reviewers.insert(spec.id.clone(), command.build());
                }
                (NodeKindSpec::Reviewer, None, Some(package)) => {
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
            budgets: self.budgets,
            convergence: ConvergencePolicy {
                clean_rounds: self.convergence.clean_rounds,
                max_rounds: self.convergence.max_rounds,
                gate: self.convergence.gate.into(),
            },
        })
    }
}
