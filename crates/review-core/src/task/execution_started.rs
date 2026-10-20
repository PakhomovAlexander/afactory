//! The marker `af/TaskExecutionStarted@1` (ADR-0153, design §5 "Retained history"): a lease
//! began kernel execution of its Task. The Task runtime commits it once per lease, before it
//! does anything else under that lease, in one `execution_started` transition of the Task's log
//! that references no artifact and never changes the Task's result. A lease that only planned
//! (`af task start` without `--execute`), decided, refreshed or swept never writes one, so the
//! markers before a lease count exactly the executions before it: Telemetry numbers its
//! executions from them. It is written identically at every telemetry level; the Store never
//! learns the level.

use serde::{Deserialize, Serialize};

use super::require;

pub const TASK_EXECUTION_STARTED_V1: &str = "af/TaskExecutionStarted@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskExecutionStartedV1 {
    /// Always [`TASK_EXECUTION_STARTED_V1`].
    pub schema: String,
}

impl TaskExecutionStartedV1 {
    /// The marker of this release.
    pub fn new() -> Self {
        Self {
            schema: TASK_EXECUTION_STARTED_V1.into(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_EXECUTION_STARTED_V1,
            "An execution marker needs its schema",
        )
    }
}

impl Default for TaskExecutionStartedV1 {
    fn default() -> Self {
        Self::new()
    }
}
