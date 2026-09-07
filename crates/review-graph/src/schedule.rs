//! Executing a planned pipeline.
//!
//! Ready nodes run concurrently and a freed slot is refilled at once; results are admitted in
//! canonical order. Gating is structural:
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
pub trait Dispatch {
    /// Persist or otherwise observe the exact input selection before the node is scheduled.
    /// Called on the scheduler thread the moment every input is published, in plan order,
    /// whether or not a slot is free — so the record is a function of the plan alone.
    fn record_invocation(&self, _node: &Node, _inputs: &ArtifactMap) -> Result<(), String> {
        Ok(())
    }

    /// Reserve what the node needs and durably record its dispatch, immediately before it
    /// starts. Called on the scheduler thread in the order nodes take slots, so under
    /// `max_parallel = 1` reservations are strictly sequential. `Err` means the node never ran.
    fn prepare_dispatch(&self, _node: &Node, _inputs: &ArtifactMap) -> Result<(), String> {
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
}

/// The bound on simultaneously running nodes when a pipeline declares none.
pub const DEFAULT_MAX_PARALLEL: usize = 4;

impl<'a> Scheduler<'a> {
    pub fn new(plan: &'a Planned) -> Scheduler<'a> {
        Scheduler {
            plan,
            // The design's default. Reviewers are model calls: minutes of latency each, no
            // local CPU — running them one after another priced a review at the *sum* of
            // model latencies.
            max_parallel: DEFAULT_MAX_PARALLEL,
        }
    }

    /// Bound on concurrently running nodes. `1` makes the run fully sequential.
    pub fn with_parallelism(mut self, max_parallel: usize) -> Scheduler<'a> {
        self.max_parallel = max_parallel.max(1);
        self
    }

    /// The bound on simultaneously running nodes this scheduler enforces.
    pub fn max_parallel(&self) -> usize {
        self.max_parallel
    }

    /// Execute the plan.
    ///
    /// Ready nodes run concurrently, up to `max_parallel`, and a slot is refilled the moment
    /// its occupant completes — never held until the slowest node of a wave returns. Determinism
    /// survives the concurrency because nothing about the *result* depends on completion order:
    ///
    /// - a node is *invoked* (its exact inputs recorded through `record_invocation`) as soon as
    ///   every gate and upstream node it depends on has been admitted, in plan order, whether
    ///   or not a slot is free — its inputs are exactly what the edges deliver (sorted) and can
    ///   no longer change;
    /// - invoked nodes take slots in plan order as slots free up; taking a slot runs
    ///   `prepare_dispatch` (reservation, durable dispatch) on this thread just before the node
    ///   starts, so reservations are sequential under a bound of one;
    /// - a completion is buffered and *admitted* (validated, published through
    ///   `record_outputs`, and made visible to dependents) only when every invoked node ahead
    ///   of it in plan order has been admitted;
    /// - after each single admission the plan is rescanned before the next, so the nodes an
    ///   admission makes ready are invoked at one canonical point in the sequence.
    ///
    /// The sequence of `record_invocation` and `record_outputs` calls — inputs and publications
    /// — is therefore a function of the pipeline alone, the same for every completion order and
    /// every `max_parallel`. Only *when* a refilled slot's `prepare_dispatch` runs follows the
    /// completion that freed it, which is the one thing a refill cannot avoid. Suppression is a
    /// function of resolved upstream state alone, and the report lists nodes in plan order.
    pub fn run(&self, dispatch: &(dyn Dispatch + Sync)) -> RunReport {
        let mut outputs: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        let mut outcomes: BTreeMap<String, NodeOutcome> = BTreeMap::new();
        let mut blocked_gates: BTreeSet<String> = BTreeSet::new();
        let mut unusable: BTreeSet<String> = BTreeSet::new();
        // Invoked nodes that have not been admitted, keyed by plan position so admission and
        // dispatch follow plan order: waiting for a slot, running, or completed and buffered.
        let mut waiting: BTreeMap<usize, ArtifactMap> = BTreeMap::new();
        let mut running: BTreeSet<usize> = BTreeSet::new();
        let mut done: BTreeMap<usize, Result<ArtifactMap, String>> = BTreeMap::new();

        std::thread::scope(|scope| {
            type Completion = (usize, Result<ArtifactMap, String>);
            let (tx, rx) = std::sync::mpsc::channel::<Completion>();

            loop {
                // Decide everything currently decidable, in plan order: suppress what a
                // blocked gate or a missing upstream has doomed, invoke what is ready.
                let mut progressed = false;
                for (position, node_id) in self.plan.order.iter().enumerate() {
                    if outcomes.contains_key(node_id)
                        || waiting.contains_key(&position)
                        || running.contains(&position)
                        || done.contains_key(&position)
                    {
                        continue;
                    }
                    let node = &self.plan.nodes[node_id];

                    // Gating first: a blocked gate suppresses this node before any input is
                    // resolved, so a suppressed node cannot even observe its would-be inputs.
                    let gates = self.plan.gates_for(node_id);
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

                    let dependencies = self.plan.dependencies_of(node_id);
                    if dependencies
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

                    // Not ready: some gate or upstream is still unadmitted. The plan order is
                    // topological over edges *and* gating, so this always clears.
                    let resolved = |id: &str| outcomes.contains_key(id);
                    if !gates.iter().all(|gate| resolved(gate))
                        || !dependencies.iter().all(|edge| resolved(&edge.from.node))
                    {
                        continue;
                    }

                    // Inputs are exactly what the edges resolved to, each labelled with the
                    // input port it arrived on — so a node reads its inputs by name (a reviewer
                    // takes `prior_findings`, not "whichever artifact happened to be first").
                    // Sorted, so the vector does not depend on edge declaration order.
                    let mut inputs: ArtifactMap = node
                        .inputs
                        .iter()
                        .map(|port| (port.name.clone(), Vec::new()))
                        .collect();
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
                    for artifacts in inputs.values_mut() {
                        artifacts.sort();
                    }

                    // Invoked now, in plan order, whether or not a slot is free: the inputs are
                    // final, so recording them is a function of the plan rather than of when a
                    // slot happened to open.
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
                    waiting.insert(position, inputs);
                    progressed = true;
                }

                // Fill every free slot, in plan order among the invoked nodes.
                while running.len() < self.max_parallel {
                    let Some(position) = waiting.keys().next().copied() else {
                        break;
                    };
                    let inputs = waiting.remove(&position).expect("waiting node");
                    let node_id = &self.plan.order[position];
                    let node = &self.plan.nodes[node_id];
                    if let Err(error) = dispatch.prepare_dispatch(node, &inputs) {
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
                    running.insert(position);
                    let tx = tx.clone();
                    scope.spawn(move || {
                        // A panicking dispatcher is a failed node, not a hung run: without
                        // this, its completion never arrives and the loop waits forever.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            dispatch.run(node, &inputs)
                        }))
                        .unwrap_or_else(|_| Err(format!("dispatch panicked for node {}", node.id)));
                        let _ = tx.send((position, result));
                    });
                }

                // Admit the plan-earliest invoked node once it has completed — one admission,
                // then rescan, so dependents it readies are invoked at a canonical point.
                let head = waiting
                    .keys()
                    .next()
                    .copied()
                    .into_iter()
                    .chain(running.iter().next().copied())
                    .chain(done.keys().next().copied())
                    .min();
                if let Some(position) = head
                    && let Some(result) = done.remove(&position)
                {
                    let node_id = &self.plan.order[position];
                    let node = &self.plan.nodes[node_id];
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
                    continue;
                }

                if running.is_empty() {
                    if progressed {
                        // Suppressions may cascade; scan again before concluding.
                        continue;
                    }
                    break;
                }

                // Every admissible result is admitted and every free slot is filled: wait for
                // a completion. It frees its slot immediately; its result waits its turn.
                let (position, result) = rx.recv().expect("a running node reports its outcome");
                running.remove(&position);
                done.insert(position, result);
                while let Ok((position, result)) = rx.try_recv() {
                    running.remove(&position);
                    done.insert(position, result);
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
