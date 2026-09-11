//! Pure compilation of reusable Task Pipelines. Registry entries come from captured trusted
//! packages; declarations in a Pipeline cannot invent an operator's interface or authority.

use std::collections::{BTreeMap, BTreeSet};

use review_attempt::task_budget::{NodeAllowance, TaskBudget};
use review_core::task::pipeline::{
    PipelineContractV1, PipelineDefinitionV1, PipelinePortV1, PortAffinityV1, ReceiptOutcomeV1,
    TaskOperatorV1, ValueRefV1, WorkerSlotV1,
};
use review_core::task::{ArtifactInputV1, TaskRevisionV1};
use serde::{Deserialize, Serialize};

use crate::{Node, NodeKind, Pipeline, Planned, Port, PortContract, SnapshotAffinity};

/// Metadata authenticated by the package resolver. The key is the Worker package name, or
/// the installed operator name. Evidence is keyed by output and exact verifier policy ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorSignature {
    pub contract: PipelineContractV1,
    pub effects: BTreeSet<String>,
    pub evidence: BTreeMap<String, BTreeSet<String>>,
    /// Public output envelopes retain these exact input receipts. Runtime admission verifies
    /// that provenance; a Pipeline's covers declaration cannot create a retention guarantee.
    #[serde(default)]
    pub retains: BTreeMap<String, BTreeSet<String>>,
    pub roles: BTreeSet<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub worker_input_type: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub worker_output_type: Option<String>,
    /// The sole typed receipt port available to `when`. Ordinary artifacts cannot branch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub outcome_port: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "review_core::task::present_option"
    )]
    pub attempt: Option<OperatorAttemptCost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorAttemptCost {
    pub tokens: u64,
    pub wall_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Address {
    pub node: String,
    pub port: String,
}

impl Address {
    pub fn qualified(&self) -> String {
        format!("{}.{}", self.node, self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompiledOperator {
    RootInputs,
    Select,
    /// Installed host bootstrap, never a Pipeline-supplied operation. All listed slots use
    /// the same captured Provider capability and share this admission Attempt.
    ProviderAdmission {
        bindings: BTreeSet<String>,
    },
    Primitive {
        operator: TaskOperatorV1,
        signature: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledNode {
    pub operator: CompiledOperator,
    pub contract: PipelineContractV1,
    pub inputs: BTreeMap<String, Address>,
    pub conditions: Vec<CompiledCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledCondition {
    pub source: Address,
    pub outcome: ReceiptOutcomeV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledCall {
    pub pipeline: String,
    pub inputs: BTreeMap<String, Address>,
    pub outputs: BTreeMap<String, Address>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub coverage: BTreeMap<String, Address>,
    pub max_attempts: u32,
    pub max_parallel: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledTask {
    pub schema: String,
    pub nodes: BTreeMap<String, CompiledNode>,
    pub order: Vec<String>,
    pub inputs: BTreeMap<String, ArtifactInputV1>,
    pub outputs: BTreeMap<String, Address>,
    pub coverage: BTreeMap<String, Address>,
    pub calls: BTreeMap<String, CompiledCall>,
    pub slots: BTreeMap<String, WorkerSlotV1>,
    /// Every default whose contract constrained a replacement, including mapped child slots.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub replaced_workers: BTreeMap<String, BTreeSet<String>>,
    pub max_parallel: u32,
    pub allowances: BTreeMap<String, NodeAllowance>,
}

impl CompiledTask {
    pub fn require_provider_admission(
        &mut self,
        bindings: &BTreeMap<String, review_core::task::plan::EffectiveWorkerBindingV1>,
        cost: &OperatorAttemptCost,
        limits: &review_core::task::TaskLimitsV1,
    ) -> Result<(), String> {
        use review_core::task::plan::WorkerExecutionV1;
        if cost.tokens == 0 || cost.wall_ms == 0 {
            return Err("Provider admission requires a bounded paid reservation".into());
        }
        if self
            .nodes
            .values()
            .any(|node| matches!(node.operator, CompiledOperator::ProviderAdmission { .. }))
        {
            return Err("Provider admission can be compiled only once".into());
        }
        let mut capabilities: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (slot, binding) in bindings {
            if matches!(binding.execution, WorkerExecutionV1::Model { .. }) {
                let key =
                    serde_json::to_string(&(&binding.execution, &binding.invocation_policy_id))
                        .map_err(|e| e.to_string())?;
                capabilities.entry(key).or_default().insert(slot.clone());
            }
        }
        for (index, slots) in capabilities.into_values().enumerate() {
            let name = format!("root.providers.admit{index}");
            if self.nodes.contains_key(&name) || self.nodes.len() >= 64 {
                return Err("Provider admission exceeds the installed graph bound".into());
            }
            let mut protected = false;
            for (id, node) in &mut self.nodes {
                if let CompiledOperator::Primitive {
                    operator:
                        TaskOperatorV1::Worker { slot }
                        | TaskOperatorV1::Verify { slot }
                        | TaskOperatorV1::FixVerify { slot },
                    ..
                } = &node.operator
                    && slots.contains(slot)
                {
                    node.conditions.push(CompiledCondition {
                        source: Address {
                            node: name.clone(),
                            port: "result".into(),
                        },
                        outcome: ReceiptOutcomeV1::Passed,
                    });
                    protected |= self
                        .allowances
                        .get(id)
                        .is_some_and(|a| a.verification_attempts > 0);
                }
            }
            self.nodes.insert(
                name.clone(),
                CompiledNode {
                    operator: CompiledOperator::ProviderAdmission { bindings: slots },
                    contract: PipelineContractV1 {
                        inputs: BTreeMap::new(),
                        outputs: BTreeMap::from([(
                            "result".into(),
                            PipelinePortV1 {
                                artifact_type: "af/TaskProviderAdmission@1".into(),
                                cardinality: review_core::PortCardinality::One,
                                optional: false,
                                affinity: PortAffinityV1::Unbound {},
                                root_default: None,
                                covers: BTreeSet::new(),
                            },
                        )]),
                    },
                    inputs: BTreeMap::new(),
                    conditions: vec![],
                },
            );
            self.allowances.insert(
                name.clone(),
                NodeAllowance {
                    tokens_per_attempt: cost.tokens,
                    wall_ms_per_attempt: cost.wall_ms,
                    max_attempts: 1,
                    verification_attempts: u32::from(protected),
                },
            );
            self.order.insert(index, name);
        }
        self.order = self.scheduler_plan()?.order;
        self.budget(limits.clone())?;
        Ok(())
    }

    pub fn budget(&self, limits: review_core::task::TaskLimitsV1) -> Result<TaskBudget, String> {
        TaskBudget::new(limits, self.allowances.clone())?.with_call_limits(
            self.calls
                .iter()
                .map(|(scope, call)| (scope.clone(), call.max_attempts))
                .collect(),
        )
    }

    pub fn run(&self, dispatch: &(dyn crate::Dispatch + Sync)) -> Result<crate::RunReport, String> {
        let plan = self.scheduler_plan()?;
        let limits = self
            .calls
            .iter()
            .map(|(scope, call)| (scope.clone(), call.max_parallel as usize))
            .collect();
        Ok(crate::Scheduler::new(&plan)
            .with_parallelism(self.max_parallel as usize)
            .with_scope_limits(limits)?
            .run(dispatch))
    }

    /// Receipt interpretation belongs to the trusted domain adapter. The graph receives only
    /// its typed outcome; diagnostics and missing execution never become a negative receipt.
    pub fn node_selected(
        &self,
        node: &str,
        inputs: &crate::ArtifactMap,
        read_outcome: impl Fn(&str) -> Result<ReceiptOutcomeV1, String>,
    ) -> Result<bool, String> {
        let mut unavailable = false;
        for (index, condition) in self
            .nodes
            .get(node)
            .ok_or("Unknown Task node")?
            .conditions
            .iter()
            .enumerate()
        {
            let ids = inputs
                .get(&condition_input(index))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            match ids {
                [id] if read_outcome(id)? != condition.outcome => return Ok(false),
                [_] => (),
                [] => unavailable = true,
                _ => return Err("Branch condition has more than one receipt".into()),
            }
        }
        if unavailable {
            Err("Required branch receipt was not produced".into())
        } else {
            Ok(true)
        }
    }

    pub fn select_output(
        &self,
        node: &str,
        inputs: &crate::ArtifactMap,
        read_outcome: impl Fn(&str) -> Result<ReceiptOutcomeV1, String>,
    ) -> Result<crate::ArtifactMap, String> {
        if !matches!(
            self.nodes.get(node).map(|n| &n.operator),
            Some(CompiledOperator::Select)
        ) {
            return Err("Node is not a typed Select".into());
        }
        let [receipt] = inputs.get("condition").map(Vec::as_slice).unwrap_or(&[]) else {
            return Err("Select requires exactly one admitted receipt".into());
        };
        let arm = outcome_name(read_outcome(receipt)?);
        let values = inputs.get(arm).ok_or("Select arm is absent")?;
        if values.is_empty() {
            return Err(format!(
                "Selected {arm} branch did not produce its required value"
            ));
        }
        Ok(BTreeMap::from([("output".into(), values.clone())]))
    }

    /// The existing graph planner remains the authority for DAG topology. Task lineage and
    /// named evidence were proven separately; legacy SameSubject cannot represent S0 -> S1.
    pub fn scheduler_plan(&self) -> Result<Planned, String> {
        let mut pipeline = Pipeline::default();
        for (id, node) in &self.nodes {
            if node
                .contract
                .inputs
                .keys()
                .any(|name| name.starts_with("af_condition_"))
            {
                return Err("Task input uses a reserved scheduler guard name".into());
            }
            let ports = |ports: &BTreeMap<String, PipelinePortV1>| -> Vec<PortContract> {
                ports
                    .iter()
                    .map(|(name, p)| PortContract {
                        name: name.clone(),
                        artifact_type: p.artifact_type.clone(),
                        cardinality: p.cardinality,
                        optional: p.optional,
                        snapshot_affinity: SnapshotAffinity::Any,
                    })
                    .collect()
            };
            let mut input_ports = ports(&node.contract.inputs);
            for (index, condition) in node.conditions.iter().enumerate() {
                let source =
                    &self.nodes[&condition.source.node].contract.outputs[&condition.source.port];
                input_ports.push(PortContract {
                    name: condition_input(index),
                    artifact_type: source.artifact_type.clone(),
                    cardinality: source.cardinality,
                    optional: true,
                    snapshot_affinity: SnapshotAffinity::Any,
                });
                pipeline = pipeline.edge(
                    Port::new(&condition.source.node, &condition.source.port),
                    Port::new(id, condition_input(index)),
                );
            }
            pipeline = pipeline.node(
                Node::new(id, NodeKind::Task)
                    .accepting_contracts(input_ports)
                    .emitting_contracts(ports(&node.contract.outputs)),
            );
            for (name, source) in &node.inputs {
                pipeline =
                    pipeline.edge(Port::new(&source.node, &source.port), Port::new(id, name));
            }
        }
        pipeline.plan().map_err(|e| e.to_string())
    }
}

pub struct CompileContext<'a> {
    pub pipelines: &'a BTreeMap<String, PipelineDefinitionV1>,
    pub signatures: &'a BTreeMap<String, OperatorSignature>,
    /// Explicit captured local settings keyed by physical qualified slot, never model input.
    pub slot_workers: BTreeMap<String, String>,
    /// Trusted Task-kind policy: each obligation identifies the public final output whose
    /// Snapshot its evidence must judge. Pipeline authors cannot redirect this obligation.
    pub acceptance_outputs: BTreeMap<String, String>,
    pub max_nodes: usize,
    pub max_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Lineage {
    Root(String),
    Derived(Address, Box<Lineage>),
    Choice(Address, Box<[Lineage; 3]>),
}

impl Lineage {
    fn derived_from(&self, source: &Self) -> bool {
        match self {
            Self::Root(_) => false,
            Self::Derived(_, parent) => parent.as_ref() == source || parent.derived_from(source),
            Self::Choice(_, arms) => arms.iter().all(|arm| arm.derived_from(source)),
        }
    }
}

struct Compiler<'a> {
    context: &'a CompileContext<'a>,
    task: &'a TaskRevisionV1,
    graph: CompiledTask,
    stack: Vec<String>,
    /// Lineage and evidence associated with physical output ports, never caller claims.
    lineage: BTreeMap<Address, Lineage>,
    evidence: BTreeMap<Address, BTreeSet<String>>,
    availability: BTreeMap<Address, Vec<CompiledCondition>>,
    outcomes: BTreeSet<Address>,
    retained: BTreeMap<Address, BTreeSet<Address>>,
}

pub fn compile_task(
    task: &TaskRevisionV1,
    root: &str,
    context: &CompileContext<'_>,
) -> Result<CompiledTask, String> {
    task.validate()?;
    if context.max_nodes == 0
        || context.max_nodes > 64
        || context.max_depth == 0
        || context.max_depth > 4
    {
        return Err("Compilation requires bounded node and depth limits".into());
    }
    let definition = context
        .pipelines
        .get(root)
        .ok_or("Root Pipeline is not installed")?;
    definition.validate()?;
    if !definition.accepts.kinds.contains(&task.kind)
        || definition
            .accepts
            .required_facts
            .iter()
            .any(|(k, v)| task.facts.get(k) != Some(v))
    {
        return Err("Task kind or known facts do not fit the root Pipeline".into());
    }
    let mut compiler = Compiler {
        context,
        task,
        stack: Vec::new(),
        lineage: BTreeMap::new(),
        evidence: BTreeMap::new(),
        availability: BTreeMap::new(),
        outcomes: BTreeSet::new(),
        retained: BTreeMap::new(),
        graph: CompiledTask {
            schema: "af.compiled-task/1".into(),
            nodes: BTreeMap::new(),
            order: Vec::new(),
            inputs: task.inputs.clone(),
            outputs: BTreeMap::new(),
            coverage: BTreeMap::new(),
            calls: BTreeMap::new(),
            slots: BTreeMap::new(),
            replaced_workers: BTreeMap::new(),
            max_parallel: definition.max_parallel,
            allowances: BTreeMap::new(),
        },
    };
    let mut root_inputs = BTreeMap::new();
    for (name, port) in &definition.contract.inputs {
        let Some(input) = task.inputs.get(name) else {
            if port.optional {
                continue;
            }
            return Err(format!(
                "Missing normalized root input {name}; apply an admitted constructor before compilation"
            ));
        };
        input.validate()?;
        if input.artifact_type != port.artifact_type || input.cardinality != port.cardinality {
            return Err(format!(
                "Root input {name} has an incompatible type or cardinality"
            ));
        }
        let address = Address {
            node: "root.inputs".into(),
            port: name.clone(),
        };
        compiler.lineage.insert(
            address.clone(),
            Lineage::Root(
                input
                    .snapshot_id
                    .clone()
                    .unwrap_or_else(|| address.qualified()),
            ),
        );
        root_inputs.insert(name.clone(), address);
    }
    if task
        .inputs
        .keys()
        .any(|name| !definition.contract.inputs.contains_key(name))
    {
        return Err("Task has inputs absent from the root Pipeline contract".into());
    }
    let mut boundary_outputs = definition.contract.inputs.clone();
    for port in boundary_outputs.values_mut() {
        port.root_default = None;
    }
    compiler.graph.nodes.insert(
        "root.inputs".into(),
        CompiledNode {
            operator: CompiledOperator::RootInputs,
            contract: PipelineContractV1 {
                inputs: BTreeMap::new(),
                outputs: boundary_outputs,
            },
            inputs: BTreeMap::new(),
            conditions: Vec::new(),
        },
    );
    let (outputs, coverage) = compiler.expand(root, "root", &root_inputs, &BTreeMap::new(), &[])?;
    if context
        .slot_workers
        .keys()
        .any(|slot| !compiler.graph.slots.contains_key(slot))
    {
        return Err("Local Worker binding names an unknown physical slot".into());
    }
    for (name, required) in &task.required_outputs {
        let address = outputs
            .get(name)
            .ok_or_else(|| format!("Pipeline lacks required output {name}"))?;
        let produced = compiler.port(address)?;
        if produced.optional
            || !compiler.available(address, &[])
            || produced.artifact_type != required.artifact_type
            || produced.cardinality != required.cardinality
        {
            return Err(format!(
                "Pipeline output {name} cannot satisfy the Task contract"
            ));
        }
    }
    for (name, obligation) in &task.acceptance {
        let address = coverage
            .get(name)
            .ok_or_else(|| format!("Pipeline lacks acceptance coverage {name}"))?;
        let produced = compiler.port(address)?;
        if produced.optional
            || !compiler.available(address, &[])
            || produced.artifact_type != obligation.evidence_type
            || !compiler
                .evidence
                .get(address)
                .is_some_and(|ids| ids.contains(&obligation.verifier_policy))
        {
            return Err(format!("Coverage {name} lacks a trusted evidence producer"));
        }
        let target_name = context
            .acceptance_outputs
            .get(name)
            .ok_or_else(|| format!("Task-kind policy has no final output for {name}"))?;
        let target = outputs
            .get(target_name)
            .ok_or_else(|| format!("Task-kind acceptance target {target_name} is unavailable"))?;
        if !task.required_outputs.contains_key(target_name)
            || compiler.lineage.get(address) != compiler.lineage.get(target)
        {
            return Err(format!(
                "Coverage {name} does not judge the required final output {target_name}"
            ));
        }
    }
    compiler.graph.outputs = outputs;
    compiler.graph.coverage = coverage;
    compiler.graph.order = compiler.graph.scheduler_plan()?.order;
    compiler.graph.budget(task.limits.clone())?;
    Ok(compiler.graph)
}

type Boundary = (BTreeMap<String, Address>, BTreeMap<String, Address>);

impl Compiler<'_> {
    fn select(
        &mut self,
        qualified: &str,
        bound: &BTreeMap<String, Address>,
        conditions: &[CompiledCondition],
    ) -> Result<BTreeMap<String, Address>, String> {
        if !bound
            .keys()
            .map(String::as_str)
            .eq(["condition", "failed", "inconclusive", "passed"])
        {
            return Err("Select requires condition, passed, failed and inconclusive inputs".into());
        }
        let condition = &bound["condition"];
        if !self.outcomes.contains(condition) || !self.available(condition, conditions) {
            return Err("Select condition is not an available trusted receipt".into());
        }
        let first = self.port(&bound["passed"])?;
        let mut output = first.clone();
        output.affinity = PortAffinityV1::Unbound {};
        output.optional = false;
        output.root_default = None;
        output.covers.clear();
        let mut input_ports = BTreeMap::from([("condition".into(), self.port(condition)?.clone())]);
        input_ports
            .get_mut("condition")
            .expect("condition")
            .affinity = PortAffinityV1::Unbound {};
        let mut lineages = Vec::new();
        let mut evidence: Option<BTreeSet<String>> = None;
        let mut retained: Option<BTreeSet<Address>> = None;
        for outcome in [
            ReceiptOutcomeV1::Passed,
            ReceiptOutcomeV1::Failed,
            ReceiptOutcomeV1::Inconclusive,
        ] {
            let arm = outcome_name(outcome);
            let address = &bound[arm];
            let mut path = conditions.to_vec();
            path.push(CompiledCondition {
                source: condition.clone(),
                outcome,
            });
            if !self.available(address, &path) {
                return Err(format!("Select {arm} value is unavailable on that path"));
            }
            self.compatible(address, &output)?;
            let mut input = output.clone();
            // Physical ports permit the two inactive arms. The compiler proved the selected
            // arm required, and select_output checks its actual admitted value at runtime.
            input.optional = true;
            input_ports.insert(arm.into(), input);
            lineages.push(
                self.lineage
                    .get(address)
                    .cloned()
                    .ok_or("Missing Select lineage")?,
            );
            let policies = self.evidence.get(address).cloned().unwrap_or_default();
            evidence = Some(match evidence {
                None => policies,
                Some(previous) => previous.intersection(&policies).cloned().collect(),
            });
            let receipts = self
                .retained
                .get(address)
                .cloned()
                .unwrap_or_else(|| BTreeSet::from([address.clone()]));
            retained = Some(match retained {
                None => receipts,
                Some(previous) => previous.intersection(&receipts).cloned().collect(),
            });
        }
        if self.graph.nodes.len() > self.context.max_nodes {
            return Err("Expanded Task exceeds the node limit".into());
        }
        self.graph.nodes.insert(
            qualified.into(),
            CompiledNode {
                operator: CompiledOperator::Select,
                contract: PipelineContractV1 {
                    inputs: input_ports,
                    outputs: BTreeMap::from([("output".into(), output)]),
                },
                inputs: bound.clone(),
                conditions: conditions.to_vec(),
            },
        );
        let address = Address {
            node: qualified.into(),
            port: "output".into(),
        };
        let lineage = if lineages.iter().all(|lineage| lineage == &lineages[0]) {
            lineages[0].clone()
        } else {
            Lineage::Choice(
                condition.clone(),
                Box::new(lineages.try_into().expect("three arms")),
            )
        };
        self.lineage.insert(address.clone(), lineage);
        self.availability
            .insert(address.clone(), conditions.to_vec());
        self.evidence
            .insert(address.clone(), evidence.unwrap_or_default());
        let mut retained = retained.unwrap_or_default();
        retained.insert(address.clone());
        self.retained.insert(address.clone(), retained);
        if ["passed", "failed", "inconclusive"]
            .iter()
            .all(|arm| self.outcomes.contains(&bound[*arm]))
        {
            self.outcomes.insert(address.clone());
        }
        Ok(BTreeMap::from([("output".into(), address)]))
    }

    fn available(&self, address: &Address, conditions: &[CompiledCondition]) -> bool {
        self.availability.get(address).is_none_or(|required| {
            required
                .iter()
                .all(|condition| conditions.contains(condition))
        })
    }

    fn port(&self, address: &Address) -> Result<&PipelinePortV1, String> {
        self.graph
            .nodes
            .get(&address.node)
            .and_then(|n| n.contract.outputs.get(&address.port))
            .ok_or_else(|| format!("Unknown producer {}", address.qualified()))
    }

    fn compatible(&self, source: &Address, target: &PipelinePortV1) -> Result<(), String> {
        let produced = self.port(source)?;
        if produced.artifact_type != target.artifact_type
            || produced.cardinality != target.cardinality
            || (produced.optional && !target.optional)
        {
            return Err(format!("Incompatible producer {}", source.qualified()));
        }
        Ok(())
    }

    fn affinity(
        &self,
        address: &Address,
        affinity: &PortAffinityV1,
        inputs: &BTreeMap<String, Address>,
    ) -> Result<(), String> {
        let (input, derived) = match affinity {
            PortAffinityV1::Unbound {} => return Ok(()),
            PortAffinityV1::SameAs { input } => (input, false),
            PortAffinityV1::DerivedFrom { input } => (input, true),
        };
        let source = inputs
            .get(input)
            .and_then(|p| self.lineage.get(p))
            .ok_or_else(|| format!("Affinity input {input} is unavailable"))?;
        let lineage = self
            .lineage
            .get(address)
            .ok_or("Producer has no Snapshot lineage")?;
        if (!derived && lineage == source) || (derived && lineage.derived_from(source)) {
            Ok(())
        } else {
            Err(format!(
                "Producer {} violates Snapshot lineage",
                address.qualified()
            ))
        }
    }

    fn expand(
        &mut self,
        name: &str,
        scope: &str,
        inputs: &BTreeMap<String, Address>,
        slot_mapping: &BTreeMap<String, String>,
        inherited: &[CompiledCondition],
    ) -> Result<Boundary, String> {
        if self.stack.len() >= self.context.max_depth || self.stack.iter().any(|n| n == name) {
            return Err(format!(
                "Recursive Pipeline or expansion depth exceeded at {name}"
            ));
        }
        let definition = self
            .context
            .pipelines
            .get(name)
            .ok_or_else(|| format!("Pipeline {name} is not installed"))?
            .clone();
        definition.validate()?;
        self.stack.push(name.into());
        for (port_name, port) in &definition.contract.inputs {
            match inputs.get(port_name) {
                Some(source) => {
                    self.compatible(source, port)?;
                    self.affinity(source, &port.affinity, inputs)?;
                    if !port.optional && !self.available(source, inherited) {
                        return Err(format!(
                            "{scope} requires a conditionally unavailable input"
                        ));
                    }
                }
                None if port.optional => (),
                None => return Err(format!("{scope} lacks required input {port_name}")),
            }
        }
        if inputs
            .keys()
            .any(|p| !definition.contract.inputs.contains_key(p))
        {
            return Err(format!("{scope} binds an undeclared child input"));
        }
        let mut slots = BTreeMap::new();
        for (local, slot) in &definition.slots {
            let qualified = slot_mapping
                .get(local)
                .cloned()
                .unwrap_or_else(|| format!("{scope}.slots.{local}"));
            if let Some(parent) = self.graph.slots.get(&qualified) {
                if parent.role != slot.role
                    || parent.input_type != slot.input_type
                    || parent.output_type != slot.output_type
                    || parent.min_attempts < slot.min_attempts
                    || parent.max_attempts > slot.max_attempts
                {
                    return Err(format!(
                        "Child slot {local} has an incompatible parent binding"
                    ));
                }
                if parent.worker != slot.worker && !slot.allow_local_replacement {
                    return Err(format!(
                        "Child slot {local} forbids replacing its Worker package"
                    ));
                }
            } else {
                let mut resolved = slot.clone();
                if let Some(worker) = self.context.slot_workers.get(&qualified) {
                    resolved.worker = worker.clone();
                }
                resolved.independent_from.clear();
                self.graph.slots.insert(qualified.clone(), resolved);
            }
            let effective = &self.graph.slots[&qualified].worker;
            if effective != &slot.worker {
                if !slot.allow_local_replacement {
                    return Err(format!("Slot {qualified} forbids Worker replacement"));
                }
                let original = self
                    .context
                    .signatures
                    .get(&format!("worker/{}", slot.worker))
                    .ok_or("Default Worker signature is missing")?;
                let replacement = self
                    .context
                    .signatures
                    .get(&format!("worker/{effective}"))
                    .ok_or("Replacement Worker signature is missing")?;
                if original.contract != replacement.contract
                    || original.worker_input_type != replacement.worker_input_type
                    || original.worker_output_type != replacement.worker_output_type
                    || original.outcome_port != replacement.outcome_port
                    || !replacement.roles.contains(&slot.role)
                    || !replacement.effects.is_subset(&original.effects)
                    || original.evidence.iter().any(|(port, policies)| {
                        !policies
                            .is_subset(replacement.evidence.get(port).unwrap_or(&BTreeSet::new()))
                    })
                    || original.retains.iter().any(|(port, inputs)| {
                        !inputs.is_subset(replacement.retains.get(port).unwrap_or(&BTreeSet::new()))
                    })
                {
                    return Err(format!(
                        "Replacement Worker violates slot {qualified}'s public contract or authority"
                    ));
                }
                self.graph
                    .replaced_workers
                    .entry(qualified.clone())
                    .or_default()
                    .insert(slot.worker.clone());
            }
            slots.insert(local.clone(), qualified);
        }
        if slot_mapping
            .keys()
            .any(|s| !definition.slots.contains_key(s))
        {
            return Err(format!("{scope} maps an undeclared child slot"));
        }
        for (local, slot) in &definition.slots {
            for other in &slot.independent_from {
                if slots[local] == slots[other] {
                    return Err(format!(
                        "{scope} maps independent slots to the same Worker slot"
                    ));
                }
                self.graph
                    .slots
                    .get_mut(&slots[local])
                    .expect("resolved slot")
                    .independent_from
                    .insert(slots[other].clone());
            }
        }
        let mut values: BTreeMap<(String, String), Address> = BTreeMap::new();
        let mut pending: BTreeMap<_, _> = definition
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.clone()))
            .collect();
        while !pending.is_empty() {
            let mut progressed = false;
            for local in pending.keys().cloned().collect::<Vec<_>>() {
                let node = &pending[&local];
                if node
                    .when
                    .as_ref()
                    .is_some_and(|condition| pending.contains_key(&condition.node))
                    || node.inputs.values().any(|v| match v {
                        ValueRefV1::Input { .. } => false,
                        ValueRefV1::Node { node, .. } => pending.contains_key(node),
                    })
                {
                    continue;
                }
                let mut conditions = inherited.to_vec();
                if let Some(condition) = &node.when {
                    let receipts: BTreeSet<_> = values
                        .iter()
                        .filter(|((n, _), address)| {
                            n == &condition.node && self.outcomes.contains(*address)
                        })
                        .map(|(_, address)| address.clone())
                        .collect();
                    if receipts.len() != 1 {
                        return Err(format!(
                            "{}.{} condition requires one trusted outcome port",
                            scope, node.id
                        ));
                    }
                    let source = receipts.into_iter().next().expect("one receipt");
                    if !self.available(&source, inherited) {
                        return Err(
                            "Branch condition is not available on every enclosing path".into()
                        );
                    }
                    let compiled = CompiledCondition {
                        source,
                        outcome: condition.outcome,
                    };
                    if conditions
                        .iter()
                        .any(|old| old.source == compiled.source && old.outcome != compiled.outcome)
                    {
                        return Err("Branch condition contradicts its enclosing path".into());
                    }
                    if !conditions.contains(&compiled) {
                        conditions.push(compiled);
                    }
                }
                let mut bound = BTreeMap::new();
                for (port, reference) in &node.inputs {
                    match resolve(reference, inputs, &values) {
                        Some(address) => {
                            bound.insert(port.clone(), address);
                        }
                        None if matches!(reference, ValueRefV1::Input {port} if definition.contract.inputs[port].optional) =>
                            {}
                        None => {
                            return Err(format!("{}.{} has an unavailable input", scope, node.id));
                        }
                    }
                }
                let qualified = format!("{scope}.nodes.{}", node.id);
                let outputs = match &node.operator {
                    TaskOperatorV1::Call { pipeline, bindings } => {
                        let mapped = bindings
                            .iter()
                            .map(|(child, parent)| {
                                slots
                                    .get(parent)
                                    .cloned()
                                    .map(|p| (child.clone(), p))
                                    .ok_or("Unknown parent slot")
                            })
                            .collect::<Result<BTreeMap<_, _>, _>>()?;
                        let (outputs, _) =
                            self.expand(pipeline, &qualified, &bound, &mapped, &conditions)?;
                        outputs
                    }
                    TaskOperatorV1::Select {} => self.select(&qualified, &bound, &conditions)?,
                    operator => {
                        let (signature_name, operator) = match operator {
                            TaskOperatorV1::Worker { slot }
                            | TaskOperatorV1::Verify { slot }
                            | TaskOperatorV1::FixVerify { slot } => {
                                let effective = &slots[slot];
                                let declaration = &self.graph.slots[effective];
                                let key = format!("worker/{}", declaration.worker);
                                let signature = self
                                    .context
                                    .signatures
                                    .get(&key)
                                    .ok_or("Worker has no trusted signature")?;
                                if !signature.roles.contains(&declaration.role)
                                    || signature.worker_input_type.as_ref()
                                        != Some(&declaration.input_type)
                                    || signature.worker_output_type.as_ref()
                                        != Some(&declaration.output_type)
                                {
                                    return Err(format!(
                                        "Worker {} does not implement slot {slot}",
                                        declaration.worker
                                    ));
                                }
                                let operator = match operator {
                                    TaskOperatorV1::Worker { .. } => TaskOperatorV1::Worker {
                                        slot: effective.clone(),
                                    },
                                    TaskOperatorV1::Verify { .. } => TaskOperatorV1::Verify {
                                        slot: effective.clone(),
                                    },
                                    _ => TaskOperatorV1::FixVerify {
                                        slot: effective.clone(),
                                    },
                                };
                                (key, operator)
                            }
                            other => (format!("operator/{}", operator_name(other)?), other.clone()),
                        };
                        let signature = self
                            .context
                            .signatures
                            .get(&signature_name)
                            .ok_or_else(|| format!("Unsupported {signature_name}"))?;
                        signature.contract.validate()?;
                        if signature.retains.iter().any(|(output, inputs)| {
                            !signature.contract.outputs.contains_key(output)
                                || inputs
                                    .iter()
                                    .any(|input| !signature.contract.inputs.contains_key(input))
                        }) {
                            return Err(
                                "Operator retention signature names an undeclared port".into()
                            );
                        }
                        if signature
                            .contract
                            .inputs
                            .keys()
                            .any(|name| name.starts_with("af_condition_"))
                        {
                            return Err("Operator input uses a reserved condition port".into());
                        }
                        if let Some(port) = &signature.outcome_port {
                            let receipt = signature
                                .contract
                                .outputs
                                .get(port)
                                .ok_or("Unknown outcome port")?;
                            if receipt.optional
                                || receipt.cardinality != review_core::PortCardinality::One
                            {
                                return Err("A branch outcome must be one required receipt".into());
                            }
                        }
                        if !signature
                            .effects
                            .is_subset(&self.task.authority.allowed_effects)
                        {
                            return Err(format!("Task authority does not permit {signature_name}"));
                        }
                        for (port_name, port) in &signature.contract.inputs {
                            match bound.get(port_name) {
                                Some(source) => {
                                    self.compatible(source, port)?;
                                    self.affinity(source, &port.affinity, &bound)?;
                                    if !port.optional && !self.available(source, &conditions) {
                                        return Err(format!(
                                            "{qualified} requires an unavailable branch output"
                                        ));
                                    }
                                }
                                None if port.optional => (),
                                None => return Err(format!("{qualified} lacks {port_name}")),
                            }
                        }
                        if bound
                            .keys()
                            .any(|p| !signature.contract.inputs.contains_key(p))
                        {
                            return Err(format!("{qualified} binds an unknown operator input"));
                        }
                        if matches!(operator, TaskOperatorV1::ReviewReduce {})
                            && !bound.keys().eq(signature.contract.inputs.keys())
                        {
                            return Err(format!(
                                "{qualified} must bind every configured reviewer and check; missing runtime results remain typed incomplete evidence"
                            ));
                        }
                        if matches!(operator, TaskOperatorV1::ReviewAccept {})
                            && !self.graph.calls.values().any(|call| {
                                call.coverage.get("reviewed") == bound.get("review")
                                    && call
                                        .outputs
                                        .values()
                                        .any(|address| Some(address) == bound.get("review"))
                            })
                        {
                            return Err("Implementation requires a child Pipeline's public reviewed coverage".into());
                        }
                        if self.graph.nodes.len() > self.context.max_nodes {
                            return Err("Expanded Task exceeds the node limit".into());
                        }
                        let paid = matches!(
                            operator,
                            TaskOperatorV1::Worker { .. }
                                | TaskOperatorV1::Verify { .. }
                                | TaskOperatorV1::FixVerify { .. }
                                | TaskOperatorV1::Check { .. }
                        );
                        if paid && signature.attempt.is_none() {
                            return Err(format!("{signature_name} has no bounded Attempt cost"));
                        }
                        if let TaskOperatorV1::Check { checks } = &operator {
                            for check in checks {
                                if self
                                    .context
                                    .signatures
                                    .get(&format!("operator/check/{check}"))
                                    != Some(signature)
                                {
                                    return Err(format!(
                                        "Check {check} is not installed under the captured check policy"
                                    ));
                                }
                            }
                        }
                        if let Some(cost) = &signature.attempt {
                            let slot = match &operator {
                                TaskOperatorV1::Worker { slot }
                                | TaskOperatorV1::Verify { slot }
                                | TaskOperatorV1::FixVerify { slot } => self.graph.slots.get(slot),
                                _ => None,
                            };
                            self.graph.allowances.insert(
                                qualified.clone(),
                                NodeAllowance {
                                    tokens_per_attempt: cost.tokens,
                                    wall_ms_per_attempt: cost.wall_ms,
                                    max_attempts: slot.map_or(1, |s| s.max_attempts),
                                    verification_attempts: if matches!(
                                        operator,
                                        TaskOperatorV1::Verify { .. }
                                            | TaskOperatorV1::FixVerify { .. }
                                            | TaskOperatorV1::Check { .. }
                                    ) || signature
                                        .evidence
                                        .values()
                                        .any(|policies| !policies.is_empty())
                                    {
                                        slot.map_or(1, |s| s.min_attempts.max(1))
                                    } else {
                                        0
                                    },
                                },
                            );
                        }
                        self.graph.nodes.insert(
                            qualified.clone(),
                            CompiledNode {
                                operator: CompiledOperator::Primitive {
                                    operator,
                                    signature: signature_name,
                                },
                                contract: signature.contract.clone(),
                                inputs: bound.clone(),
                                conditions: conditions.clone(),
                            },
                        );
                        let mut outputs = BTreeMap::new();
                        for (port_name, port) in &signature.contract.outputs {
                            let address = Address {
                                node: qualified.clone(),
                                port: port_name.clone(),
                            };
                            let lineage = match &port.affinity {
                                PortAffinityV1::Unbound {} => Lineage::Root(address.qualified()),
                                PortAffinityV1::SameAs { input } => self
                                    .lineage
                                    .get(bound.get(input).ok_or("Missing lineage input")?)
                                    .cloned()
                                    .ok_or("Missing lineage")?,
                                PortAffinityV1::DerivedFrom { input } => Lineage::Derived(
                                    address.clone(),
                                    Box::new(
                                        self.lineage
                                            .get(bound.get(input).ok_or("Missing lineage input")?)
                                            .cloned()
                                            .ok_or("Missing lineage")?,
                                    ),
                                ),
                            };
                            let mut retained = BTreeSet::from([address.clone()]);
                            let mut evidence = signature
                                .evidence
                                .get(port_name)
                                .cloned()
                                .unwrap_or_default();
                            if let Some(inputs) = signature.retains.get(port_name) {
                                for input in inputs {
                                    let Some(source) = bound.get(input) else {
                                        continue;
                                    };
                                    retained.insert(source.clone());
                                    if let Some(ancestors) = self.retained.get(source) {
                                        retained.extend(ancestors.iter().cloned());
                                    }
                                    if self.lineage.get(source) == Some(&lineage) {
                                        evidence.extend(
                                            self.evidence
                                                .get(source)
                                                .into_iter()
                                                .flatten()
                                                .cloned(),
                                        );
                                    }
                                }
                            }
                            self.lineage.insert(address.clone(), lineage);
                            self.retained.insert(address.clone(), retained);
                            self.availability
                                .insert(address.clone(), conditions.clone());
                            if signature.outcome_port.as_ref() == Some(port_name) {
                                self.outcomes.insert(address.clone());
                            }
                            self.evidence.insert(address.clone(), evidence);
                            outputs.insert(port_name.clone(), address);
                        }
                        outputs
                    }
                };
                for (port, address) in outputs {
                    values.insert((local.clone(), port), address);
                }
                pending.remove(&local);
                progressed = true;
            }
            if !progressed {
                return Err(format!("Pipeline {name} contains a dependency cycle"));
            }
        }
        let mut outputs = BTreeMap::new();
        let mut coverage = BTreeMap::new();
        for (public, reference) in &definition.outputs {
            let address = resolve(reference, inputs, &values).ok_or("Unavailable public output")?;
            let contract = &definition.contract.outputs[public];
            self.compatible(&address, contract)?;
            self.affinity(&address, &contract.affinity, inputs)?;
            if !contract.optional && !self.available(&address, inherited) {
                return Err(format!(
                    "Required public output {public} is unavailable on some paths"
                ));
            }
            for obligation in &contract.covers {
                let producer = resolve(&definition.coverage[obligation], inputs, &values)
                    .ok_or("Unavailable evidence output")?;
                if producer != address
                    && !self
                        .retained
                        .get(&address)
                        .is_some_and(|retained| retained.contains(&producer))
                {
                    return Err(format!(
                        "Public output {public} does not retain coverage {obligation}"
                    ));
                }
                if self.evidence.get(&producer).is_none_or(BTreeSet::is_empty) {
                    return Err(format!("Coverage {obligation} has no trusted producer"));
                }
                coverage.insert(obligation.clone(), producer);
            }
            outputs.insert(public.clone(), address);
        }
        self.graph.calls.insert(
            scope.into(),
            CompiledCall {
                pipeline: name.into(),
                inputs: inputs.clone(),
                outputs: outputs.clone(),
                coverage: coverage.clone(),
                max_attempts: definition.max_attempts,
                max_parallel: definition.max_parallel,
            },
        );
        self.stack.pop();
        Ok((outputs, coverage))
    }
}

fn resolve(
    reference: &ValueRefV1,
    inputs: &BTreeMap<String, Address>,
    values: &BTreeMap<(String, String), Address>,
) -> Option<Address> {
    match reference {
        ValueRefV1::Input { port } => inputs.get(port).cloned(),
        ValueRefV1::Node { node, port } => values.get(&(node.clone(), port.clone())).cloned(),
    }
}

fn operator_name(operator: &TaskOperatorV1) -> Result<&'static str, String> {
    match operator {
        TaskOperatorV1::Seal {} => Ok("seal"),
        TaskOperatorV1::Accept {} => Ok("accept"),
        TaskOperatorV1::Check { .. } => Ok("check"),
        TaskOperatorV1::ReviewBind {} => Ok("review-bind"),
        TaskOperatorV1::ReviewReduce {} => Ok("review-reduce"),
        TaskOperatorV1::ReviewAccept {} => Ok("review-accept"),
        TaskOperatorV1::AttestFixes {} => Ok("attest-fixes"),
        _ => Err("Operator requires package expansion".into()),
    }
}

pub fn condition_input(index: usize) -> String {
    format!("af_condition_{index}")
}

fn outcome_name(outcome: ReceiptOutcomeV1) -> &'static str {
    match outcome {
        ReceiptOutcomeV1::Passed => "passed",
        ReceiptOutcomeV1::Failed => "failed",
        ReceiptOutcomeV1::Inconclusive => "inconclusive",
    }
}
