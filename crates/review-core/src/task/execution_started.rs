//! The marker `af/TaskExecutionStarted@1` (ADR-0153, design §5 "Retained history"): a lease
//! began kernel execution of its Task. It is its own versioned event of the Task's log, the
//! kind `TaskExecutionStarted@1` beside `TaskTransition@5`, whose shape it leaves unchanged
//! (ADR-0145). The Task runtime commits it once per lease, before it does anything else under
//! that lease; it references no artifact, never changes the Task's result, and the Store
//! refuses it on a finished Task. A lease that only planned (`af task start` without
//! `--execute`), decided, refreshed or swept never writes one, so the markers before a lease
//! count exactly the executions before it: Telemetry numbers its executions from them. It is
//! written identically at every telemetry level; the Store never learns the level.

use serde::{Deserialize, Serialize};

use super::{is_name, require, safe_number};

pub const TASK_EXECUTION_STARTED_V1: &str = "af/TaskExecutionStarted@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskExecutionStartedV1 {
    /// Always [`TASK_EXECUTION_STARTED_V1`].
    pub schema: String,
    /// The lease that began execution: its writer and epoch, judged against the Task's current
    /// lease exactly as a transition's are.
    pub writer: String,
    pub epoch: u64,
    /// The policy time it was recorded at.
    pub now_unix_ms: u64,
}

impl TaskExecutionStartedV1 {
    /// The marker of the lease `writer` at `epoch`, recorded at `now_unix_ms`.
    pub fn new(writer: impl Into<String>, epoch: u64, now_unix_ms: u64) -> Self {
        Self {
            schema: TASK_EXECUTION_STARTED_V1.into(),
            writer: writer.into(),
            epoch,
            now_unix_ms,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_EXECUTION_STARTED_V1,
            "An execution marker needs its schema",
        )?;
        require(
            is_name(&self.writer)
                && self.epoch > 0
                && safe_number(self.epoch)
                && self.now_unix_ms > 0
                && safe_number(self.now_unix_ms),
            "An execution marker needs a writer, epoch and bounded policy time",
        )
    }
}
