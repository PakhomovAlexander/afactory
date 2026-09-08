//! The pipeline the design describes, executed.
//!
//! gate -> (architecture | performance | tdd) -> gather -> ledger, with the reviewers gated on
//! the gate. Every property below is about *scheduling*, so the dispatcher is a recording stub:
//! no models, no checks, no filesystem. That is the point of the split — these guarantees can be
//! proved without anything expensive or nondeterministic in the loop.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use review_graph::{
    ArtifactMap, Dispatch, Node, NodeFailureClass, NodeKind, NodeOutcome, Pipeline, PlanError,
    Port, PortCardinality, PortContract, Scheduler, SnapshotAffinity, SuppressionReason,
};

/// Records every dispatch, so "this node never ran" is checkable rather than assumed.
struct Recorder {
    dispatched: Mutex<Vec<String>>,
    gate_passes: bool,
    failing: Option<String>,
}

impl Recorder {
    fn new(gate_passes: bool) -> Recorder {
        Recorder {
            dispatched: Mutex::new(Vec::new()),
            gate_passes,
            failing: None,
        }
    }

    fn failing(node: &str) -> Recorder {
        Recorder {
            dispatched: Mutex::new(Vec::new()),
            gate_passes: true,
            failing: Some(node.to_string()),
        }
    }

    fn log(&self) -> Vec<String> {
        self.dispatched.lock().unwrap().clone()
    }
}

impl Dispatch for Recorder {
    fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        let node_id = node.id.as_str();
        let rendered = inputs
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        self.dispatched
            .lock()
            .unwrap()
            .push(format!("{node_id}({rendered})"));
        if self.failing.as_deref() == Some(node_id) {
            return Err(format!("{node_id} exploded"));
        }
        Ok(BTreeMap::from([(
            node.outputs[0].name.clone(),
            vec![format!("artifact:{node_id}")],
        )]))
    }

    fn gate_passed(&self, _node_id: &str, _outputs: &ArtifactMap) -> bool {
        self.gate_passes
    }
}

fn heavy_pipeline() -> Pipeline {
    let mut pipeline = Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(
            Node::new("gather", NodeKind::Gather)
                .accepting(&["architecture", "performance", "tdd"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["reports"])
                .emitting(&["findings"]),
        );

    for reviewer in ["architecture", "performance", "tdd"] {
        pipeline = pipeline
            .node(
                Node::new(reviewer, NodeKind::Reviewer)
                    .accepting(&["gate"])
                    .emitting(&["result"])
                    .gated_by("gate"),
            )
            .edge(Port::new("gate", "decision"), Port::new(reviewer, "gate"))
            .edge(Port::new(reviewer, "result"), Port::new("gather", reviewer));
    }
    pipeline.edge(
        Port::new("gather", "reports"),
        Port::new("ledger", "reports"),
    )
}

#[test]
fn the_plan_order_is_a_function_of_the_pipeline() {
    let a = heavy_pipeline().plan().unwrap();
    let b = heavy_pipeline().plan().unwrap();
    assert_eq!(a.order, b.order);
    assert_eq!(a.order[0], "gate", "the gate is first");
    assert_eq!(a.order.last().unwrap(), "ledger");
    // Ties among the three ready reviewers break by ID, not by declaration order.
    assert_eq!(&a.order[1..4], &["architecture", "performance", "tdd"]);
}

#[test]
fn a_passing_gate_lets_everything_run() {
    let plan = heavy_pipeline().plan().unwrap();
    let recorder = Recorder::new(true);
    let report = Scheduler::new(&plan).run(&recorder);

    assert!(report.complete(), "{:?}", report.outcomes);
    assert_eq!(report.suppressed(), Vec::<&str>::new());
    assert_eq!(
        recorder.log().len(),
        6,
        "gate + three reviewers + gather + ledger"
    );

    // Inputs are exactly what the edges resolved to.
    assert!(
        recorder
            .log()
            .contains(&"architecture(artifact:gate)".to_string()),
        "{:?}",
        recorder.log()
    );
    assert!(
        recorder.log().iter().any(|entry| entry
            .starts_with("gather(artifact:architecture,artifact:performance,artifact:tdd)")),
        "gather receives all three, sorted: {:?}",
        recorder.log()
    );
}

/// The property gating exists for: a failed gate must make downstream dispatch impossible, not
/// merely discouraged.
#[test]
fn a_blocked_gate_suppresses_every_gated_node() {
    let plan = heavy_pipeline().plan().unwrap();
    let recorder = Recorder::new(false);
    let report = Scheduler::new(&plan).run(&recorder);

    assert_eq!(
        recorder.log(),
        vec!["gate()"],
        "nothing beyond the gate may be dispatched"
    );
    assert!(!report.complete());
    assert_eq!(
        report.suppressed(),
        // Plan order, not alphabetical: the report reads as the run would have gone.
        vec!["architecture", "performance", "tdd", "gather", "ledger"]
    );
    assert!(report.blocked_gates.contains("gate"));

    // Suppression is labelled with its cause, and a suppressed node is present in the report —
    // an absent node would read as "nothing to report".
    assert_eq!(
        report.outcome("architecture"),
        Some(&NodeOutcome::Suppressed {
            reason: SuppressionReason::GateBlocked
        })
    );
    // gather is labelled GateBlocked too, not UpstreamMissing: gating is transitive, so the
    // root cause wins over the proximate one. Reporting "upstream missing" across a whole
    // suppressed subgraph would bury the single fact that explains all of it.
    assert_eq!(
        report.outcome("gather"),
        Some(&NodeOutcome::Suppressed {
            reason: SuppressionReason::GateBlocked
        })
    );
}

/// One reviewer failing is a fact about the review, not a reason to lose the rest of it — but
/// nothing may consume an output that does not exist.
#[test]
fn a_failed_reviewer_does_not_take_the_pipeline_down_but_does_stop_its_dependents() {
    let plan = heavy_pipeline().plan().unwrap();
    let recorder = Recorder::failing("performance");
    let report = Scheduler::new(&plan).run(&recorder);

    assert!(matches!(
        report.outcome("performance"),
        Some(NodeOutcome::Failed { .. })
    ));
    assert!(matches!(
        report.outcome("architecture"),
        Some(NodeOutcome::Completed { .. })
    ));
    assert!(matches!(
        report.outcome("tdd"),
        Some(NodeOutcome::Completed { .. })
    ));
    assert_eq!(
        report.outcome("gather"),
        Some(&NodeOutcome::Suppressed {
            reason: SuppressionReason::UpstreamMissing
        }),
        "gather cannot run on two of three inputs and call it a gather"
    );
    assert!(!report.complete());
}

#[test]
fn planning_refuses_a_cycle_before_anything_runs() {
    let pipeline = Pipeline::default()
        .node(
            Node::new("a", NodeKind::Reviewer)
                .accepting(&["in"])
                .emitting(&["out"]),
        )
        .node(
            Node::new("b", NodeKind::Reviewer)
                .accepting(&["in"])
                .emitting(&["out"]),
        )
        .edge(Port::new("a", "out"), Port::new("b", "in"))
        .edge(Port::new("b", "out"), Port::new("a", "in"));
    assert!(matches!(pipeline.plan(), Err(PlanError::Cycle(_))));
}

#[test]
fn planning_refuses_an_edge_to_a_port_that_does_not_exist() {
    let pipeline = Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(Node::new("deep", NodeKind::Reviewer).accepting(&["gate"]))
        // Typo: the reviewer accepts "gate", not "gates".
        .edge(Port::new("gate", "decision"), Port::new("deep", "gates"));
    assert!(matches!(
        pipeline.plan(),
        Err(PlanError::UnknownPort { .. })
    ));
}

/// The failure this typing exists to prevent: a reviewer that expects prior findings, wired to
/// nothing, reviewing an empty input with full confidence.
#[test]
fn planning_refuses_an_input_nothing_feeds() {
    let pipeline = Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(Node::new("deep", NodeKind::Reviewer).accepting(&["gate", "prior_findings"]))
        .edge(Port::new("gate", "decision"), Port::new("deep", "gate"));
    match pipeline.plan() {
        Err(PlanError::UnwiredInput(port)) => {
            assert_eq!(port, Port::new("deep", "prior_findings"));
        }
        other => panic!("expected an unwired input, got {other:?}"),
    }
}

#[test]
fn planning_refuses_an_unknown_node_or_gate() {
    let missing_node = Pipeline::default()
        .node(Node::new("a", NodeKind::Reviewer).emitting(&["out"]))
        .node(Node::new("b", NodeKind::Reviewer).accepting(&["in"]))
        .edge(Port::new("ghost", "out"), Port::new("b", "in"));
    assert!(matches!(
        missing_node.plan(),
        Err(PlanError::UnknownNode { .. })
    ));

    let missing_gate =
        Pipeline::default().node(Node::new("a", NodeKind::Reviewer).gated_by("no-such-gate"));
    assert!(matches!(
        missing_gate.plan(),
        Err(PlanError::UnknownGate { .. })
    ));
}

/// Gating is transitive: a node downstream of a gated node is gated too, without having to say
/// so. Otherwise every pipeline author has to remember to re-declare it, and one omission is a
/// node running after its gate blocked.
#[test]
fn gating_reaches_through_the_graph() {
    let plan = heavy_pipeline().plan().unwrap();
    assert!(plan.gates_for("ledger").contains("gate"));
    assert!(plan.gates_for("gather").contains("gate"));
    assert!(plan.gates_for("gate").is_empty());
}

/// The point of the concurrency: independent reviewers cost max(t), not sum(t). Three reviewers
/// behind one gate must be in flight *at the same time*. The dispatcher proves it directly: each
/// reviewer holds its slot until all three have arrived, and the maximum simultaneous count is
/// asserted to be exactly three. An elapsed-time bound would also have passed a two-at-a-time
/// scheduler (600ms of 300ms reviewers is under any generous limit); only the timeout that stops
/// a wedged scheduler from hanging the suite is time-based.
#[test]
fn independent_reviewers_run_concurrently() {
    struct Rendezvous {
        /// `(active now, maximum ever active)` reviewer dispatches.
        state: Mutex<(usize, usize)>,
        arrived: std::sync::Condvar,
        expected: usize,
    }
    impl Dispatch for Rendezvous {
        fn run(&self, node: &Node, _inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
            if node.kind == NodeKind::Reviewer {
                let mut state = self.state.lock().unwrap();
                state.0 += 1;
                state.1 = state.1.max(state.0);
                self.arrived.notify_all();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while state.1 < self.expected {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        state.0 -= 1;
                        return Err(format!(
                            "{} waited 10s for {} simultaneous reviewers; saw at most {}",
                            node.id, self.expected, state.1
                        ));
                    }
                    state = self.arrived.wait_timeout(state, remaining).unwrap().0;
                }
                state.0 -= 1;
            }
            Ok(BTreeMap::from([(
                node.outputs[0].name.clone(),
                vec![format!("artifact:{}", node.id)],
            )]))
        }
    }

    let plan = heavy_pipeline().plan().unwrap();
    let dispatcher = Rendezvous {
        state: Mutex::new((0, 0)),
        arrived: std::sync::Condvar::new(),
        expected: 3,
    };
    let report = Scheduler::new(&plan).run(&dispatcher);

    assert!(report.complete(), "{:?}", report.outcomes);
    let (active, max_active) = *dispatcher.state.lock().unwrap();
    assert_eq!(active, 0, "every reviewer released its slot");
    assert_eq!(
        max_active, 3,
        "three independent reviewers must be dispatched simultaneously"
    );
}

/// A slot is refilled the moment its occupant completes, not when the slowest member of the
/// dispatch wave returns. Five independent reviewers under the default bound of four: `a` holds
/// its slot until it has seen `e` start, and `e` can only start if the slots `b`, `c`, and `d`
/// release are refilled while `a` is still running. A wave-at-a-time scheduler deadlocks here
/// and fails through `a`'s timeout.
#[test]
fn a_freed_slot_is_refilled_while_its_wave_is_still_running() {
    struct Refill {
        /// `(e has started, a is running)`.
        state: Mutex<(bool, bool)>,
        changed: std::sync::Condvar,
        active: Mutex<(usize, usize)>,
    }
    impl Refill {
        fn enter(&self) {
            let mut active = self.active.lock().unwrap();
            active.0 += 1;
            active.1 = active.1.max(active.0);
        }
        fn leave(&self) {
            self.active.lock().unwrap().0 -= 1;
        }
    }
    impl Dispatch for Refill {
        fn run(&self, node: &Node, _inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
            if node.kind == NodeKind::Reviewer {
                self.enter();
                match node.id.as_str() {
                    "a" => {
                        let mut state = self.state.lock().unwrap();
                        state.1 = true;
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                        while !state.0 {
                            let remaining =
                                deadline.saturating_duration_since(std::time::Instant::now());
                            if remaining.is_zero() {
                                state.1 = false;
                                self.leave();
                                return Err(
                                    "a waited 10s for e to start; the freed slots were not refilled"
                                        .into(),
                                );
                            }
                            state = self.changed.wait_timeout(state, remaining).unwrap().0;
                        }
                        state.1 = false;
                    }
                    "e" => {
                        let mut state = self.state.lock().unwrap();
                        assert!(state.1, "e started only after a finished: no refill");
                        state.0 = true;
                        self.changed.notify_all();
                    }
                    _ => {}
                }
                self.leave();
            }
            Ok(BTreeMap::from([(
                node.outputs[0].name.clone(),
                vec![format!("artifact:{}", node.id)],
            )]))
        }
    }

    let mut pipeline = Pipeline::default().node(
        Node::new("gather", NodeKind::Gather)
            .accepting(&["a", "b", "c", "d", "e"])
            .emitting(&["reports"]),
    );
    for reviewer in ["a", "b", "c", "d", "e"] {
        pipeline = pipeline
            .node(Node::new(reviewer, NodeKind::Reviewer).emitting(&["result"]))
            .edge(Port::new(reviewer, "result"), Port::new("gather", reviewer));
    }
    let plan = pipeline.plan().unwrap();
    let dispatcher = Refill {
        state: Mutex::new((false, false)),
        changed: std::sync::Condvar::new(),
        active: Mutex::new((0, 0)),
    };
    let scheduler = Scheduler::new(&plan);
    assert_eq!(scheduler.max_parallel(), 4);
    let report = scheduler.run(&dispatcher);

    assert!(report.complete(), "{:?}", report.outcomes);
    let (_, max_active) = *dispatcher.active.lock().unwrap();
    assert!(
        max_active <= 4,
        "the bound still holds while slots are refilled: {max_active} ran at once"
    );
}

/// The sequence of invocations and admissions the dispatcher observes — the shape of the
/// durable log — is a function of the pipeline alone: the same under every completion order
/// and under every parallelism bound. Four reviewers, two dependents readied by individual
/// admissions, four delay patterns that really do change the completion order, and bounds
/// from fully sequential to fully parallel: one sequence.
#[test]
fn the_admission_sequence_is_canonical_under_shuffled_completion_and_any_bound() {
    struct Timed {
        delays_ms: BTreeMap<&'static str, u64>,
        sequence: Mutex<Vec<String>>,
        completed: Mutex<Vec<String>>,
    }
    impl Dispatch for Timed {
        fn record_invocation(&self, node: &Node, _inputs: &ArtifactMap) -> Result<(), String> {
            self.sequence
                .lock()
                .unwrap()
                .push(format!("inv:{}", node.id));
            Ok(())
        }
        fn run(&self, node: &Node, _inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
            if let Some(delay) = self.delays_ms.get(node.id.as_str()) {
                std::thread::sleep(std::time::Duration::from_millis(*delay));
            }
            self.completed.lock().unwrap().push(node.id.clone());
            Ok(BTreeMap::from([(
                node.outputs[0].name.clone(),
                vec![format!("artifact:{}", node.id)],
            )]))
        }
        fn record_outputs(&self, node: &Node, _outputs: &ArtifactMap) -> Result<(), String> {
            self.sequence
                .lock()
                .unwrap()
                .push(format!("out:{}", node.id));
            Ok(())
        }
    }

    // r1..r4 are independent; g1 waits on r1 alone and g2 on r2 alone, so each of those
    // admissions readies exactly one more node; the ledger waits on everything.
    let mut pipeline = Pipeline::default()
        .node(
            Node::new("g1", NodeKind::Gather)
                .accepting(&["r1"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("g2", NodeKind::Gather)
                .accepting(&["r2"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["g1", "g2", "r3", "r4"])
                .emitting(&["findings"]),
        )
        .edge(Port::new("g1", "reports"), Port::new("ledger", "g1"))
        .edge(Port::new("g2", "reports"), Port::new("ledger", "g2"));
    for reviewer in ["r1", "r2", "r3", "r4"] {
        pipeline = pipeline.node(Node::new(reviewer, NodeKind::Reviewer).emitting(&["result"]));
    }
    pipeline = pipeline
        .edge(Port::new("r1", "result"), Port::new("g1", "r1"))
        .edge(Port::new("r2", "result"), Port::new("g2", "r2"))
        .edge(Port::new("r3", "result"), Port::new("ledger", "r3"))
        .edge(Port::new("r4", "result"), Port::new("ledger", "r4"));
    let plan = pipeline.plan().unwrap();

    let patterns: [[u64; 4]; 4] = [
        [0, 20, 40, 60],
        [60, 40, 20, 0],
        [40, 0, 60, 20],
        [20, 60, 0, 40],
    ];
    let mut sequences = BTreeSet::new();
    let mut parallel_completion_orders = BTreeSet::new();
    for pattern in patterns {
        for bound in [1, 2, 4] {
            let dispatcher = Timed {
                delays_ms: ["r1", "r2", "r3", "r4"].into_iter().zip(pattern).collect(),
                sequence: Mutex::new(Vec::new()),
                completed: Mutex::new(Vec::new()),
            };
            let report = Scheduler::new(&plan)
                .with_parallelism(bound)
                .run(&dispatcher);
            assert!(report.complete(), "{:?}", report.outcomes);
            sequences.insert(dispatcher.sequence.into_inner().unwrap());
            if bound == 4 {
                parallel_completion_orders.insert(dispatcher.completed.into_inner().unwrap());
            }
        }
    }
    assert!(
        parallel_completion_orders.len() >= 3,
        "the delay patterns must really change the completion order: {parallel_completion_orders:?}"
    );
    assert_eq!(
        sequences.len(),
        1,
        "the invocation/admission sequence varied with timing or the bound: {sequences:?}"
    );
    let sequence = sequences.into_iter().next().unwrap();
    let index = |event: &str| {
        sequence
            .iter()
            .position(|entry| entry == event)
            .unwrap_or_else(|| panic!("{event} missing from {sequence:?}"))
    };
    // Admissions follow plan order among the invoked nodes.
    assert!(index("out:r1") < index("out:r2") && index("out:r2") < index("out:r3"));
    assert!(index("out:r3") < index("out:r4"));
    // A dependent is invoked right after the admission that readied it, before the next one.
    assert!(index("out:r1") < index("inv:g1") && index("inv:g1") < index("out:r2"));
    assert!(index("out:r2") < index("inv:g2") && index("inv:g2") < index("out:r3"));
    // Nothing is invoked before its inputs are published.
    assert!(index("out:g1") < index("inv:ledger") && index("out:g2") < index("inv:ledger"));
}

#[test]
fn planning_refuses_incompatible_port_contracts() {
    let typed_edge = |output: PortContract, input: PortContract| {
        Pipeline::default()
            .node(Node::new("producer", NodeKind::Generation).emitting_contracts(vec![output]))
            .node(Node::new("consumer", NodeKind::Gather).accepting_contracts(vec![input]))
            .edge(Port::new("producer", "out"), Port::new("consumer", "in"))
    };

    let output = PortContract::new("out", "review.kernel/FindingSet@1");
    let input = PortContract::new("in", "review.kernel/ReportSet@1");
    assert!(matches!(
        typed_edge(output, input).plan(),
        Err(PlanError::TypeMismatch { .. })
    ));

    let output = PortContract::new("out", "review.kernel/FindingSet@1")
        .with_cardinality(PortCardinality::Many);
    let input = PortContract::new("in", "review.kernel/FindingSet@1");
    assert!(matches!(
        typed_edge(output, input).plan(),
        Err(PlanError::CardinalityMismatch { .. })
    ));

    let output = PortContract::new("out", "review.kernel/FindingSet@1")
        .with_snapshot_affinity(SnapshotAffinity::Unbound);
    let input = PortContract::new("in", "review.kernel/FindingSet@1");
    assert!(matches!(
        typed_edge(output, input).plan(),
        Err(PlanError::SnapshotAffinityMismatch { .. })
    ));
}

#[test]
fn an_optional_input_may_be_unwired() {
    let pipeline =
        Pipeline::default().node(Node::new("consumer", NodeKind::Gather).accepting_contracts(
            vec![PortContract::new("maybe", "review.kernel/FindingSet@1").optional()],
        ));
    assert!(pipeline.plan().is_ok());
}

#[test]
fn planning_uses_the_persisted_artifact_type_contract() {
    let pipeline_for = |artifact_type: &str| {
        Pipeline::default().node(
            Node::new("producer", NodeKind::Generation)
                .emitting_contracts(vec![PortContract::new("out", artifact_type)]),
        )
    };

    assert!(pipeline_for("review.kernel/GateDecision@1").plan().is_ok());
    for invalid in [
        "review.kernel/GateDecision@01",
        "review.kernel/GateDecision@+1",
        "1review/GateDecision@1",
        ".review/GateDecision@1",
    ] {
        assert!(matches!(
            pipeline_for(invalid).plan(),
            Err(PlanError::InvalidArtifactType { .. })
        ));
    }
}

/// A `Capped` dispatcher stands in for the run-scope budget: `prepare_dispatch` is where the
/// kernel takes a `BudgetScope::Run` reservation, and its refusal is `RunBudgetExhausted` — a
/// durable `Failed` outcome the report carries. Reservations are never returned, which is the
/// shape of a run cap that cannot cover every first Attempt.
struct Capped {
    delays_ms: BTreeMap<&'static str, u64>,
    per_node_tokens: u64,
    remaining: Mutex<u64>,
    /// Every `prepare_dispatch`, in the order it happened: reserved or refused.
    sequence: Mutex<Vec<String>>,
    completed: Mutex<Vec<String>>,
}

impl Capped {
    fn new(delays_ms: BTreeMap<&'static str, u64>, per_node_tokens: u64, cap: u64) -> Capped {
        Capped {
            delays_ms,
            per_node_tokens,
            remaining: Mutex::new(cap),
            sequence: Mutex::new(Vec::new()),
            completed: Mutex::new(Vec::new()),
        }
    }
}

impl Dispatch for Capped {
    fn prepare_dispatch(&self, node: &Node, _inputs: &ArtifactMap) -> Result<(), String> {
        let mut remaining = self.remaining.lock().unwrap();
        if *remaining < self.per_node_tokens {
            self.sequence
                .lock()
                .unwrap()
                .push(format!("refused:{}", node.id));
            return Err(format!("run budget exhausted reserving for {}", node.id));
        }
        *remaining -= self.per_node_tokens;
        self.sequence
            .lock()
            .unwrap()
            .push(format!("reserved:{}", node.id));
        Ok(())
    }

    fn failure_class(&self, _node_id: &str) -> Option<NodeFailureClass> {
        Some(NodeFailureClass::RunBudgetExhausted)
    }

    fn run(&self, node: &Node, _inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        if let Some(delay) = self.delays_ms.get(node.id.as_str()) {
            std::thread::sleep(std::time::Duration::from_millis(*delay));
        }
        self.completed.lock().unwrap().push(node.id.clone());
        Ok(BTreeMap::from([(
            node.outputs[0].name.clone(),
            vec![format!("artifact:{}", node.id)],
        )]))
    }
}

/// The node a run cap exhausts is a fact about the pipeline, not about who finished first.
///
/// The existing sequence tests use dispatchers with no budget, so they never observe the one
/// thing `prepare_dispatch` exists for. Here the cap covers the gate and three of the four
/// reviewers: the fourth must be the *same* reviewer in every run, `Failed` with
/// `RunBudgetExhausted`, cascading the same suppression — otherwise two runs over an identical
/// Subject reach different verdicts, and the difference is a durable, reviewer-visible artifact
/// that is not a function of the pipeline.
#[test]
fn a_run_cap_exhausts_the_same_node_under_every_completion_order_and_bound() {
    let mut pipeline = Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(
            Node::new("gather", NodeKind::Gather)
                .accepting(&["r1", "r2", "r3", "r4"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["reports"])
                .emitting(&["findings"]),
        )
        .edge(
            Port::new("gather", "reports"),
            Port::new("ledger", "reports"),
        );
    for reviewer in ["r1", "r2", "r3", "r4"] {
        pipeline = pipeline
            .node(
                Node::new(reviewer, NodeKind::Reviewer)
                    .accepting(&["gate"])
                    .emitting(&["result"])
                    .gated_by("gate"),
            )
            .edge(Port::new("gate", "decision"), Port::new(reviewer, "gate"))
            .edge(Port::new(reviewer, "result"), Port::new("gather", reviewer));
    }
    let plan = pipeline.plan().unwrap();

    // Four reservations of 300k against a 1.2M cap: the gate and three reviewers fit, `r4`
    // does not, and `gather` is suppressed because its input never arrives.
    let patterns: [[u64; 4]; 4] = [
        [0, 20, 40, 60],
        [60, 40, 20, 0],
        [40, 0, 60, 20],
        [20, 60, 0, 40],
    ];
    let mut sequences = BTreeSet::new();
    let mut reports = BTreeSet::new();
    let mut parallel_completion_orders = BTreeSet::new();
    for pattern in patterns {
        for bound in [1, 2, 4] {
            let dispatcher = Capped::new(
                ["r1", "r2", "r3", "r4"].into_iter().zip(pattern).collect(),
                300_000,
                1_200_000,
            );
            let report = Scheduler::new(&plan)
                .with_parallelism(bound)
                .run(&dispatcher);
            reports.insert(
                report
                    .outcomes
                    .iter()
                    .map(|(id, outcome)| {
                        let shape = match outcome {
                            NodeOutcome::Completed { .. } => "completed".to_string(),
                            NodeOutcome::Failed { class, .. } => format!("failed:{class:?}"),
                            NodeOutcome::Suppressed { reason } => format!("suppressed:{reason:?}"),
                        };
                        format!("{id}={shape}")
                    })
                    .collect::<Vec<String>>(),
            );
            sequences.insert(dispatcher.sequence.into_inner().unwrap());
            if bound == 4 {
                parallel_completion_orders.insert(dispatcher.completed.into_inner().unwrap());
            }
        }
    }

    assert!(
        parallel_completion_orders.len() >= 3,
        "the delay patterns must really change the completion order: {parallel_completion_orders:?}"
    );
    assert_eq!(
        sequences,
        BTreeSet::from([vec![
            "reserved:gate".to_string(),
            "reserved:r1".to_string(),
            "reserved:r2".to_string(),
            "reserved:r3".to_string(),
            "refused:r4".to_string(),
        ]]),
        "the run cap was consumed in a different order"
    );
    assert_eq!(
        reports,
        BTreeSet::from([vec![
            "gate=completed".to_string(),
            "r1=completed".to_string(),
            "r2=completed".to_string(),
            "r3=completed".to_string(),
            "r4=failed:Some(RunBudgetExhausted)".to_string(),
            "gather=suppressed:UpstreamMissing".to_string(),
            "ledger=suppressed:UpstreamMissing".to_string(),
        ]]),
        "a different node was failed or suppressed"
    );
}

/// An admission readies its dependent *before* the slot it freed is filled.
///
/// Plan `a, b, c, x1, x2` with `c` downstream of `a` and the rest sources, bound two. When `a`
/// completes, the loop admits it, rescans — which invokes `c` — and only then fills the slot
/// `a` just freed, so `c` takes it. Filling first handed that slot to `x1`, and whether `c` or
/// `x1` reserved third then depended on how many completions the last drain happened to
/// collect: a plain `try_recv` race that varies run to run at identical delays. `a` is the
/// fastest node here, so this is the case the loop order decides; when the plan-earliest
/// running node is *not* first to finish, dispatch order still follows the completion that
/// freed the slot, which `Scheduler::run` documents.
#[test]
fn an_admission_readies_its_dependent_before_the_slot_it_freed_is_filled() {
    let pipeline = Pipeline::default()
        .node(Node::new("a", NodeKind::Reviewer).emitting(&["result"]))
        .node(Node::new("b", NodeKind::Reviewer).emitting(&["result"]))
        .node(
            Node::new("c", NodeKind::Reviewer)
                .accepting(&["upstream"])
                .emitting(&["result"]),
        )
        .node(Node::new("x1", NodeKind::Reviewer).emitting(&["result"]))
        .node(Node::new("x2", NodeKind::Reviewer).emitting(&["result"]))
        .edge(Port::new("a", "result"), Port::new("c", "upstream"));
    let plan = pipeline.plan().unwrap();
    assert_eq!(plan.order, vec!["a", "b", "c", "x1", "x2"]);

    let mut sequences = BTreeSet::new();
    for trailing in [[30_u64, 60], [60, 30], [30, 30], [0, 0]] {
        for _ in 0..4 {
            let dispatcher = Capped::new(
                BTreeMap::from([
                    ("a", 0),
                    ("b", 90),
                    ("x1", trailing[0]),
                    ("x2", trailing[1]),
                ]),
                300_000,
                u64::MAX,
            );
            let report = Scheduler::new(&plan).with_parallelism(2).run(&dispatcher);
            assert!(report.complete(), "{:?}", report.outcomes);
            sequences.insert(dispatcher.sequence.into_inner().unwrap());
        }
    }
    assert_eq!(
        sequences,
        BTreeSet::from([vec![
            "reserved:a".to_string(),
            "reserved:b".to_string(),
            "reserved:c".to_string(),
            "reserved:x1".to_string(),
            "reserved:x2".to_string(),
        ]]),
        "a slot freed by `a` went to a plan-later node instead of the dependent `a` readied"
    );
}
