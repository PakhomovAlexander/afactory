//! Read-only fleet observations and the client briefing that projects them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Connect,
    Infra,
    Status,
    Allocations,
    Cluster,
    Inbox,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Run,
    AskOwner,
    Wait,
    Repair,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextAction {
    pub action: Action,
    pub reason: String,
    pub success_condition: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Unconfigured,
    Ready,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale,
    Unavailable,
    Denied,
    IdentityMismatch,
    MissingCredential,
    InvalidResponse,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub source: String,
    pub subject: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub revision: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub observed_unix_ms: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub age_ms: Option<u64>,
    pub freshness: Freshness,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Reading<T> {
    pub observation: Observation,
    #[serde(deserialize_with = "Option::deserialize")]
    pub data: Option<T>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capacity {
    pub cpu_ids: Vec<u32>,
    pub ram_gib: f64,
    pub ssd_gib: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub node: String,
    pub available: Capacity,
    #[serde(deserialize_with = "Option::deserialize")]
    pub static_available: Option<Capacity>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub physical_totals: Option<Capacity>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub reservations: Option<Capacity>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pool {
    pub name: String,
    pub static_pool: bool,
    pub dynamic_pool: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub available_gib: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub hosts: Vec<Host>,
    /// Storage pools identified by the provider. An empty list means none were reported.
    pub pools: Vec<Pool>,
    pub release: ReleasePolicy,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleasePolicy {
    Explicit,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub vmid: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub name: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub node: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub power: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub state: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub charged: Option<Capacity>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub executor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDecision {
    pub task_revision_id: String,
    pub plan_id: String,
    pub developer: String,
    /// Exact prepared experiment, when this Decision concerns a child plan.
    #[serde(deserialize_with = "Option::deserialize")]
    pub prepared_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitingTask {
    pub task_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub placement: Option<String>,
    pub phase: WaitingPhase,
    pub reason: crate::task::TaskWaitingReasonV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub decision: Option<PlanDecision>,
    pub observation: Observation,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingPhase {
    Waiting,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOrigin {
    pub origin: String,
    pub project_id: String,
    pub store_id: String,
    pub principal: String,
    pub tasks: Vec<WaitingTask>,
}

pub const ORIGIN_OBSERVATION_V1: &str = "af/fleet-origin@1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginObservationV1 {
    pub schema: String,
    pub scope: String,
    pub observed_unix_ms: u64,
    pub origin: ProjectOrigin,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterState {
    Absent,
    #[default]
    Unknown,
}

pub const FLEET_BRIEFING_V1: &str = "af/fleet-briefing@1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefingV1 {
    pub schema: String,
    pub view: View,
    #[serde(deserialize_with = "Option::deserialize")]
    pub context: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub scope: Option<String>,
    pub configured_contexts: Vec<String>,
    pub discovery: Readiness,
    pub cluster: ClusterState,
    pub distributed_execution_ready: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub desired_footprint: Option<Capacity>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub inventory: Option<Reading<Inventory>>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub allocations: Option<Reading<Vec<Allocation>>>,
    pub origins: Vec<Reading<ProjectOrigin>>,
    pub next_action: NextAction,
}
