//! Reusable Pipeline definition contracts, independent of parser and scheduler implementations.

use super::{REVIEW_HISTORY_V1, TaskFactV1, is_name, is_package_name, require};
use crate::{PortCardinality, is_artifact_type};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelineSchemaV1 {
    #[serde(rename = "af.pipeline/1")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PortAffinityV1 {
    Unbound {},
    SameAs { input: String },
    DerivedFrom { input: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootDefaultV1 {
    EmptyReviewHistory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelinePortV1 {
    pub artifact_type: String,
    pub cardinality: PortCardinality,
    pub optional: bool,
    pub affinity: PortAffinityV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub root_default: Option<RootDefaultV1>,
    #[serde(deserialize_with = "super::unique_set")]
    pub covers: BTreeSet<String>,
}

impl PipelinePortV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_artifact_type(&self.artifact_type),
            "Pipeline port requires a versioned artifact type",
        )?;
        match &self.affinity {
            PortAffinityV1::Unbound {} => {}
            PortAffinityV1::SameAs { input } | PortAffinityV1::DerivedFrom { input } => {
                require(is_name(input), "Invalid port affinity input")?;
            }
        }
        require(
            self.covers.iter().all(|s| is_name(s)),
            "Invalid public coverage name",
        )?;
        if self.root_default.is_some() {
            require(
                self.artifact_type == REVIEW_HISTORY_V1 && self.cardinality == PortCardinality::One,
                "Empty review history is only a default for a single ReviewHistory port",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineContractV1 {
    pub inputs: BTreeMap<String, PipelinePortV1>,
    pub outputs: BTreeMap<String, PipelinePortV1>,
}

impl PipelineContractV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            !self.outputs.is_empty(),
            "Pipeline must declare public outputs",
        )?;
        for (is_input, name, port) in self
            .inputs
            .iter()
            .map(|(n, p)| (true, n, p))
            .chain(self.outputs.iter().map(|(n, p)| (false, n, p)))
        {
            require(is_name(name), "Invalid Pipeline port name")?;
            port.validate()?;
            match &port.affinity {
                PortAffinityV1::SameAs { input } | PortAffinityV1::DerivedFrom { input } => {
                    require(
                        self.inputs.contains_key(input) && (!is_input || input != name),
                        "Port affinity must reference another declared public input",
                    )?;
                }
                PortAffinityV1::Unbound {} => {}
            }
        }
        require(
            self.outputs.values().all(|p| p.root_default.is_none()),
            "Public outputs cannot have root defaults",
        )?;
        require(
            self.inputs.values().all(|p| p.covers.is_empty()),
            "Only public outputs declare coverage",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValueRefV1 {
    Input { port: String },
    Node { node: String, port: String },
}

impl ValueRefV1 {
    pub fn validate(&self) -> Result<(), String> {
        let valid = match self {
            Self::Input { port } => is_name(port),
            Self::Node { node, port } => is_name(node) && is_name(port),
        };
        require(valid, "Invalid named Pipeline reference")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerSlotV1 {
    pub worker: String,
    pub role: String,
    pub input_type: String,
    pub output_type: String,
    pub min_attempts: u32,
    pub max_attempts: u32,
    pub allow_local_replacement: bool,
    #[serde(deserialize_with = "super::unique_set")]
    pub independent_from: BTreeSet<String>,
}

impl WorkerSlotV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_package_name(&self.worker) && is_name(&self.role),
            "Invalid Worker slot identity",
        )?;
        require(
            is_artifact_type(&self.input_type) && is_artifact_type(&self.output_type),
            "Worker slot needs typed input/output contracts",
        )?;
        require(
            self.max_attempts > 0 && self.min_attempts <= self.max_attempts,
            "Worker slot Attempt bounds are invalid",
        )?;
        require(
            self.independent_from.iter().all(|s| is_name(s)),
            "Invalid independent slot reference",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskOperatorV1 {
    Worker {
        slot: String,
    },
    Seal {},
    /// Kernel receipt assembly: preserves negative/inconclusive checks without invoking a
    /// conditional evaluator, and admits success only from current independent evidence.
    Accept {},
    Check {
        #[serde(deserialize_with = "super::unique_set")]
        checks: BTreeSet<String>,
    },
    Verify {
        slot: String,
    },
    Call {
        pipeline: String,
        bindings: BTreeMap<String, String>,
    },
    ReviewBind {},
    /// One atomic gather/reducer barrier over the captured set of required reviewers.
    ReviewReduce {},
    /// Accept implementation only from the embedded current-Snapshot Review and its checks.
    ReviewAccept {},
    AttestFixes {},
    FixVerify {
        slot: String,
    },
    Select {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcomeV1 {
    Passed,
    Failed,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConditionV1 {
    pub node: String,
    pub outcome: ReceiptOutcomeV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskNodeV1 {
    pub id: String,
    pub operator: TaskOperatorV1,
    pub inputs: BTreeMap<String, ValueRefV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub when: Option<NodeConditionV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineApplicabilityV1 {
    #[serde(deserialize_with = "super::unique_set")]
    pub kinds: BTreeSet<String>,
    pub required_facts: BTreeMap<String, TaskFactV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineDefinitionV1 {
    pub schema: PipelineSchemaV1,
    pub name: String,
    pub version: String,
    pub contract: PipelineContractV1,
    pub accepts: PipelineApplicabilityV1,
    pub slots: BTreeMap<String, WorkerSlotV1>,
    pub nodes: Vec<TaskNodeV1>,
    pub outputs: BTreeMap<String, ValueRefV1>,
    pub coverage: BTreeMap<String, ValueRefV1>,
    pub max_attempts: u32,
    pub max_parallel: u32,
}

impl PipelineDefinitionV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_package_name(&self.name) && !self.version.trim().is_empty(),
            "Invalid Pipeline name/version",
        )?;
        self.contract.validate()?;
        require(
            !self.nodes.is_empty() && self.nodes.len() <= 64,
            "Pipeline must contain one to 64 static nodes",
        )?;
        require(
            self.max_attempts > 0 && (1..=16).contains(&self.max_parallel),
            "Invalid Pipeline execution bounds",
        )?;
        require(
            !self.accepts.kinds.is_empty() && self.accepts.kinds.iter().all(|s| is_package_name(s)),
            "Invalid Pipeline root applicability",
        )?;
        for (name, fact) in &self.accepts.required_facts {
            require(is_name(name), "Invalid applicability fact")?;
            fact.validate()?;
        }
        require(
            self.outputs.keys().eq(self.contract.outputs.keys()),
            "Every public output must have exactly one binding",
        )?;
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            require(
                is_name(&node.id) && ids.insert(node.id.clone()),
                "Duplicate or invalid Pipeline node ID",
            )?;
            for (port, input) in &node.inputs {
                require(is_name(port), "Invalid operator input name")?;
                input.validate()?;
            }
            match &node.operator {
                TaskOperatorV1::Worker { slot }
                | TaskOperatorV1::Verify { slot }
                | TaskOperatorV1::FixVerify { slot } => {
                    require(
                        self.slots.contains_key(slot),
                        "Operator refers to an unknown Worker slot",
                    )?;
                }
                TaskOperatorV1::Check { checks } => require(
                    !checks.is_empty() && checks.iter().all(|s| is_name(s)),
                    "Check operator needs named trusted checks",
                )?,
                TaskOperatorV1::Call { pipeline, bindings } => {
                    require(
                        is_package_name(pipeline),
                        "Invalid child Pipeline reference",
                    )?;
                    require(
                        bindings.iter().all(|(child, parent)| {
                            is_name(child) && self.slots.contains_key(parent)
                        }),
                        "Invalid child Worker slot mapping",
                    )?;
                }
                _ => {}
            }
        }
        let mut reserved = 0u32;
        for (name, slot) in &self.slots {
            require(is_name(name), "Invalid Worker slot name")?;
            slot.validate()?;
            require(
                slot.independent_from
                    .iter()
                    .all(|other| other != name && self.slots.contains_key(other)),
                "Independence must reference another declared slot",
            )?;
            reserved = reserved
                .checked_add(slot.min_attempts)
                .ok_or("Attempt reservation overflow")?;
        }
        require(
            reserved <= self.max_attempts,
            "Pipeline cannot hold its reserved Worker Attempts",
        )?;
        for node in &self.nodes {
            if let Some(condition) = &node.when {
                require(
                    condition.node != node.id && ids.contains(&condition.node),
                    "Condition must refer to another declared receipt producer",
                )?;
            }
        }
        for reference in self
            .nodes
            .iter()
            .flat_map(|n| n.inputs.values())
            .chain(self.outputs.values())
            .chain(self.coverage.values())
        {
            reference.validate()?;
            require(
                match reference {
                    ValueRefV1::Input { port } => self.contract.inputs.contains_key(port),
                    ValueRefV1::Node { node, .. } => ids.contains(node),
                },
                "Pipeline reference has no declared producer",
            )?;
        }
        let declared: BTreeSet<_> = self
            .contract
            .outputs
            .values()
            .flat_map(|p| p.covers.iter().cloned())
            .collect();
        require(
            self.coverage.keys().all(|s| is_name(s))
                && declared.iter().all(|s| self.coverage.contains_key(s)),
            "Public coverage must have an internal evidence binding",
        )
    }
}
