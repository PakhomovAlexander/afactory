//! The typed pipeline graph.
//!
//! A pipeline is a DAG of nodes with **named typed ports**, not a list of stages. Two properties
//! follow from that, and neither is available to a script:
//!
//! - **Nothing ambient.** A node sees exactly what an edge hands it. It cannot query the ledger,
//!   read a sibling's output file, or pick up whatever the orchestrator happened to leave in
//!   scope. The shell harness passed prior claims by rendering them into a prompt, which meant
//!   what a reviewer received existed only inside a subagent's context — unreconstructable
//!   afterwards from any artifact.
//! - **A failed gate suppresses dispatch.** Not "the orchestrator remembers not to continue":
//!   gated nodes are structurally unreachable once their gate blocks, and the test asserts they
//!   left no events and no artifacts behind.
//!
//! Planning happens before anything runs. A cycle, a dangling dependency, or an edge to a port a
//! node does not declare is a planning failure — the graph never starts, rather than failing
//! halfway with some nodes already dispatched.

pub mod plan;
pub mod schedule;

pub use plan::{Edge, Node, NodeKind, Pipeline, PlanError, Planned, Port};
pub use schedule::{Dispatch, NodeOutcome, RunReport, Scheduler, SuppressionReason};
