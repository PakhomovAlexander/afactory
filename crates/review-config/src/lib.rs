//! The pipeline definition: a review that is configured rather than constructed.
//!
//! Until now a pipeline existed only as Rust. That is fine for proving properties and useless
//! for a project that wants to describe its own review, so this is the loader for the file
//! format — the `.af/` shape the design's project layout describes.
//!
//! The shape itself — every `*Spec` type, [`PipelineDefinition`], its one parser and its pure
//! rules — lives in `review_core::definition`, beneath the event store, because pinned-authority
//! replay reads the same bytes and must agree with the loader field for field. This crate
//! re-exports the shape unchanged and adds only what it alone can do: resolve `package = "…"`
//! reviewers against the lockfile and plan the typed graph. [`Definition`] is the loader's
//! handle on that shape.
//!
//! Every struct denies unknown fields. A typo in a pipeline must be an error, not a setting that
//! silently does nothing — the same rule the contracts use, for the same reason.

pub mod lock;
pub mod pipeline_edit;

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use review_check::CheckDefinition;
use review_core::Command;
use review_graph::{
    Dispatch, Node, NodeKind, Pipeline, PlanError, Planned, Port, PortContract, RunReport,
    Scheduler,
};
use review_store::ConvergencePolicy;
use serde::{Deserialize, Serialize};

pub use review_core::definition::{
    ArgSpec, BudgetSpec, BudgetUnit, CacheKindSpec, CheckSpec, CloseoutModeSpec, CommandSpec,
    ConvergenceSpec, DefinitionError, EdgeSpec, GateExecutionSpec, GateModeSpec, IntegrationSpec,
    IsolationSpec, NodeBudgetSpec, NodeKindSpec, NodeSpec, PipelineDefinition, PortContractSpec,
    PortSpec, ProvenanceSpec, ReviewerExecutionSpec, SUPPORTED_VERSIONS, SandboxProviderSpec,
    SeveritySpec, SlicingSpec, SubjectSpec, TypedPortSpec,
};

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
                "pipeline definition: unsupported version {v}; this kernel understands versions {} through {}",
                SUPPORTED_VERSIONS.start(),
                SUPPORTED_VERSIONS.end()
            ),
            ConfigError::Lock(e) => write!(f, "pipeline definition: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The shape's own refusals are this crate's refusals, by kind: the parse, the version, and
/// every pure rule keep the variant they always had here.
impl From<DefinitionError> for ConfigError {
    fn from(error: DefinitionError) -> ConfigError {
        match error {
            DefinitionError::Parse(message) => ConfigError::Parse(message),
            DefinitionError::UnknownVersion(version) => ConfigError::UnknownVersion(version),
            DefinitionError::Invalid(message) => ConfigError::Binding(message),
        }
    }
}

/// A whole pipeline definition, as a project writes it: the loader's handle on
/// [`PipelineDefinition`], read through `Deref` and never reshaped. The wire shape is
/// transparent, so this parses, serializes, and compares exactly as the kernel contract does;
/// what it adds is the loader — [`from_toml`](Self::from_toml) with this crate's error, and
/// [`load`](Self::load) / [`load_with`](Self::load_with), which bind packages and plan the
/// graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Definition(PipelineDefinition);

impl Deref for Definition {
    type Target = PipelineDefinition;

    fn deref(&self) -> &PipelineDefinition {
        &self.0
    }
}

impl DerefMut for Definition {
    fn deref_mut(&mut self) -> &mut PipelineDefinition {
        &mut self.0
    }
}

impl From<PipelineDefinition> for Definition {
    fn from(definition: PipelineDefinition) -> Definition {
        Definition(definition)
    }
}

impl From<Definition> for PipelineDefinition {
    fn from(definition: Definition) -> PipelineDefinition {
        definition.0
    }
}

/// The graph's kind for a declared node kind.
pub(crate) fn node_kind(spec: NodeKindSpec) -> NodeKind {
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

/// The graph contract a port declaration expands to, read through the same accessors the
/// shape's wiring rules and pinned-authority replay use — so a name-keyed port means exactly
/// one thing to the planner and to the store.
pub(crate) fn port_contract(spec: &PortContractSpec) -> PortContract {
    let contract = PortContract::new(spec.name(), spec.artifact_type())
        .with_cardinality(spec.cardinality())
        .with_snapshot_affinity(spec.snapshot_affinity());
    if spec.optional() {
        contract.optional()
    } else {
        contract
    }
}

/// A validated definition: the plan, the checks, and the reviewer bindings.
pub struct Loaded {
    version: u32,
    subject: SubjectSpec,
    plan: Planned,
    checks: Vec<CheckDefinition>,
    check_timeout_seconds: u64,
    max_parallel: usize,
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

    /// The bound on simultaneously running nodes the scheduler enforces for this pipeline.
    pub fn max_parallel(&self) -> usize {
        self.max_parallel
    }

    /// Reviewer and Scatter nodes, in plan order, that declare an input port of `artifact_type`
    /// — for example every node that receives the exact prior `FindingSet@1`.
    pub fn reviewer_nodes_receiving(&self, artifact_type: &str) -> Vec<String> {
        self.plan
            .order
            .iter()
            .filter(|id| {
                let node = &self.plan.nodes[*id];
                matches!(node.kind, NodeKind::Reviewer | NodeKind::Scatter)
                    && node
                        .inputs
                        .iter()
                        .any(|port| port.artifact_type == artifact_type)
            })
            .cloned()
            .collect()
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
        Ok(Scheduler::new(&self.plan)
            .with_parallelism(self.max_parallel)
            .run(dispatcher))
    }
}

impl Definition {
    /// Parse the shape through the kernel's one parser; a refusal is this crate's
    /// [`ConfigError::Parse`].
    pub fn from_toml(text: &str) -> Result<Definition, ConfigError> {
        Ok(Definition(PipelineDefinition::from_toml(text)?))
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
        let definition = self.0;
        // Every rule the shape can judge alone — the same rules pinned-authority replay applies.
        // What follows is only what needs the lockfile and the planner.
        definition.validate()?;
        let subject = SubjectSpec {
            kind: definition.subject_kind(),
        };

        let mut pipeline = Pipeline::default();
        let mut reviewers = BTreeMap::new();
        let mut demand_requirements = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut reviewer_execution = BTreeMap::new();
        let mut slicing = BTreeMap::new();
        let mut closeouts = BTreeMap::new();
        let mut resolved_packages: BTreeMap<String, std::sync::Arc<lock::ResolvedReviewer>> =
            BTreeMap::new();
        for spec in &definition.nodes {
            if spec.kind.is_worker() {
                demand_requirements.insert(
                    spec.id.clone(),
                    spec.demands
                        .unwrap_or(review_core::DemandRequirement::Required),
                );
                if let Some(execution) = &spec.execution {
                    reviewer_execution.insert(spec.id.clone(), execution.clone());
                }
            }
            if let Some(policy) = &spec.slicing {
                slicing.insert(spec.id.clone(), policy.clone());
            }
            if let Some(scatter) = &spec.closeout_for {
                closeouts.insert(scatter.clone(), spec.id.clone());
            }
            let mut node = Node::new(&spec.id, node_kind(spec.kind))
                .accepting_contracts(spec.inputs.iter().map(port_contract).collect())
                .emitting_contracts(spec.outputs.iter().map(port_contract).collect());
            if let Some(gate) = &spec.gated_by {
                node = node.gated_by(gate);
            }
            pipeline = pipeline.node(node);

            // Which of `runner` and `package` a Worker binds, and whether a non-Worker binds
            // either, was judged above; binding what runs is the loader's job.
            match (&spec.runner, &spec.package) {
                (Some(command), None) => {
                    reviewers.insert(spec.id.clone(), command.build());
                }
                (None, Some(package)) => {
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
                _ => {}
            }
        }

        for edge in &definition.edges {
            pipeline = pipeline.edge(
                Port::new(&edge.from.node, &edge.from.port),
                Port::new(&edge.to.node, &edge.to.port),
            );
        }

        let check_timeout_seconds = definition.check_timeout_seconds.unwrap_or(3600);
        // `validate()` already refused zero; only the width conversion is this crate's.
        let max_parallel = match definition.max_parallel {
            None => review_graph::DEFAULT_MAX_PARALLEL,
            Some(bound) => usize::try_from(bound).map_err(|_| {
                ConfigError::Binding("max_parallel exceeds this platform".to_string())
            })?,
        };
        let plan = pipeline.plan().map_err(ConfigError::Plan)?;
        let checks = definition
            .checks
            .iter()
            .map(|c| {
                let check = CheckDefinition::new(&c.name, c.command.build());
                if c.required { check } else { check.optional() }
            })
            .collect();

        let node_attempt_caps = definition
            .nodes
            .iter()
            .filter_map(|node| node.budget.map(|budget| (node.id.clone(), budget.attempt)))
            .collect();
        Ok(Loaded {
            version: definition.version,
            subject,
            plan,
            checks,
            check_timeout_seconds,
            max_parallel,
            gate: definition.gate,
            reviewers,
            demand_requirements,
            packages,
            reviewer_execution,
            slicing,
            closeouts,
            budgets: definition.budgets,
            node_attempt_caps,
            integration: definition.integration,
            convergence: ConvergencePolicy {
                clean_rounds: definition.convergence.clean_rounds,
                max_rounds: definition.convergence.max_rounds,
                gate: definition.convergence.gate.into(),
            },
        })
    }
}
