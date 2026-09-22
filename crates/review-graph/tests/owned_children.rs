use review_graph::{
    ArtifactMap, Dispatch, Node, NodeKind, NodeOutcome, OwnedChildDispatch, Pipeline, Port,
    PortContract, Scheduler,
};
use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};

#[derive(Default)]
struct State {
    events: Vec<String>,
    terminal: Vec<(String, NodeOutcome)>,
    running: usize,
    peak: usize,
    second_done: bool,
    first_started: bool,
}
struct Host {
    state: Mutex<State>,
    changed: Condvar,
    count: usize,
    fail: bool,
    reverse: bool,
    invalid: bool,
}
impl Host {
    fn new(count: usize) -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            count,
            fail: false,
            reverse: false,
            invalid: false,
        }
    }
    fn event(&self, event: String) {
        self.state.lock().unwrap().events.push(event);
    }
}
fn output(id: &str) -> ArtifactMap {
    BTreeMap::from([("out".into(), vec![format!("artifact:{id}")])])
}
impl Dispatch for Host {
    fn coordinates_owned_children(&self, node: &Node) -> bool {
        node.id == "root.owner"
    }
    fn record_invocation(&self, node: &Node, _: &ArtifactMap) -> Result<(), String> {
        self.event(format!("invoke:{}", node.id));
        Ok(())
    }
    fn expand_owned_children(
        &self,
        node: &Node,
        _: &ArtifactMap,
    ) -> Result<Vec<OwnedChildDispatch>, String> {
        assert_eq!(
            self.state.lock().unwrap().events.last().unwrap(),
            &format!("invoke:{}", node.id)
        );
        self.event("register:all".into());
        Ok((0..self.count)
            .map(|index| OwnedChildDispatch {
                node: Node::new(
                    if self.invalid {
                        "root.after".into()
                    } else {
                        format!("{}.slice{index}", node.id)
                    },
                    NodeKind::Task,
                )
                .accepting_contracts(vec![PortContract::opaque("item")]),
                inputs: BTreeMap::from([("item".into(), vec![format!("item:{index}")])]),
            })
            .collect())
    }
    fn complete_owned_children(
        &self,
        node: &Node,
        _: &ArtifactMap,
        children: &[(String, NodeOutcome)],
    ) -> Result<ArtifactMap, String> {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.running, 0);
        state.events.push("fold:all".into());
        state.terminal = children.to_vec();
        Ok(output(&node.id))
    }
    fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        assert_ne!(
            node.id, "root.owner",
            "owner must never acquire a Worker slot"
        );
        let mut state = self.state.lock().unwrap();
        state.running += 1;
        state.peak = state.peak.max(state.running);
        state.events.push(format!("run:{}", node.id));
        if let Some(index) = node.id.strip_prefix("root.owner.slice") {
            assert!(state.events.contains(&"register:all".into()));
            assert_eq!(inputs["item"], vec![format!("item:{index}")]);
        }
        if self.reverse && node.id.ends_with("slice0") {
            state.first_started = true;
            self.changed.notify_all();
            while !state.second_done {
                state = self.changed.wait(state).unwrap();
            }
        }
        if self.reverse && node.id.ends_with("slice1") {
            while !state.first_started {
                state = self.changed.wait(state).unwrap();
            }
            state.second_done = true;
            self.changed.notify_all();
        }
        state.running -= 1;
        state.events.push(format!("done:{}", node.id));
        if self.fail && node.id.ends_with("slice1") {
            Err("retained failure".into())
        } else {
            Ok(output(&node.id))
        }
    }
    fn record_outputs(&self, node: &Node, _: &ArtifactMap) -> Result<(), String> {
        self.event(format!("publish:{}", node.id));
        Ok(())
    }
}
fn plan() -> review_graph::Planned {
    Pipeline::default()
        .node(Node::new("root.owner", NodeKind::Task))
        .node(
            Node::new("root.after", NodeKind::Task)
                .accepting_contracts(vec![PortContract::opaque("parent")]),
        )
        .edge(
            Port::new("root.owner", "out"),
            Port::new("root.after", "parent"),
        )
        .plan()
        .unwrap()
}

#[test]
fn an_owned_parent_makes_progress_with_one_global_slot_and_retains_every_failure() {
    let plan = plan();
    let mut host = Host::new(3);
    host.fail = true;
    let report = Scheduler::new(&plan, 1).run(&host);
    assert_eq!(
        report.outcomes.len(),
        2,
        "public report retains only static nodes"
    );
    assert!(matches!(
        report.outcome("root.owner"),
        Some(NodeOutcome::Completed { .. })
    ));
    let state = host.state.lock().unwrap();
    assert_eq!(state.peak, 1);
    assert_eq!(state.terminal.len(), 3);
    assert!(
        matches!(&state.terminal[1].1,NodeOutcome::Failed{error,..} if error=="retained failure")
    );
    assert!(
        state.events.iter().position(|v| v == "fold:all").unwrap()
            < state
                .events
                .iter()
                .position(|v| v == "run:root.after")
                .unwrap()
    );
}

#[test]
fn owned_children_share_the_existing_wave_and_publish_in_registered_order() {
    let plan = plan();
    let mut host = Host::new(2);
    host.reverse = true;
    let report = Scheduler::new(&plan, 2).run(&host);
    assert!(matches!(
        report.outcome("root.after"),
        Some(NodeOutcome::Completed { .. })
    ));
    let state = host.state.lock().unwrap();
    assert_eq!(state.peak, 2);
    let index = |value: &str| {
        state
            .events
            .iter()
            .position(|event| event == value)
            .unwrap()
    };
    assert!(index("done:root.owner.slice1") < index("done:root.owner.slice0"));
    assert!(index("publish:root.owner.slice0") < index("publish:root.owner.slice1"));
    assert_eq!(
        state
            .terminal
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["root.owner.slice0", "root.owner.slice1"]
    );
}

#[test]
fn owned_children_obey_existing_scope_capacity_and_empty_sets_fold() {
    for count in [0, 3] {
        let plan = plan();
        let host = Host::new(count);
        let report = Scheduler::new(&plan, 4)
            .with_scope_limits(BTreeMap::from([("root.owner".into(), 1)]))
            .unwrap()
            .run(&host);
        assert!(matches!(
            report.outcome("root.owner"),
            Some(NodeOutcome::Completed { .. })
        ));
        let state = host.state.lock().unwrap();
        assert_eq!(state.terminal.len(), count);
        assert!(state.peak <= 1);
    }
}

#[test]
fn an_owned_expansion_cannot_replace_a_static_or_foreign_node() {
    let plan = plan();
    let mut host = Host::new(1);
    host.invalid = true;
    let report = Scheduler::new(&plan, 4).run(&host);
    assert!(matches!(
        report.outcome("root.owner"),
        Some(NodeOutcome::Failed { .. })
    ));
    assert!(
        host.state
            .lock()
            .unwrap()
            .events
            .iter()
            .all(|v| !v.starts_with("run:"))
    );
}
