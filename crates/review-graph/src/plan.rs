//! Building and validating a pipeline before anything runs.

use std::collections::{BTreeMap, BTreeSet};

/// A named typed port. Edges connect an upstream node's output port to a downstream input port,
/// so what a node receives is a property of the pipeline definition rather than of whatever was
/// lying around when it ran.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Port {
    pub node: String,
    pub name: String,
}

impl Port {
    pub fn new(node: impl Into<String>, name: impl Into<String>) -> Port {
        Port {
            node: node.into(),
            name: name.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: Port,
    pub to: Port,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Emits the run's generation state — the campaign's prior findings — as an artifact, so
    /// reviewers receive it through a wired input port rather than from ambient kernel state.
    Generation,
    /// Runs project checks and emits a gate decision.
    Gate,
    /// A model-backed or command-backed reviewer.
    Reviewer,
    /// Collects several upstream outputs at a barrier.
    Gather,
    /// Reduces reports into the ledger projection.
    Ledger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    /// Ports this node will accept input on. An edge to any other name is a planning error —
    /// a typo in a pipeline must not silently mean "this node gets nothing".
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// The gate whose pass is a precondition for dispatching this node. Transitive: a node
    /// downstream of a gated node is gated too.
    pub gated_by: Option<String>,
}

impl Node {
    pub fn new(id: impl Into<String>, kind: NodeKind) -> Node {
        Node {
            id: id.into(),
            kind,
            inputs: Vec::new(),
            outputs: vec!["out".to_string()],
            gated_by: None,
        }
    }

    pub fn accepting(mut self, ports: &[&str]) -> Node {
        self.inputs = ports.iter().map(|p| (*p).to_string()).collect();
        self
    }

    pub fn emitting(mut self, ports: &[&str]) -> Node {
        self.outputs = ports.iter().map(|p| (*p).to_string()).collect();
        self
    }

    pub fn gated_by(mut self, gate: impl Into<String>) -> Node {
        self.gated_by = Some(gate.into());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct Pipeline {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    DuplicateNode(String),
    UnknownNode {
        edge: String,
        node: String,
    },
    /// An edge naming a port the node does not declare.
    UnknownPort {
        edge: String,
        port: Port,
    },
    /// A node's declared input port with nothing wired to it. Not a warning: a reviewer running
    /// without the prior findings it expects produces a confident, wrong review.
    UnwiredInput(Port),
    Cycle(Vec<String>),
    UnknownGate {
        node: String,
        gate: String,
    },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::DuplicateNode(id) => write!(f, "duplicate node id: {id}"),
            PlanError::UnknownNode { edge, node } => {
                write!(f, "edge {edge} names an unknown node: {node}")
            }
            PlanError::UnknownPort { edge, port } => write!(
                f,
                "edge {edge} names port {}.{} which that node does not declare",
                port.node, port.name
            ),
            PlanError::UnwiredInput(port) => write!(
                f,
                "input {}.{} has nothing wired to it; a node that silently receives nothing \
                 produces a confident review of an empty input",
                port.node, port.name
            ),
            PlanError::Cycle(nodes) => write!(f, "cycle: {}", nodes.join(" -> ")),
            PlanError::UnknownGate { node, gate } => {
                write!(f, "node {node} is gated by unknown gate {gate}")
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// A validated pipeline with a deterministic execution order.
#[derive(Debug, Clone)]
pub struct Planned {
    pub nodes: BTreeMap<String, Node>,
    pub edges: Vec<Edge>,
    /// Nodes in dependency order. Ties broken by node ID, so the plan is a function of the
    /// pipeline alone — two planners on two machines produce the same order.
    pub order: Vec<String>,
}

impl Planned {
    pub fn dependencies_of(&self, node: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.to.node == node).collect()
    }

    /// Every gate this node depends on, directly or through an ancestor.
    pub fn gates_for(&self, node: &str) -> BTreeSet<String> {
        let mut gates = BTreeSet::new();
        let mut stack = vec![node.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(spec) = self.nodes.get(&current)
                && let Some(gate) = &spec.gated_by
            {
                gates.insert(gate.clone());
            }
            for edge in self.dependencies_of(&current) {
                stack.push(edge.from.node.clone());
            }
        }
        gates
    }
}

impl Pipeline {
    pub fn node(mut self, node: Node) -> Pipeline {
        self.nodes.push(node);
        self
    }

    pub fn edge(mut self, from: Port, to: Port) -> Pipeline {
        self.edges.push(Edge { from, to });
        self
    }

    /// Validate everything that can be known without running, and fix the execution order.
    pub fn plan(self) -> Result<Planned, PlanError> {
        let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
        for node in self.nodes {
            if nodes.contains_key(&node.id) {
                return Err(PlanError::DuplicateNode(node.id));
            }
            nodes.insert(node.id.clone(), node);
        }

        for edge in &self.edges {
            let label = format!(
                "{}.{} -> {}.{}",
                edge.from.node, edge.from.name, edge.to.node, edge.to.name
            );
            for port in [&edge.from, &edge.to] {
                let Some(spec) = nodes.get(&port.node) else {
                    return Err(PlanError::UnknownNode {
                        edge: label.clone(),
                        node: port.node.clone(),
                    });
                };
                let declared = if std::ptr::eq(port, &edge.from) {
                    &spec.outputs
                } else {
                    &spec.inputs
                };
                if !declared.contains(&port.name) {
                    return Err(PlanError::UnknownPort {
                        edge: label.clone(),
                        port: port.clone(),
                    });
                }
            }
        }

        // Every declared input must be fed. A node whose input silently defaults to nothing is
        // the failure mode this typing exists to remove.
        for node in nodes.values() {
            for input in &node.inputs {
                let wired = self
                    .edges
                    .iter()
                    .any(|e| e.to.node == node.id && &e.to.name == input);
                if !wired {
                    return Err(PlanError::UnwiredInput(Port::new(&node.id, input)));
                }
            }
            if let Some(gate) = &node.gated_by
                && !nodes.contains_key(gate)
            {
                return Err(PlanError::UnknownGate {
                    node: node.id.clone(),
                    gate: gate.clone(),
                });
            }
        }

        let order = topological_order(&nodes, &self.edges)?;
        Ok(Planned {
            nodes,
            edges: self.edges,
            order,
        })
    }
}

/// Kahn's algorithm with a deterministic tie-break: among ready nodes, the lowest ID first.
///
/// `gated_by` counts as an ordering dependency alongside the edges: a gate must resolve
/// before any node it gates is even considered, whether or not an edge also connects them —
/// and a gate that depends on its own gated node is a cycle, caught here rather than
/// deadlocking a run.
fn topological_order(
    nodes: &BTreeMap<String, Node>,
    edges: &[Edge],
) -> Result<Vec<String>, PlanError> {
    let mut incoming: BTreeMap<&str, BTreeSet<&str>> = nodes
        .keys()
        .map(|id| (id.as_str(), BTreeSet::new()))
        .collect();
    for edge in edges {
        incoming
            .entry(edge.to.node.as_str())
            .or_default()
            .insert(edge.from.node.as_str());
    }
    for node in nodes.values() {
        if let Some(gate) = &node.gated_by {
            incoming
                .entry(node.id.as_str())
                .or_default()
                .insert(gate.as_str());
        }
    }

    let mut order = Vec::with_capacity(nodes.len());
    let mut remaining = incoming.clone();
    while !remaining.is_empty() {
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(id, _)| *id)
            .collect();
        let Some(next) = ready.first().copied() else {
            let mut cycle: Vec<String> = remaining.keys().map(|id| (*id).to_string()).collect();
            cycle.sort();
            return Err(PlanError::Cycle(cycle));
        };
        order.push(next.to_string());
        remaining.remove(next);
        for deps in remaining.values_mut() {
            deps.remove(next);
        }
    }
    Ok(order)
}
