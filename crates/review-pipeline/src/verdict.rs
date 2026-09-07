//! Verdict conversion: what one whole run amounts to, and the schema-pinned spelling it is
//! persisted with. The graph's own enums never reach a payload through `Debug`.

use review_core::{MissingNodeV2, RunFailureReasonV3, RunSuppressionReasonV2, RunVerdictV3};
use review_graph::{NodeFailureClass, NodeOutcome, RunReport};
use review_store::{Convergence, Verdict};

/// What one whole run amounts to.
///
/// `Incomplete` exists because a partial review must never pass on the strength of the part
/// that ran: a run where any node failed or was suppressed — a blocked gate, a refused budget
/// reservation or a crashed reviewer reports *which* nodes never contributed and cannot pass
/// the gate, whatever the ledger's findings would have said. This is the owner's exhaustion
/// policy (2026-08-18): finish in-flight work, dispatch nothing new, and fail closed at the
/// verdict rather than by discarding paid work. Budget exhaustion is the terminal
/// `Fail(Exhausted)` exception: it closes the Round while naming the work that could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    Pass,
    Fail(Verdict),
    Incomplete { missing: Vec<(String, String)> },
}

impl RunVerdict {
    pub fn passed(&self) -> bool {
        matches!(self, RunVerdict::Pass)
    }
}

/// The persisted classification of a scheduler suppression. `RunSuppressionReasonV2` is the
/// schema-pinned vocabulary every RunReport since `@2` carries in `outcomes[].reason`; the
/// graph's own enum never reaches a payload through `Debug`.
pub fn run_suppression_reason(reason: review_graph::SuppressionReason) -> RunSuppressionReasonV2 {
    match reason {
        review_graph::SuppressionReason::GateBlocked => RunSuppressionReasonV2::GateBlocked,
        review_graph::SuppressionReason::UpstreamMissing => RunSuppressionReasonV2::UpstreamMissing,
    }
}

/// The string a suppressed node's `missing_nodes[].reason` carries: the same serde name as its
/// `outcomes[].reason`, so one payload cannot spell one suppression two ways.
fn suppression_reason_label(reason: review_graph::SuppressionReason) -> String {
    match serde_json::to_value(run_suppression_reason(reason))
        .expect("RunSuppressionReasonV2 serializes")
    {
        serde_json::Value::String(name) => name,
        other => other.to_string(),
    }
}

/// Combine what ran with what converged. Completeness is checked first: convergence is a
/// statement about the findings that exist, and says nothing about the reviewers that never
/// produced any.
pub fn run_verdict(report: &RunReport, convergence: &Convergence) -> RunVerdict {
    let missing: Vec<(String, String)> = report
        .outcomes
        .iter()
        .filter_map(|(id, outcome)| match outcome {
            NodeOutcome::Completed { .. } => None,
            NodeOutcome::Failed { error, .. } => Some((id.clone(), error.clone())),
            NodeOutcome::Suppressed { reason } => {
                Some((id.clone(), suppression_reason_label(*reason)))
            }
        })
        .collect();
    if report.outcomes.iter().any(|(_, outcome)| {
        matches!(
            outcome,
            NodeOutcome::Failed {
                class: Some(NodeFailureClass::RunBudgetExhausted),
                ..
            }
        )
    }) {
        return RunVerdict::Fail(Verdict::Exhausted);
    }
    if !missing.is_empty() {
        return RunVerdict::Incomplete { missing };
    }
    // A gate can be intentionally observational and gate no downstream node. Its blocked
    // decision still prevents a pass even though there is then no suppressed outcome to make
    // the run incomplete.
    if !report.blocked_gates.is_empty() {
        return RunVerdict::Fail(Verdict::NotConverged);
    }
    match convergence.verdict {
        Verdict::Converged => RunVerdict::Pass,
        other => RunVerdict::Fail(other),
    }
}

pub(crate) fn persisted_verdict(
    verdict: &RunVerdict,
    convergence: &Convergence,
    has_blocked_gates: bool,
) -> Result<RunVerdictV3, String> {
    Ok(match verdict {
        RunVerdict::Pass => RunVerdictV3::Pass,
        RunVerdict::Fail(Verdict::NotConverged) => RunVerdictV3::Fail {
            reason: if convergence.authority_failures_recent > 0
                && convergence.open_blocking == 0
                && convergence.new_recent == 0
                && !has_blocked_gates
            {
                RunFailureReasonV3::AuthorityUnavailable
            } else {
                RunFailureReasonV3::NotConverged
            },
        },
        RunVerdict::Fail(Verdict::Exhausted) => RunVerdictV3::Fail {
            reason: RunFailureReasonV3::Exhausted,
        },
        RunVerdict::Fail(Verdict::Converged) => {
            return Err("invalid run verdict: converged cannot be a failure".to_string());
        }
        RunVerdict::Incomplete { missing } => RunVerdictV3::Incomplete {
            missing_nodes: missing
                .iter()
                .map(|(node, reason)| MissingNodeV2 {
                    node: node.clone(),
                    reason: reason.clone(),
                })
                .collect(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_authority_has_a_distinct_durable_failure_reason() {
        let mut convergence = Convergence {
            round: 2,
            open_blocking: 1,
            open_required_demands: 0,
            new_recent: 1,
            authority_failures_recent: 1,
            verdict: Verdict::NotConverged,
        };
        assert_eq!(
            persisted_verdict(
                &RunVerdict::Fail(Verdict::NotConverged),
                &convergence,
                false,
            )
            .unwrap(),
            RunVerdictV3::Fail {
                reason: RunFailureReasonV3::NotConverged
            },
            "real finding blockers remain the immediate durable cause"
        );

        convergence.open_blocking = 0;
        convergence.new_recent = 0;
        assert_eq!(
            persisted_verdict(
                &RunVerdict::Fail(Verdict::NotConverged),
                &convergence,
                false,
            )
            .unwrap(),
            RunVerdictV3::Fail {
                reason: RunFailureReasonV3::AuthorityUnavailable
            }
        );

        assert_eq!(
            persisted_verdict(&RunVerdict::Fail(Verdict::NotConverged), &convergence, true,)
                .unwrap(),
            RunVerdictV3::Fail {
                reason: RunFailureReasonV3::NotConverged
            },
            "an explicit blocked gate is the immediate durable cause"
        );
    }
}
