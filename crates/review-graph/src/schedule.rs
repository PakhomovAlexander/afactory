//! Executing a planned pipeline.
//!
//! Ready nodes run concurrently; results are admitted in canonical order. Gating is structural:
//! once a gate blocks, every node downstream of it is *suppressed* — recorded as such, never
//! dispatched, and never able to leave an artifact behind. The distinction matters because a
//! suppressed node and a node that ran and found nothing are the same shape in a report unless
//! the kernel keeps them apart.

use std::collections::{BTreeMap, BTreeSet};

use crate::plan::{Node, NodeKind, Planned};

/// Exact artifacts resolved per named port. Every declared input is present, including an
/// optional input that resolved to an empty vector.
pub type ArtifactMap = BTreeMap<String, Vec<String>>;

/// What the caller does when a node is dispatched. The scheduler owns *when* and *whether*, the
/// caller owns *what* — so scheduling can be tested without models, checks, or a filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedChildDispatch {
    pub node: Node,
    pub inputs: ArtifactMap,
}

pub trait Dispatch {
    /// Captured owners coordinate registered children without occupying a Worker slot.
    fn coordinates_owned_children(&self, _node: &Node) -> bool {
        false
    }

    /// Called after the owner's exact invocation is durable. The implementation must persist
    /// the complete bounded registration before returning any dispatchable child.
    fn expand_owned_children(
        &self,
        _node: &Node,
        _inputs: &ArtifactMap,
    ) -> Result<Vec<OwnedChildDispatch>, String> {
        Err("Owned-child expansion is not installed".into())
    }

    /// Every registered child is terminal, in registration order, including failures. The
    /// callback seals the pure parent result; the scheduler applies ordinary output admission.
    fn complete_owned_children(
        &self,
        _node: &Node,
        _inputs: &ArtifactMap,
        _children: &[(String, NodeOutcome)],
    ) -> Result<ArtifactMap, String> {
        Err("Owned-child completion is not installed".into())
    }

    /// A failed predecessor is different from a successful optional empty output. The
    /// installed Review frontend retains its complete predecessor barrier through Task
    /// execution; ordinary Task operators may handle optional missing inputs themselves.
    fn requires_successful_predecessors(&self, node: &Node) -> bool {
        node.kind != NodeKind::Task
    }

    /// Task-only branch decision over admitted, named inputs. This runs before invocation
    /// publication and may suppress an inactive branch without manufacturing a receipt.
    fn task_node_selected(&self, _node: &Node, _inputs: &ArtifactMap) -> Result<bool, String> {
        Ok(true)
    }

    /// Persist or otherwise observe the exact input selection before the node is scheduled.
    fn record_invocation(&self, _node: &Node, _inputs: &ArtifactMap) -> Result<(), String> {
        Ok(())
    }

    /// Run a node, given its exact artifacts grouped by input port.
    ///
    /// The whole `Node` is handed over, not an id: what a node *is* is its validated `kind`,
    /// and a dispatcher routing on the id string would silently misroute a reviewer that
    /// happens to be named `gather` — skipped, yet reported complete. Inputs are labelled with
    /// the port they arrived on so a node reads them by name rather than by position.
    ///
    /// Returning `Err` means the node ran and failed; it does not stop the pipeline, because a
    /// failed reviewer is a fact about the review, not a reason to lose the rest of it.
    fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String>;

    /// Seal the complete output map after the dispatcher succeeds and cardinality is validated.
    fn record_outputs(&self, _node: &Node, _outputs: &ArtifactMap) -> Result<(), String> {
        Ok(())
    }

    /// Whether this node's outputs constitute a passing gate. Only consulted for `Gate` nodes.
    fn gate_passed(&self, _node_id: &str, _outputs: &ArtifactMap) -> bool {
        true
    }

    /// Typed classification for a failed node. Human-readable errors remain diagnostics;
    /// policy must not recover a class by parsing them.
    fn failure_class(&self, _node_id: &str) -> Option<NodeFailureClass> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeFailureClass {
    RunBudgetExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionReason {
    /// A typed Task receipt selected another branch. No invocation or Attempt occurred.
    BranchNotSelected,
    /// A gate this node depends on did not pass.
    GateBlocked,
    /// An upstream node it depends on was itself suppressed or failed.
    UpstreamMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeOutcome {
    Completed {
        outputs: ArtifactMap,
    },
    Failed {
        error: String,
        class: Option<NodeFailureClass>,
    },
    Suppressed {
        reason: SuppressionReason,
    },
}

impl NodeOutcome {
    pub fn dispatched(&self) -> bool {
        !matches!(self, NodeOutcome::Suppressed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// Every node, in plan order, with what became of it. A suppressed node is present and
    /// labelled — absence would read as "nothing to report".
    pub outcomes: Vec<(String, NodeOutcome)>,
    pub blocked_gates: BTreeSet<String>,
}

impl RunReport {
    pub fn outcome(&self, node: &str) -> Option<&NodeOutcome> {
        self.outcomes
            .iter()
            .find(|(id, _)| id == node)
            .map(|(_, outcome)| outcome)
    }

    pub fn dispatched(&self) -> Vec<&str> {
        self.outcomes
            .iter()
            .filter(|(_, o)| o.dispatched())
            .map(|(id, _)| id.as_str())
            .collect()
    }

    pub fn suppressed(&self) -> Vec<&str> {
        self.outcomes
            .iter()
            .filter(|(_, o)| !o.dispatched())
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// A run is shippable only if nothing was suppressed and nothing failed. Suppression is not
    /// a neutral outcome: it means part of the review did not happen.
    pub fn complete(&self) -> bool {
        self.outcomes
            .iter()
            .all(|(_, o)| matches!(o, NodeOutcome::Completed { .. }))
    }
}

pub struct Scheduler<'a> {
    plan: &'a Planned,
    max_parallel: usize,
    scope_limits: BTreeMap<String, usize>,
}

pub const DEFAULT_MAX_PARALLEL: usize = 4;

impl<'a> Scheduler<'a> {
    pub fn new(plan: &'a Planned) -> Scheduler<'a> {
        Scheduler {
            plan,
            // The design's default. Reviewers are model calls: minutes of latency each, no
            // local CPU — running them one after another priced a review at the *sum* of
            // model latencies.
            max_parallel: DEFAULT_MAX_PARALLEL,
            scope_limits: BTreeMap::new(),
        }
    }

    /// Bound on concurrently running nodes. `1` makes the run fully sequential.
    pub fn with_parallelism(mut self, max_parallel: usize) -> Scheduler<'a> {
        self.max_parallel = max_parallel.max(1);
        self
    }

    /// Additional bounds for flattened child Pipeline namespaces. All share this scheduler.
    pub fn with_scope_limits(mut self, limits: BTreeMap<String, usize>) -> Result<Self, String> {
        if limits
            .iter()
            .any(|(scope, limit)| *limit == 0 || !scope.split('.').all(review_core::task::is_name))
        {
            return Err("Invalid Pipeline scope concurrency bound".into());
        }
        self.scope_limits = limits;
        Ok(self)
    }

    /// Execute the plan.
    ///
    /// Ready nodes run concurrently, up to `max_parallel`. Determinism survives the
    /// concurrency because nothing about the *result* depends on completion order: a node is
    /// dispatched only once every gate and upstream node it depends on has resolved, its
    /// inputs are exactly what the edges deliver (sorted), suppression is a function of
    /// resolved upstream state alone, and the report lists nodes in plan order.
    pub fn run(&self, dispatch: &(dyn Dispatch + Sync)) -> RunReport {
        let mut outputs: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        let mut outcomes: BTreeMap<String, NodeOutcome> = BTreeMap::new();
        let mut blocked_gates: BTreeSet<String> = BTreeSet::new();
        let mut unusable: BTreeSet<String> = BTreeSet::new();
        let mut in_flight: BTreeSet<String> = BTreeSet::new();

        let mut nodes = self.plan.nodes.clone();
        let mut order = self.plan.order.clone();
        let mut child_inputs: BTreeMap<String, ArtifactMap> = BTreeMap::new();
        let mut owners: BTreeMap<String, (ArtifactMap, Vec<String>)> = BTreeMap::new();

        std::thread::scope(|scope| {
            type Completion = (String, Result<ArtifactMap, String>);
            let (tx, rx) = std::sync::mpsc::channel::<Completion>();

            loop {
                // Decide everything currently decidable, in plan order: suppress what a
                // blocked gate or a missing upstream has doomed, dispatch what is ready.
                let mut progressed = false;
                let mut coordinated = BTreeMap::new();
                // Newly registered children join the next scan, preserving deterministic order.
                for node_id in &order.clone() {
                    if outcomes.contains_key(node_id) || in_flight.contains(node_id) {
                        continue;
                    }
                    let node = nodes[node_id].clone();
                    let node = &node;
                    if let Some((inputs, children)) = owners.get(node_id) {
                        if children.iter().all(|id| outcomes.contains_key(id)) {
                            let terminal: Vec<_> = children
                                .iter()
                                .map(|id| (id.clone(), outcomes[id].clone()))
                                .collect();
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    dispatch.complete_owned_children(node, inputs, &terminal)
                                }))
                                .unwrap_or_else(|_| {
                                    Err(format!("owned completion panicked for node {}", node.id))
                                });
                            coordinated.insert(node_id.clone(), result);
                            progressed = true;
                        }
                        continue;
                    }
                    let is_child = child_inputs.contains_key(node_id);
                    let coordinates = !is_child && dispatch.coordinates_owned_children(node);

                    // Gating first: a blocked gate suppresses this node before any input is
                    // resolved, so a suppressed node cannot even observe its would-be inputs.
                    let gates = if is_child {
                        BTreeSet::new()
                    } else {
                        self.plan.gates_for(node_id)
                    };
                    if gates.iter().any(|gate| blocked_gates.contains(gate)) {
                        outcomes.insert(
                            node_id.clone(),
                            NodeOutcome::Suppressed {
                                reason: SuppressionReason::GateBlocked,
                            },
                        );
                        unusable.insert(node_id.clone());
                        progressed = true;
                        continue;
                    }

                    let dependencies = if is_child {
                        Vec::new()
                    } else {
                        self.plan.dependencies_of(node_id)
                    };
                    if dispatch.requires_successful_predecessors(node)
                        && dependencies
                            .iter()
                            .any(|edge| unusable.contains(&edge.from.node))
                    {
                        outcomes.insert(
                            node_id.clone(),
                            NodeOutcome::Suppressed {
                                reason: SuppressionReason::UpstreamMissing,
                            },
                        );
                        unusable.insert(node_id.clone());
                        progressed = true;
                        continue;
                    }

                    // Not ready: some gate or upstream is still running. The plan order is
                    // topological over edges *and* gating, so this always clears.
                    let resolved = |id: &str| outcomes.contains_key(id);
                    if !gates.iter().all(|gate| resolved(gate))
                        || !dependencies.iter().all(|edge| resolved(&edge.from.node))
                    {
                        continue;
                    }
                    if !coordinates && in_flight.len() >= self.max_parallel {
                        continue;
                    }
                    if !coordinates
                        && self.scope_limits.iter().any(|(scope, limit)| {
                            let contains = |id: &str| {
                                id.strip_prefix(scope)
                                    .is_some_and(|tail| tail.starts_with('.'))
                            };
                            contains(node_id)
                                && in_flight.iter().filter(|id| contains(id)).count() >= *limit
                        })
                    {
                        continue;
                    }

                    // Inputs are exactly what the edges resolved to, each labelled with the
                    // input port it arrived on — so a node reads its inputs by name (a reviewer
                    // takes `prior_findings`, not "whichever artifact happened to be first").
                    // Sorted, so the vector does not depend on edge declaration order.
                    let mut inputs: ArtifactMap =
                        child_inputs.get(node_id).cloned().unwrap_or_else(|| {
                            node.inputs
                                .iter()
                                .map(|port| (port.name.clone(), Vec::new()))
                                .collect()
                        });
                    for edge in &dependencies {
                        if let Some(artifacts) =
                            outputs.get(&(edge.from.node.clone(), edge.from.name.clone()))
                        {
                            inputs
                                .get_mut(&edge.to.name)
                                .expect("planned input port")
                                .extend(artifacts.iter().cloned());
                        }
                    }
                    if node.kind != NodeKind::Task {
                        for artifacts in inputs.values_mut() {
                            artifacts.sort();
                        }
                    } else {
                        match dispatch.task_node_selected(node, &inputs) {
                            Ok(false) => {
                                outcomes.insert(
                                    node_id.clone(),
                                    NodeOutcome::Suppressed {
                                        reason: SuppressionReason::BranchNotSelected,
                                    },
                                );
                                unusable.insert(node_id.clone());
                                progressed = true;
                                continue;
                            }
                            Err(error) => {
                                outcomes.insert(
                                    node_id.clone(),
                                    NodeOutcome::Failed {
                                        error,
                                        class: dispatch.failure_class(node_id),
                                    },
                                );
                                unusable.insert(node_id.clone());
                                progressed = true;
                                continue;
                            }
                            Ok(true) => (),
                        }
                        if node.inputs.iter().any(|port| {
                            let count = inputs[&port.name].len();
                            (!port.optional && count == 0)
                                || (port.cardinality == review_core::PortCardinality::One
                                    && count > 1)
                        }) {
                            outcomes.insert(
                                node_id.clone(),
                                NodeOutcome::Suppressed {
                                    reason: SuppressionReason::UpstreamMissing,
                                },
                            );
                            unusable.insert(node_id.clone());
                            progressed = true;
                            continue;
                        }
                    }

                    if let Err(error) = dispatch.record_invocation(node, &inputs) {
                        unusable.insert(node_id.clone());
                        if node.kind == NodeKind::Gate {
                            blocked_gates.insert(node_id.clone());
                        }
                        outcomes.insert(
                            node_id.clone(),
                            NodeOutcome::Failed {
                                error,
                                class: dispatch.failure_class(node_id),
                            },
                        );
                        progressed = true;
                        continue;
                    }

                    if coordinates {
                        let expansion =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                dispatch.expand_owned_children(node, &inputs)
                            }))
                            .unwrap_or_else(|_| {
                                Err(format!("owned expansion panicked for node {}", node.id))
                            });
                        match expansion.and_then(|children| {
                            validate_owned_expansion(node, &children, &nodes)?;
                            Ok(children)
                        }) {
                            Ok(children) => {
                                let ids: Vec<_> =
                                    children.iter().map(|child| child.node.id.clone()).collect();
                                let index = order
                                    .iter()
                                    .position(|id| id == node_id)
                                    .expect("scheduled owner")
                                    + 1;
                                order.splice(index..index, ids.clone());
                                for child in children {
                                    child_inputs.insert(child.node.id.clone(), child.inputs);
                                    nodes.insert(child.node.id.clone(), child.node);
                                }
                                owners.insert(node_id.clone(), (inputs, ids));
                            }
                            Err(error) => {
                                coordinated.insert(node_id.clone(), Err(error));
                            }
                        }
                        progressed = true;
                        continue;
                    }
                    in_flight.insert(node_id.clone());
                    let tx = tx.clone();
                    let node = node.clone();
                    scope.spawn(move || {
                        // A panicking dispatcher is a failed node, not a hung run: without
                        // this, its completion never arrives and the loop waits forever.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            dispatch.run(&node, &inputs)
                        }))
                        .unwrap_or_else(|_| Err(format!("dispatch panicked for node {}", node.id)));
                        let _ = tx.send((node.id.clone(), result));
                    });
                    progressed = true;
                }

                if in_flight.is_empty() && coordinated.is_empty() {
                    if progressed {
                        // Suppressions may cascade; scan again before concluding.
                        continue;
                    }
                    break;
                }

                // Complete a whole dispatch wave before admitting any result. Workers retain
                // full concurrency, while publication and dependent dispatch are canonical in
                // plan order rather than functions of thread completion timing.
                let mut wave = in_flight.clone();
                wave.extend(coordinated.keys().cloned());
                let mut completions = coordinated;
                for _ in 0..in_flight.len() {
                    let (node_id, result) = rx.recv().expect("a running node reports its outcome");
                    in_flight.remove(&node_id);
                    completions.insert(node_id, result);
                }
                for node_id in order.iter().filter(|id| wave.contains(*id)) {
                    let result = completions
                        .remove(node_id)
                        .expect("every wave member completed");
                    let node = &nodes[node_id];
                    match result {
                        Ok(produced) => {
                            if let Err(error) = validate_outputs(node, &produced) {
                                unusable.insert(node_id.clone());
                                if node.kind == NodeKind::Gate {
                                    blocked_gates.insert(node_id.clone());
                                }
                                outcomes.insert(
                                    node_id.clone(),
                                    NodeOutcome::Failed { error, class: None },
                                );
                                continue;
                            }
                            if let Err(error) = dispatch.record_outputs(node, &produced) {
                                unusable.insert(node_id.clone());
                                if node.kind == NodeKind::Gate {
                                    blocked_gates.insert(node_id.clone());
                                }
                                outcomes.insert(
                                    node_id.clone(),
                                    NodeOutcome::Failed { error, class: None },
                                );
                                continue;
                            }
                            for (port, artifacts) in &produced {
                                outputs.insert((node_id.clone(), port.clone()), artifacts.clone());
                            }
                            if node.kind == NodeKind::Gate
                                && !dispatch.gate_passed(node_id, &produced)
                            {
                                blocked_gates.insert(node_id.clone());
                            }
                            outcomes.insert(
                                node_id.clone(),
                                NodeOutcome::Completed { outputs: produced },
                            );
                        }
                        Err(error) => {
                            // A failed node's dependents cannot run — they would be reviewing an
                            // input that does not exist — but the rest of the graph continues.
                            unusable.insert(node_id.clone());
                            if node.kind == NodeKind::Gate {
                                blocked_gates.insert(node_id.clone());
                            }
                            outcomes.insert(
                                node_id.clone(),
                                NodeOutcome::Failed {
                                    error,
                                    class: dispatch.failure_class(node_id),
                                },
                            );
                        }
                    }
                }
            }
        });

        RunReport {
            // Plan order, not completion order: the report is a function of the pipeline.
            outcomes: self
                .plan
                .order
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        outcomes.remove(id).expect("every node resolved"),
                    )
                })
                .collect(),
            blocked_gates,
        }
    }
}

fn validate_outputs(node: &Node, produced: &ArtifactMap) -> Result<(), String> {
    for name in produced.keys() {
        if !node.outputs.iter().any(|port| &port.name == name) {
            return Err(format!(
                "node produced undeclared output port {}.{name}",
                node.id
            ));
        }
    }
    for port in &node.outputs {
        let count = produced.get(&port.name).map_or(0, Vec::len);
        if !port.optional && count == 0 {
            return Err(format!(
                "node produced no artifacts for required output port {}.{}",
                node.id, port.name
            ));
        }
        if port.cardinality == review_core::PortCardinality::One && count > 1 {
            return Err(format!(
                "node produced {count} artifacts for single-valued output port {}.{}",
                node.id, port.name
            ));
        }
    }
    Ok(())
}

// Store rederives the captured contract and complete set. Scheduler validation independently
// prevents expansion from overwriting another owner/static node or introducing hidden edges.
fn validate_owned_expansion(
    parent: &Node,
    children: &[OwnedChildDispatch],
    nodes: &BTreeMap<String, Node>,
) -> Result<(), String> {
    let prefix = format!("{}.", parent.id);
    let mut seen = BTreeSet::new();
    for child in children {
        let node = &child.node;
        if node
            .id
            .strip_prefix(&prefix)
            .is_none_or(|suffix| !review_core::task::is_name(suffix))
            || nodes.contains_key(&node.id)
            || !seen.insert(&node.id)
            || node.gated_by.is_some()
            || !matches!(node.kind, NodeKind::Task | NodeKind::Reviewer)
            || child.inputs.len() != node.inputs.len()
            || node.inputs.iter().any(|port| {
                child.inputs.get(&port.name).is_none_or(|values| {
                    (!port.optional && values.is_empty())
                        || (port.cardinality == review_core::PortCardinality::One
                            && values.len() > 1)
                })
            })
            || [&node.inputs, &node.outputs].iter().any(|ports| {
                let mut names = BTreeSet::new();
                ports.iter().any(|port| {
                    !review_core::task::is_name(&port.name)
                        || !review_core::is_artifact_type(&port.artifact_type)
                        || !names.insert(&port.name)
                })
            })
        {
            return Err(format!("Invalid owned-child dispatch for {}", parent.id));
        }
    }
    Ok(())
}
