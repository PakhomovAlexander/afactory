//! The pipeline the design describes, executed.
//!
//! gate -> (architecture | performance | tdd) -> gather -> ledger, with the reviewers gated on
//! the gate. Every property below is about *scheduling*, so the dispatcher is a recording stub:
//! no models, no checks, no filesystem. That is the point of the split — these guarantees can be
//! proved without anything expensive or nondeterministic in the loop.

use std::collections::BTreeMap;
use std::sync::Mutex;

use review_graph::{
    ArtifactMap, Dispatch, Node, NodeKind, NodeOutcome, Pipeline, PlanError, Port, PortCardinality,
    PortContract, Scheduler, SnapshotAffinity, SuppressionReason,
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

/// The point of the concurrency: independent reviewers cost max(t), not sum(t). Three 300ms
/// reviewers behind one gate must overlap — a sequential dispatcher would take 900ms.
#[test]
fn independent_reviewers_run_concurrently() {
    struct Sleepy;
    impl Dispatch for Sleepy {
        fn run(&self, node: &Node, _inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
            if node.kind == NodeKind::Reviewer {
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            Ok(BTreeMap::from([(
                node.outputs[0].name.clone(),
                vec![format!("artifact:{}", node.id)],
            )]))
        }
    }

    let plan = heavy_pipeline().plan().unwrap();
    let start = std::time::Instant::now();
    let report = Scheduler::new(&plan).run(&Sleepy);
    let elapsed = start.elapsed();

    assert!(report.complete(), "{:?}", report.outcomes);
    assert!(
        elapsed < std::time::Duration::from_millis(700),
        "three 300ms reviewers took {elapsed:?}; they must overlap"
    );
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
