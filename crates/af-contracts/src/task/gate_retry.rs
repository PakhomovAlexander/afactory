//! The event `af/TaskGateRetried@1`: `af task run ID --retry-gate` reopened a finished Task
//! whose last round ended at a failed gate, so its check node runs again, in a new check
//! Attempt of the same round, on the same sealed candidate. It is its own versioned event of the
//! Task's log, the kind `TaskGateRetried@1` beside `TaskTransition@5`, whose shape it leaves
//! unchanged (ADR-0145), as `TaskExecutionStarted@1` is (ADR-0153).
//!
//! It grants nothing a plan did not: no new plan, revision or Task, no re-admission, no change to
//! the candidate. It names the finished result it reopens and, for each failed gate, the check
//! node and its published output whose check receipt did not pass; the Store admits it only for
//! exactly that state, by the Task's current lease, while the plan's approval and the Task's
//! budget still hold each new Attempt. The superseded outputs, their Attempts and every Attempt
//! after them stay in the log and in the Task's accounting.

use serde::{Deserialize, Serialize};

use super::{is_name, require, safe_number};
use crate::is_digest;

pub const TASK_GATE_RETRIED_V1: &str = "af/TaskGateRetried@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGateRetriedV1 {
    /// Always [`TASK_GATE_RETRIED_V1`].
    pub schema: String,
    /// The lease that reopened the Task, judged as a transition's writer is.
    pub writer: String,
    pub epoch: u64,
    /// The policy time it was recorded at.
    pub now_unix_ms: u64,
    /// The finished `af/TaskResult@1` the retry reopens.
    pub result_id: String,
    /// Every failed gate that runs again, in graph order.
    pub gates: Vec<RetriedGateV1>,
}

/// One check node that runs again, and its published output it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetriedGateV1 {
    pub node: String,
    /// The node's published `af/TaskOutput@1` whose check receipt did not pass.
    pub output_id: String,
}

impl TaskGateRetriedV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == TASK_GATE_RETRIED_V1,
            "A gate retry needs its schema",
        )?;
        require(
            is_name(&self.writer)
                && self.epoch > 0
                && safe_number(self.epoch)
                && self.now_unix_ms > 0
                && safe_number(self.now_unix_ms)
                && is_digest(&self.result_id)
                && !self.gates.is_empty()
                && self.gates.len() <= 64
                && self.gates.iter().all(|gate| {
                    is_digest(&gate.output_id)
                        && !gate.node.is_empty()
                        && gate.node.len() <= 1024
                        && !gate.node.chars().any(char::is_control)
                })
                && self
                    .gates
                    .iter()
                    .map(|gate| &gate.node)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == self.gates.len(),
            "A gate retry needs a writer, epoch, bounded policy time, its result and each \
             distinct gate's node and output",
        )
    }

    /// The artifacts its event references: the reopened result and the superseded outputs.
    pub fn artifact_refs(&self) -> Vec<String> {
        let mut refs: Vec<String> = std::iter::once(self.result_id.clone())
            .chain(self.gates.iter().map(|gate| gate.output_id.clone()))
            .collect();
        refs.sort();
        refs.dedup();
        refs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retry_names_its_result_node_and_output_and_nothing_else() {
        let digest = |c: char| format!("sha256:{}", c.to_string().repeat(64));
        let retry = TaskGateRetriedV1 {
            schema: TASK_GATE_RETRIED_V1.into(),
            writer: "cli-42".into(),
            epoch: 3,
            now_unix_ms: 1_700_000_000_000,
            result_id: digest('b'),
            gates: vec![RetriedGateV1 {
                node: "root.nodes.gate".into(),
                output_id: digest('a'),
            }],
        };
        retry.validate().unwrap();
        assert_eq!(retry.artifact_refs(), vec![digest('a'), digest('b')]);
        let gate = |node: &str, output: String| RetriedGateV1 {
            node: node.into(),
            output_id: output,
        };
        for broken in [
            TaskGateRetriedV1 {
                schema: "af/TaskGateRetried@2".into(),
                ..retry.clone()
            },
            TaskGateRetriedV1 {
                epoch: 0,
                ..retry.clone()
            },
            TaskGateRetriedV1 {
                gates: Vec::new(),
                ..retry.clone()
            },
            TaskGateRetriedV1 {
                gates: vec![gate("", digest('a'))],
                ..retry.clone()
            },
            TaskGateRetriedV1 {
                gates: vec![gate("root.nodes.gate", "sha256:short".into())],
                ..retry.clone()
            },
            TaskGateRetriedV1 {
                gates: vec![
                    gate("root.nodes.gate", digest('a')),
                    gate("root.nodes.gate", digest('c')),
                ],
                ..retry.clone()
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
        let mut value = serde_json::to_value(&retry).unwrap();
        value["plan_id"] = serde_json::json!(digest('c'));
        assert!(serde_json::from_value::<TaskGateRetriedV1>(value).is_err());
    }
}
