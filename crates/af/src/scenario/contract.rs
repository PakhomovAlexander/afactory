//! Current-release Scenario wire contracts. Definitions are data; only machine policy grants effects.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) type Result<T> = std::result::Result<T, String>;
pub(super) const MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_TIME: u64 = 8_640_000_000_000;

pub(super) fn require(ok: bool, why: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(why.into()) }
}
pub(super) fn hash(value: &impl Serialize) -> Result<String> {
    let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
    let bytes = review_store::canonicalize(&value).map_err(|e| e.to_string())?;
    Ok(digest(&bytes))
}
pub(super) fn digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        review_core::hex::encode(&Sha256::digest(bytes))
    )
}
pub(super) fn execution_id() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(digest(&bytes))
}
pub(super) fn name(s: &str) -> Result<()> {
    require(
        review_core::task::is_name(s),
        "Invalid Scenario, Run, Ant, step or port name",
    )
}
pub(super) fn text(s: &str) -> Result<()> {
    require(
        !s.is_empty() && s.len() <= 65_536 && !s.contains('\0'),
        "Empty or oversized text",
    )
}

fn unique<'de, D, T>(deserializer: D) -> std::result::Result<BTreeSet<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Ord,
{
    let values = Vec::<T>::deserialize(deserializer)?;
    let mut result = BTreeSet::new();
    for value in values {
        if !result.insert(value) {
            return Err(serde::de::Error::custom("Duplicate authority entry"));
        }
    }
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Port {
    Text,
    Boolean,
    Integer,
    Task,
    Action,
    Decision,
    Observation,
    OwnedFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum Datum {
    Text(String),
    Boolean(bool),
    Integer(u64),
    Task(TaskReceipt),
    Action(ActionReceipt),
    Decision(DecisionReceipt),
    Observation(Observation),
    OwnedFile(OwnedFile),
}
impl Datum {
    pub fn port(&self) -> Port {
        match self {
            Self::Text(_) => Port::Text,
            Self::Boolean(_) => Port::Boolean,
            Self::Integer(_) => Port::Integer,
            Self::Task(_) => Port::Task,
            Self::Action(_) => Port::Action,
            Self::Decision(_) => Port::Decision,
            Self::Observation(_) => Port::Observation,
            Self::OwnedFile(_) => Port::OwnedFile,
        }
    }
    pub fn scalar(&self) -> bool {
        matches!(self, Self::Text(_) | Self::Boolean(_) | Self::Integer(_))
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Text(s) => text(s),
            Self::Integer(n) => require(
                *n <= review_core::json::SAFE_INTEGER_MAX as u64,
                "Unsafe integer",
            ),
            Self::Boolean(_) => Ok(()),
            Self::Task(r) => {
                name(&r.task_id)?;
                require(
                    [&r.plan_id, &r.revision_id, &r.result_id]
                        .iter()
                        .all(|s| review_core::is_digest(s)),
                    "Invalid Task receipt identity",
                )?;
                r.result.validate()?;
                require(
                    r.result.task_revision_id == r.revision_id
                        && r.result.acceptance == review_core::task::TaskAcceptanceV1::Satisfied
                        && r.result.execution == review_core::task::TaskExecutionV1::Completed,
                    "Task receipt lacks acceptance",
                )
            }
            Self::Action(r) => {
                text(&r.intent_id)?;
                text(&r.evidence)?;
                text(&r.subject)?;
                text(&r.revision)
            }
            Self::Decision(r) => r.request.validate(),
            Self::Observation(r) => r.validate(),
            Self::OwnedFile(r) => require(
                r.root.is_absolute()
                    && review_core::task::is_name(&r.file)
                    && review_core::is_digest(&r.digest)
                    && !r.owner.is_empty(),
                "Invalid file ownership receipt",
            ),
        }
    }
    pub fn as_text(&self) -> Result<&str> {
        match self {
            Self::Text(s) => Ok(s),
            _ => Err("Expected a text input".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskReceipt {
    pub task_id: String,
    pub plan_id: String,
    pub revision_id: String,
    pub result_id: String,
    pub charged_tokens: u64,
    pub result: review_core::task::TaskResultV1,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActionReceipt {
    pub intent_id: String,
    pub operation: Operation,
    pub destination: String,
    pub subject: String,
    pub revision: String,
    pub evidence: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observation {
    pub subject: String,
    pub revision: String,
    pub condition: String,
    pub satisfied: bool,
    pub evidence: String,
}
impl Observation {
    pub fn validate(&self) -> Result<()> {
        for s in [
            &self.subject,
            &self.revision,
            &self.condition,
            &self.evidence,
        ] {
            text(s)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnedFile {
    pub root: PathBuf,
    pub file: String,
    pub digest: String,
    pub owner: String,
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Binding {
    Literal {
        value: Box<Datum>,
    },
    Input {
        port: String,
    },
    Step {
        step: String,
        port: String,
    },
    Project {
        value: Box<Binding>,
        projection: Projection,
    },
    Select {
        condition: Box<Binding>,
        when_true: Box<Binding>,
        when_false: Box<Binding>,
    },
    Equals {
        left: Box<Binding>,
        right: Box<Binding>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Projection {
    ActionIntent,
    WorkspaceSubject,
    WorkspaceRevision,
    WorkspacePublication,
    WorkspaceReceipt,
    TaskConclusion,
    TaskResultId,
    TaskOutputId { port: String },
    TaskOutputSnapshot { port: String },
    ObservationSatisfied,
    DecisionApproved,
}
impl Projection {
    fn types(&self) -> Result<(Port, Port)> {
        Ok(match self {
            Self::ActionIntent
            | Self::WorkspaceSubject
            | Self::WorkspaceRevision
            | Self::WorkspacePublication
            | Self::WorkspaceReceipt => (Port::Action, Port::Text),
            Self::TaskConclusion | Self::TaskResultId => (Port::Task, Port::Text),
            Self::TaskOutputId { port } | Self::TaskOutputSnapshot { port } => {
                name(port)?;
                (Port::Task, Port::Text)
            }
            Self::ObservationSatisfied => (Port::Observation, Port::Boolean),
            Self::DecisionApproved => (Port::Decision, Port::Boolean),
        })
    }
    pub fn apply(&self, value: Datum) -> Result<Datum> {
        Ok(match (self, value) {
            (Self::WorkspaceSubject, Datum::Action(a)) => {
                Datum::Text(super::workspace::Prepared::parse(&a)?.subject()?)
            }
            (Self::WorkspaceRevision, Datum::Action(a)) => {
                Datum::Text(super::workspace::Prepared::parse(&a)?.commit)
            }
            (Self::WorkspacePublication, Datum::Action(a)) => Datum::Text(
                serde_json::to_string(&super::workspace::Prepared::parse(&a)?.publication())
                    .map_err(|e| e.to_string())?,
            ),
            (Self::WorkspaceReceipt, Datum::Action(a)) => {
                super::workspace::Prepared::parse(&a)?;
                Datum::Text(a.evidence)
            }
            (Self::ActionIntent, Datum::Action(a)) => Datum::Text(a.intent_id),
            (Self::TaskConclusion, Datum::Task(t)) => Datum::Text(t.result.domain_conclusion),
            (Self::TaskResultId, Datum::Task(t)) => Datum::Text(t.result_id),
            (Self::TaskOutputId { port }, Datum::Task(t)) => {
                let output = t
                    .result
                    .outputs
                    .get(port)
                    .ok_or("Task output port absent")?;
                require(
                    output.cardinality == review_core::PortCardinality::One
                        && output.artifact_ids.len() == 1,
                    "Task output projection requires one exact artifact",
                )?;
                Datum::Text(output.artifact_ids[0].clone())
            }
            (Self::TaskOutputSnapshot { port }, Datum::Task(t)) => {
                let output = t
                    .result
                    .outputs
                    .get(port)
                    .ok_or("Task output port absent")?;
                Datum::Text(
                    output
                        .snapshot_id
                        .clone()
                        .ok_or("Task output has no exact Snapshot identity")?,
                )
            }
            (Self::ObservationSatisfied, Datum::Observation(o)) => Datum::Boolean(o.satisfied),
            (Self::DecisionApproved, Datum::Decision(d)) => Datum::Boolean(d.request.approve),
            _ => return Err("Projection input type differs".into()),
        })
    }
}
impl Binding {
    fn port(
        &self,
        inputs: &BTreeMap<String, Port>,
        previous: &BTreeMap<String, BTreeMap<String, Port>>,
    ) -> Result<Port> {
        match self {
            Self::Project { value, projection } => {
                let (input, output) = projection.types()?;
                require(
                    value.port(inputs, previous)? == input,
                    "Projection input port type mismatch",
                )?;
                Ok(output)
            }
            Self::Select {
                condition,
                when_true,
                when_false,
            } => {
                require(
                    condition.port(inputs, previous)? == Port::Boolean,
                    "Selection requires a boolean condition",
                )?;
                let port = when_true.port(inputs, previous)?;
                require(
                    port == when_false.port(inputs, previous)? && port != Port::Decision,
                    "Selection requires equal non-Decision branch types",
                )?;
                Ok(port)
            }
            Self::Equals { left, right } => {
                let p = left.port(inputs, previous)?;
                require(
                    matches!(p, Port::Text | Port::Boolean | Port::Integer)
                        && p == right.port(inputs, previous)?,
                    "Equality requires two scalars of the same type",
                )?;
                Ok(Port::Boolean)
            }
            Self::Literal { value } => {
                value.validate()?;
                require(value.scalar(), "Receipts cannot be literal authority")?;
                Ok(value.port())
            }
            Self::Input { port } => inputs
                .get(port)
                .copied()
                .ok_or_else(|| format!("Unknown input port {port}")),
            Self::Step { step, port } => previous
                .get(step)
                .and_then(|p| p.get(port))
                .copied()
                .ok_or_else(|| format!("Invalid or forward step port {step}.{port}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Limits {
    pub max_tokens: u64,
    pub max_wall_ms: u64,
    pub max_dispatches: u32,
}
impl Limits {
    pub fn validate(&self) -> Result<()> {
        require(
            self.max_tokens <= review_core::json::SAFE_INTEGER_MAX as u64
                && self.max_wall_ms > 0
                && self.max_wall_ms <= MAX_TIME
                && self.max_dispatches > 0
                && self.max_dispatches <= 10_000,
            "Invalid Scenario limits",
        )
    }
    pub fn within(&self, parent: &Self) -> bool {
        self.max_tokens <= parent.max_tokens
            && self.max_wall_ms <= parent.max_wall_ms
            && self.max_dispatches <= parent.max_dispatches
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Operation {
    PublishPr,
    Followup,
    Notify,
    ExperimentSubmitV1,
    PrepareCandidateV1,
    CleanupWorkspaceV1,
    WriteOwnedFile,
    CleanupOwnedFile,
    Observe,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Grant {
    pub adapter: String,
    pub operation: Operation,
    pub destination: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Authority {
    #[serde(deserialize_with = "unique")]
    pub task_plans: BTreeSet<String>,
    #[serde(deserialize_with = "unique")]
    pub task_templates: BTreeSet<String>,
    #[serde(deserialize_with = "unique")]
    pub grants: BTreeSet<Grant>,
    #[serde(deserialize_with = "unique")]
    pub decision_owners: BTreeSet<String>,
}
impl Authority {
    pub fn within(&self, parent: &Self) -> bool {
        self.task_templates.is_subset(&parent.task_templates)
            && self.task_plans.is_subset(&parent.task_plans)
            && self.grants.is_subset(&parent.grants)
            && self.decision_owners.is_subset(&parent.decision_owners)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskTarget {
    pub repo: PathBuf,
    pub state: PathBuf,
    pub task_id: String,
    pub plan_id: String,
    pub revision_id: String,
    pub max_tokens: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Timeout {
    Fail,
    Skip,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Kind {
    FreshTask {
        template: Box<super::template::Template>,
        bindings: BTreeMap<String, super::template::Predecessor>,
    },
    Task {
        target: TaskTarget,
    },
    Scenario {
        scenario: String,
    },
    Action {
        grant: Grant,
        requires_decision: bool,
        required_condition: Option<String>,
    },
    Decision {
        owner: String,
    },
    Wait {
        grant: Grant,
        condition: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Step {
    pub id: String,
    pub inputs: BTreeMap<String, Binding>,
    pub outputs: BTreeMap<String, Port>,
    pub when: Option<Binding>,
    pub max_attempts: u8,
    pub timeout_ms: u64,
    pub on_timeout: Timeout,
    pub operation: Kind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scenario {
    pub schema: String,
    pub inputs: BTreeMap<String, Port>,
    pub outputs: BTreeMap<String, Port>,
    pub steps: Vec<Step>,
    pub result: BTreeMap<String, Binding>,
    pub completion: Vec<Binding>,
    pub authority: Authority,
    pub limits: Limits,
    #[serde(default)]
    pub admission: Option<super::roadmap::Admission>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Bundle {
    pub schema: String,
    pub root: String,
    pub definitions: BTreeMap<String, Scenario>,
}

/// Decision provenance and Action uses at a definition's boundary. Internal receipts
/// which are not exported need not escape the summary. Memoization avoids expanding a shared
/// child definition once for every possible invocation path.
#[derive(Clone, Default)]
struct DecisionFlow {
    uses: BTreeMap<String, DecisionUses>,
    outputs: BTreeMap<String, String>,
}

#[derive(Clone, Default)]
struct DecisionUses {
    actions: u8,
    publication: bool,
}
impl DecisionUses {
    fn merge(&mut self, other: Self) -> Result<()> {
        // Only zero, one and multiple consumers matter. Keep this bounded even when a
        // shared child definition is invoked repeatedly or several ports alias one receipt.
        self.actions = (self.actions + other.actions).min(2);
        self.publication |= other.publication;
        require(
            !self.publication || self.actions <= 1,
            "Publication decision has multiple Action consumers",
        )
    }
}

fn decision_origin(
    binding: &Binding,
    inputs: &BTreeMap<String, String>,
    previous: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<String> {
    match binding {
        Binding::Input { port } => inputs.get(port),
        Binding::Step { step, port } => previous.get(step).and_then(|p| p.get(port)),
        _ => None,
    }
    .cloned()
    .ok_or_else(|| "Publication requires a pinned Decision source".into())
}

impl Bundle {
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-bundle@4"
                && !self.definitions.is_empty()
                && self.definitions.len() <= 64,
            "Unsupported or oversized Scenario bundle",
        )?;
        name(&self.root)?;
        let mut reachable = BTreeSet::new();
        self.visit(
            &self.root,
            &mut Vec::new(),
            &mut reachable,
            &mut BTreeMap::new(),
        )?;
        require(
            reachable.len() == self.definitions.len(),
            "Bundle contains unreachable definitions",
        )?;
        for (id, definition) in &self.definitions {
            if self.definitions[&self.root].admission.is_some() {
                require(
                    definition
                        .steps
                        .iter()
                        .all(|s| !matches!(s.operation, Kind::Task { .. })),
                    "Roadmap loop Tasks must be fresh template invocations throughout the bundle",
                )?;
            }
            name(id)?;
            definition.validate(self)?;
        }
        self.publication_decisions(&self.root, &mut BTreeMap::new())?;
        Ok(())
    }
    fn publication_decisions(
        &self,
        id: &str,
        summaries: &mut BTreeMap<String, DecisionFlow>,
    ) -> Result<DecisionFlow> {
        if let Some(summary) = summaries.get(id) {
            return Ok(summary.clone());
        }
        let definition = &self.definitions[id];
        let inputs: BTreeMap<_, _> = definition
            .inputs
            .iter()
            .filter(|(_, p)| **p == Port::Decision)
            .map(|(port, _)| (port.clone(), format!("input/{port}")))
            .collect();
        let mut flow = DecisionFlow::default();
        for origin in inputs.values() {
            flow.uses.insert(origin.clone(), DecisionUses::default());
        }
        let mut previous = BTreeMap::new();
        for step in &definition.steps {
            let mut outputs = BTreeMap::new();
            match &step.operation {
                Kind::Decision { .. } => {
                    let origin = format!("step/{}", step.id);
                    flow.uses.insert(origin.clone(), DecisionUses::default());
                    outputs.insert("result".into(), origin);
                }
                Kind::Scenario { scenario } => {
                    let child = self.publication_decisions(scenario, summaries)?;
                    let mut origins = BTreeMap::new();
                    for (origin, count) in child.uses {
                        let source = if let Some(port) = origin.strip_prefix("input/") {
                            decision_origin(&step.inputs[port], &inputs, &previous)?
                        } else {
                            format!("step/{}/{origin}", step.id)
                        };
                        flow.uses.entry(source.clone()).or_default().merge(count)?;
                        origins.insert(origin, source);
                    }
                    for (port, origin) in child.outputs {
                        outputs.insert(port, origins[&origin].clone());
                    }
                }
                Kind::Action {
                    grant,
                    requires_decision: true,
                    ..
                } => {
                    let origin = decision_origin(&step.inputs["decision"], &inputs, &previous)?;
                    flow.uses.entry(origin).or_default().merge(DecisionUses {
                        actions: 1,
                        publication: matches!(
                            grant.operation,
                            Operation::PublishPr | Operation::Followup
                        ),
                    })?;
                }
                _ => {}
            }
            previous.insert(step.id.clone(), outputs);
        }
        for (port, kind) in &definition.outputs {
            if *kind == Port::Decision {
                flow.outputs.insert(
                    port.clone(),
                    decision_origin(&definition.result[port], &inputs, &previous)?,
                );
            }
        }
        flow.uses.retain(|origin, _| {
            origin.starts_with("input/") || flow.outputs.values().any(|o| o == origin)
        });
        summaries.insert(id.into(), flow.clone());
        Ok(flow)
    }
    fn visit(
        &self,
        id: &str,
        stack: &mut Vec<String>,
        reached: &mut BTreeSet<String>,
        heights: &mut BTreeMap<String, usize>,
    ) -> Result<usize> {
        require(
            !stack.iter().any(|s| s == id),
            "Recursive Scenario invocation",
        )?;
        if let Some(height) = heights.get(id) {
            return Ok(*height);
        }
        let def = self
            .definitions
            .get(id)
            .ok_or_else(|| format!("Missing child Scenario {id}"))?;
        reached.insert(id.into());
        stack.push(id.into());
        let mut height = 1;
        for step in &def.steps {
            if let Kind::Scenario { scenario } = &step.operation {
                height = height.max(1 + self.visit(scenario, stack, reached, heights)?);
            }
        }
        require(height <= 16, "Over-deep Scenario invocation")?;
        stack.pop();
        heights.insert(id.into(), height);
        Ok(height)
    }
}
impl Scenario {
    fn validate(&self, bundle: &Bundle) -> Result<()> {
        require(
            self.schema == "af/scenario@4"
                && !self.outputs.is_empty()
                && self.outputs.len() <= 32
                && self.inputs.len() <= 32
                && !self.steps.is_empty()
                && self.steps.len() <= 128
                && !self.completion.is_empty()
                && self.completion.len() <= 32,
            "Invalid Scenario contract",
        )?;
        self.limits.validate()?;
        if let Some(admission) = &self.admission {
            admission.validate()?;
        }
        require(
            self.authority.task_templates.len() <= 256
                && self
                    .authority
                    .task_templates
                    .iter()
                    .all(|p| review_core::is_digest(p)),
            "Invalid template authority",
        )?;
        for p in self.inputs.keys().chain(self.outputs.keys()) {
            name(p)?;
        }
        require(
            self.authority.task_plans.len() <= 256
                && self
                    .authority
                    .task_plans
                    .iter()
                    .all(|p| review_core::is_digest(p)),
            "Invalid Scenario Task plan authority",
        )?;
        for owner in &self.authority.decision_owners {
            name(owner)?;
        }
        let mut previous = BTreeMap::new();
        for step in &self.steps {
            if let Kind::Scenario { scenario } = &step.operation {
                require(
                    bundle
                        .definitions
                        .get(scenario)
                        .is_some_and(|c| c.admission.is_none()),
                    "Child Scenario cannot admit another roadmap goal",
                )?;
            }
            name(&step.id)?;
            require(
                !previous.contains_key(&step.id)
                    && step.max_attempts > 0
                    && step.max_attempts <= 8
                    && step.timeout_ms > 0
                    && step.timeout_ms <= self.limits.max_wall_ms,
                "Invalid step identity, retry or timeout bound",
            )?;
            require(
                step.on_timeout != Timeout::Skip
                    || matches!(step.operation, Kind::Wait { .. } | Kind::Decision { .. }),
                "Only Wait and Decision steps may skip on timeout",
            )?;
            if let Some(binding) = &step.when {
                require(
                    binding.port(&self.inputs, &previous)? == Port::Boolean,
                    "Condition must be boolean",
                )?;
            }
            let actual: BTreeMap<String, Port> = step
                .inputs
                .iter()
                .map(|(n, b)| {
                    name(n)?;
                    Ok((n.clone(), b.port(&self.inputs, &previous)?))
                })
                .collect::<Result<_>>()?;
            let mut expected = BTreeMap::new();
            let output;
            match &step.operation {
                Kind::FreshTask { template, bindings } => {
                    template.validate()?;
                    super::template::validate_bindings(bindings)?;
                    require(
                        self.authority.task_templates.contains(&template.id()?),
                        "Task template exceeds Scenario authority",
                    )?;
                    require(
                        template.max_tokens <= self.limits.max_tokens,
                        "Template exceeds Scenario budget",
                    )?;
                    expected = bindings.keys().map(|k| (k.clone(), Port::Task)).collect();
                    if step.inputs.contains_key("scenario_evidence") {
                        expected.insert("scenario_evidence".into(), Port::Observation);
                    }
                    output = Port::Task;
                }
                Kind::Task { target } => {
                    name(&target.task_id)?;
                    require(
                        self.authority.task_plans.contains(&target.plan_id),
                        "Task plan exceeds Scenario authority",
                    )?;
                    require(
                        target.repo.is_absolute()
                            && target.state.is_absolute()
                            && review_core::is_digest(&target.plan_id)
                            && review_core::is_digest(&target.revision_id)
                            && target.max_tokens <= self.limits.max_tokens,
                        "Invalid pinned Task target or budget",
                    )?;
                    output = Port::Task;
                }
                Kind::Scenario { scenario } => {
                    let child = bundle
                        .definitions
                        .get(scenario)
                        .ok_or("Missing child definition")?;
                    require(
                        child.authority.within(&self.authority)
                            && child.limits.within(&self.limits),
                        "Child Scenario widens authority or budgets",
                    )?;
                    expected = child.inputs.clone();
                    require(
                        step.outputs == child.outputs,
                        "Child output contract mismatch",
                    )?;
                    require(actual == expected, "Child input contract mismatch")?;
                    previous.insert(step.id.clone(), step.outputs.clone());
                    continue;
                }
                Kind::Decision { owner } => {
                    require(
                        self.authority.decision_owners.contains(owner),
                        "Unauthorized decision owner",
                    )?;
                    expected.insert("subject".into(), Port::Text);
                    expected.insert("revision".into(), Port::Text);
                    output = Port::Decision;
                }
                Kind::Wait { grant, condition } => {
                    require(
                        grant.operation == Operation::Observe
                            && self.authority.grants.contains(grant),
                        "Unauthorized Wait adapter",
                    )?;
                    text(condition)?;
                    expected.insert("subject".into(), Port::Text);
                    expected.insert("revision".into(), Port::Text);
                    output = Port::Observation;
                }
                Kind::Action {
                    grant,
                    requires_decision,
                    required_condition,
                } => {
                    require(
                        self.authority.grants.contains(grant)
                            && grant.operation != Operation::Observe,
                        "Unauthorized action",
                    )?;
                    require(
                        !matches!(grant.operation, Operation::PublishPr | Operation::Followup)
                            || *requires_decision,
                        "Publication requires an exact human decision",
                    )?;
                    match grant.operation {
                        Operation::PrepareCandidateV1 => {
                            expected.insert("subject".into(), Port::Text);
                            expected.insert("revision".into(), Port::Text);
                            expected.insert("body".into(), Port::Text);
                            expected.insert("candidate".into(), Port::Task);
                            if step.inputs.contains_key("previous") {
                                expected.insert("previous".into(), Port::Action);
                            }
                            output = Port::Action;
                        }
                        Operation::CleanupWorkspaceV1 => {
                            require(
                                required_condition.as_deref() == Some("merged"),
                                "Workspace cleanup requires exact merge evidence",
                            )?;
                            expected.insert("subject".into(), Port::Text);
                            expected.insert("revision".into(), Port::Text);
                            expected.insert("body".into(), Port::Text);
                            expected.insert("prepared".into(), Port::Action);
                            output = Port::Action;
                        }
                        Operation::ExperimentSubmitV1 => {
                            expected.insert("subject".into(), Port::Text);
                            expected.insert("revision".into(), Port::Text);
                            expected.insert("body".into(), Port::Text);
                            expected.insert("candidate".into(), Port::Task);
                            output = Port::Action;
                        }
                        Operation::WriteOwnedFile => {
                            expected.insert("file".into(), Port::Text);
                            expected.insert("content".into(), Port::Text);
                            output = Port::OwnedFile;
                        }
                        Operation::CleanupOwnedFile => {
                            expected.insert("owned".into(), Port::OwnedFile);
                            output = Port::Action;
                        }
                        _ => {
                            expected.insert("subject".into(), Port::Text);
                            expected.insert("revision".into(), Port::Text);
                            expected.insert("body".into(), Port::Text);
                            output = Port::Action;
                        }
                    }
                    if *requires_decision {
                        expected.insert("decision".into(), Port::Decision);
                    }
                    if let Some(condition) = required_condition {
                        text(condition)?;
                        expected.insert("evidence".into(), Port::Observation);
                    }
                }
            }
            require(
                actual == expected,
                &format!("Step {} input ports differ: expected {expected:?}", step.id),
            )?;
            require(
                step.outputs == BTreeMap::from([("result".into(), output)]),
                "Step must declare its typed result port",
            )?;
            previous.insert(step.id.clone(), step.outputs.clone());
        }
        require(
            self.result.keys().eq(self.outputs.keys()),
            "Scenario result ports differ",
        )?;
        for (p, b) in &self.result {
            require(
                b.port(&self.inputs, &previous)? == self.outputs[p],
                "Scenario output type mismatch",
            )?;
        }
        for b in &self.completion {
            require(
                b.port(&self.inputs, &previous)? == Port::Boolean,
                "Completion conditions must be boolean",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Adapter {
    pub executable: PathBuf,
    pub digest: String,
    #[serde(deserialize_with = "unique")]
    pub operations: BTreeSet<Operation>,
    #[serde(deserialize_with = "unique")]
    pub destinations: BTreeSet<String>,
    pub max_wall_ms: u64,
    pub max_output_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub schema: String,
    pub adapters: BTreeMap<String, Adapter>,
    #[serde(deserialize_with = "unique")]
    pub task_plans: BTreeSet<String>,
    #[serde(deserialize_with = "unique")]
    pub task_templates: BTreeSet<String>,
    pub decision_keys: BTreeMap<String, String>,
    #[serde(deserialize_with = "unique")]
    pub file_roots: BTreeSet<PathBuf>,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-policy@4"
                && self.adapters.len() <= 32
                && self.decision_keys.len() <= 32
                && self.task_templates.len() <= 256
                && self
                    .task_templates
                    .iter()
                    .all(|p| review_core::is_digest(p))
                && self.task_plans.len() <= 256
                && self.file_roots.len() <= 32,
            "Invalid machine Scenario policy",
        )?;
        for (id, a) in &self.adapters {
            name(id)?;
            require(
                id != "builtin"
                    && a.executable.is_absolute()
                    && review_core::is_digest(&a.digest)
                    && a.max_wall_ms > 0
                    && a.max_wall_ms <= 30_000
                    && a.max_output_bytes > 0
                    && a.max_output_bytes <= 65_536
                    && !a.operations.is_empty()
                    && !a.destinations.is_empty(),
                "Invalid bounded adapter",
            )?;
            require(
                !a.operations.contains(&Operation::WriteOwnedFile)
                    && !a.operations.contains(&Operation::CleanupOwnedFile),
                "File actions use the builtin adapter",
            )?;
            for d in &a.destinations {
                text(d)?;
            }
        }
        for (owner, key) in &self.decision_keys {
            name(owner)?;
            minisign_verify::PublicKey::decode(key).map_err(|e| e.to_string())?;
        }
        require(
            self.task_plans.iter().all(|p| review_core::is_digest(p)),
            "Invalid authorized Task plan",
        )?;
        require(
            self.file_roots.iter().all(|p| p.is_absolute()),
            "File roots must be absolute",
        )
    }
    pub fn admits(&self, bundle: &Bundle) -> Result<()> {
        self.validate()?;
        for def in bundle.definitions.values() {
            require(
                def.authority.task_templates.is_subset(&self.task_templates),
                "Scenario Task template authority exceeds machine policy",
            )?;
            require(
                def.authority.task_plans.is_subset(&self.task_plans),
                "Scenario Task plan authority exceeds machine policy",
            )?;
            for grant in &def.authority.grants {
                if matches!(
                    grant.operation,
                    Operation::WriteOwnedFile | Operation::CleanupOwnedFile
                ) {
                    require(
                        grant.adapter == "builtin"
                            && self.file_roots.contains(&PathBuf::from(&grant.destination)),
                        "Unauthorized file root",
                    )?;
                } else {
                    let a = self
                        .adapters
                        .get(&grant.adapter)
                        .ok_or("Unknown authorized adapter")?;
                    require(
                        a.operations.contains(&grant.operation)
                            && a.destinations.contains(&grant.destination),
                        "Adapter authority exceeds machine policy",
                    )?;
                }
            }
            for owner in &def.authority.decision_owners {
                require(
                    self.decision_keys.contains_key(owner),
                    "Decision owner has no machine signing key",
                )?;
            }
            for step in &def.steps {
                if let Kind::Task { target } = &step.operation {
                    require(
                        self.task_plans.contains(&target.plan_id),
                        "Task plan not authorized by machine policy",
                    )?;
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pin {
    pub schema: String,
    pub bundle: Bundle,
    pub policy: Policy,
    pub revision: String,
    pub definitions: BTreeMap<String, String>,
}
impl Pin {
    pub fn new(bundle: Bundle, policy: Policy) -> Result<Self> {
        bundle.validate()?;
        policy.admits(&bundle)?;
        let definitions = bundle
            .definitions
            .iter()
            .map(|(n, d)| Ok((n.clone(), hash(d)?)))
            .collect::<Result<_>>()?;
        let revision = hash(&(&bundle, &policy))?;
        Ok(Self {
            schema: "af/scenario-pin@4".into(),
            bundle,
            policy,
            revision,
            definitions,
        })
    }
    pub fn validate(&self) -> Result<()> {
        require(
            *self == Self::new(self.bundle.clone(), self.policy.clone())?,
            "Corrupt transitive Scenario pin",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecisionRequest {
    pub schema: String,
    pub run_id: String,
    pub execution_id: String,
    pub pin_id: String,
    pub step: String,
    pub subject: String,
    pub revision: String,
    pub owner: String,
    pub approve: bool,
    pub expires_unix_ms: u64,
}
impl DecisionRequest {
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-decision@1"
                && review_core::is_digest(&self.execution_id)
                && review_core::is_digest(&self.pin_id)
                && self.expires_unix_ms > 0
                && self.expires_unix_ms <= review_core::json::SAFE_INTEGER_MAX as u64,
            "Invalid decision request",
        )?;
        name(&self.run_id)?;
        name(&self.owner)?;
        text(&self.step)?;
        text(&self.subject)?;
        text(&self.revision)
    }
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = b"af/scenario-decision@1\n".to_vec();
        bytes.extend(
            review_store::canonicalize(&serde_json::to_value(self).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        );
        Ok(bytes)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecisionReceipt {
    pub schema: String,
    pub request: DecisionRequest,
    pub signature: String,
}
impl DecisionReceipt {
    pub fn verify(&self, policy: &Policy) -> Result<()> {
        require(
            self.schema == "af/scenario-signed-decision@1",
            "Unsupported signed decision envelope",
        )?;
        require(
            !self.signature.is_empty() && self.signature.len() <= 4096,
            "Invalid decision signature bound",
        )?;
        let key = policy
            .decision_keys
            .get(&self.request.owner)
            .ok_or("Unknown decision signer")?;
        let key = minisign_verify::PublicKey::decode(key).map_err(|e| e.to_string())?;
        let signature =
            minisign_verify::Signature::decode(&self.signature).map_err(|e| e.to_string())?;
        key.verify(&self.request.signing_bytes()?, &signature, false)
            .map_err(|e| format!("Decision authentication failed: {e}"))
    }
}

pub(super) fn input_values(
    contract: &BTreeMap<String, Port>,
    values: &BTreeMap<String, Datum>,
) -> Result<()> {
    require(
        contract.keys().eq(values.keys()),
        "Concrete input ports differ from the contract",
    )?;
    for (n, value) in values {
        value.validate()?;
        require(contract[n] == value.port(), "Concrete port type mismatch")?;
    }
    Ok(())
}
