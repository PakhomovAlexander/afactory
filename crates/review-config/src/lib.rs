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

use std::collections::BTreeMap;

use review_check::CheckDefinition;
use review_core::{Arg, Command, Provenance};
use review_graph::{
    Node, NodeKind, Pipeline, PlanError, Planned, Port, PortCardinality, PortContract,
    SnapshotAffinity,
};
use review_store::ConvergencePolicy;
use serde::{Deserialize, Serialize};

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
                "pipeline definition: unsupported version {v}; this kernel understands versions 1 and 2"
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

/// A port declaration. The string arm keeps v1 pipeline files readable and expands to an
/// explicit opaque/one/required/any contract; new and shipped definitions use the typed arm.
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
/// Owner decision, 2026-08-18: tokens as the unit; heavy defaults 300k/attempt, 2M/run; a run
/// that exhausts finishes in-flight work and reports incomplete; a fenced attempt charges.
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

/// A whole pipeline definition, as a project writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub version: u32,
    #[serde(default)]
    pub subject: Option<SubjectSpec>,
    #[serde(default)]
    pub checks: Vec<CheckSpec>,
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
    pub subject: SubjectSpec,
    pub plan: Planned,
    pub checks: Vec<CheckDefinition>,
    pub reviewers: BTreeMap<String, Command>,
    /// Package-backed reviewers, by node: name, exact version, digest, verified root. What a
    /// run manifest records so replay can prove which reviewer bytes were used.
    pub packages: BTreeMap<String, std::sync::Arc<lock::ResolvedReviewer>>,
    pub convergence: ConvergencePolicy,
    pub budgets: Option<BudgetSpec>,
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
        let subject = match (self.version, self.subject) {
            (1, None) => SubjectSpec {
                kind: review_core::SubjectKind::WholeTree,
            },
            (1, Some(_)) => {
                return Err(ConfigError::Binding(
                    "pipeline format version 1 has no `[subject]`; use version 2 to declare it"
                        .to_string(),
                ));
            }
            (2, Some(subject)) => subject,
            (2, None) => {
                return Err(ConfigError::Binding(
                    "pipeline format version 2 requires `[subject]`".to_string(),
                ));
            }
            (version, _) => return Err(ConfigError::UnknownVersion(version)),
        };
        if let Some(budgets) = &self.budgets {
            if budgets.attempt == 0 || budgets.run == 0 {
                return Err(ConfigError::Binding(
                    "a zero-token budget cap means nothing can ever dispatch;                      omit [budgets] to run uncapped"
                        .to_string(),
                ));
            }
            if budgets.attempt > budgets.run {
                return Err(ConfigError::Binding(format!(
                    "the attempt cap ({}) exceeds the run cap ({}); no attempt could ever                      reserve",
                    budgets.attempt, budgets.run
                )));
            }
        }

        let mut pipeline = Pipeline::default();
        let mut reviewers = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut resolved_packages: BTreeMap<String, std::sync::Arc<lock::ResolvedReviewer>> =
            BTreeMap::new();
        for spec in &self.nodes {
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
            subject,
            plan,
            checks,
            reviewers,
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
