//! The typed pipeline graph.
//!
//! A pipeline is a DAG of nodes with **named typed ports**, not a list of stages. Two properties
//! follow from that, and neither is available to a script:
//!
//! - **Nothing ambient.** A node sees exactly what an edge hands it. It cannot query the ledger,
//!   read a sibling's output file, or pick up whatever the orchestrator happened to leave in
//!   scope. Prior claims rendered ad hoc into a prompt would exist only inside a subagent's
//!   context, unreconstructable afterwards from any artifact.
//! - **A failed gate suppresses dispatch.** Not "the orchestrator remembers not to continue":
//!   planning resolves every gate a node depends on, directly or through an ancestor, and the
//!   Review compiler turns each into a Task condition, so nothing behind a blocked gate is
//!   dispatched.
//!
//! Planning happens before anything runs. A cycle, a dangling dependency, or an edge to a port a
//! node does not declare is a planning failure — the graph never starts, rather than failing
//! halfway with some nodes already dispatched.

pub mod plan;
pub mod schedule;
pub mod task;

pub use plan::{
    Edge, Node, NodeKind, Pipeline, PlanError, Planned, Port, PortCardinality, PortContract,
    SnapshotAffinity,
};
pub use schedule::{
    ArtifactMap, Dispatch, NodeFailureClass, NodeOutcome, OwnedChildDispatch, RunReport, Scheduler,
    SuppressionReason,
};
