//! Worker warm layers, package P4: Cold Closeout.
//!
//! A Round is only known to be the closing Round after its results are reduced, so a
//! Round-level rule cannot be scheduled. Cold Closeout is compiled instead: the pinned
//! convergence policy names, at load time, the exact warm reviewers that owe a cold
//! confirmation, and each of them reserves its confirmation Attempt **before its warm Attempt
//! runs**. That reservation is what "protected" means here — no retry, no sibling node and no
//! later Attempt can consume it, because it was taken first and is released only when the
//! Round is known not to need it.
//!
//! The confirmation itself is conditional: it is dispatched only when the warm result would
//! otherwise close the Round clean. A warm result that already reports a claim at or above the
//! convergence gate blocks the Round on its own, and nothing about a cold second opinion would
//! change that; the reservation is released and no Attempt runs.
//!
//! Both results reach the Ledger before the convergence decision. The cold Attempt is an
//! Attempt of the same node, carrying no warm layer of any kind — no Notes, no Head Delta, no
//! Session Snapshot, no Build Cache — and its result is folded through
//! `ColdCloseoutDispatched@1`, which names the exact warm result artifact its own edge
//! delivered. The Ledger therefore still reduces what its edges delivered, plus the closure of
//! that delivery, never a global scan of whatever happened to run.
//!
//! The Task host folds a recorded confirmation into the Ledger but does not dispatch one yet.
//! The rules below are kept as the base for that port (ADR-0110).

use review_core::{LegacyStageOutput, Severity};

/// Whether this result, on its own, leaves the Round clean at the pinned severity gate.
///
/// Deliberately one-sided: the confirmation is skipped only when the warm result *certainly*
/// blocks, because skipping it wrongly is what would let convergence close on a warm result
/// alone. A warm result that merely corroborates a prior claim still gets its cold Attempt.
pub fn would_close_clean(output: &LegacyStageOutput, gate: Severity) -> bool {
    output
        .findings
        .iter()
        .all(|finding| finding.severity.rank() < gate.rank())
}

/// The Attempt ledger slot the confirmation runs in. It is an accounting name, never a node: the
/// durable record says the Attempt belongs to the reviewer, and keeping the slot separate is
/// what stops a confirmation from fencing or replacing the warm Attempt it confirms.
pub fn closeout_slot(node_id: &str) -> String {
    format!("{node_id}#cold-closeout")
}

/// The failure reason a confirmation that produced no admissible result records: bounded,
/// and never empty.
pub fn bounded_reason(error: &str) -> String {
    let mut reason: String = error.chars().take(480).collect();
    if reason.trim().is_empty() {
        reason = "the Cold Closeout Attempt produced no admissible result".into();
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::legacy::{LegacyFinding, LegacyVerdict};

    fn output(severities: &[Severity]) -> LegacyStageOutput {
        LegacyStageOutput {
            verdict: LegacyVerdict::Approve,
            summary: None,
            findings: severities
                .iter()
                .map(|severity| LegacyFinding {
                    severity: *severity,
                    file: "src/lib.rs".into(),
                    line: None,
                    title: "t".into(),
                    body: "b".into(),
                    fix: Some("f".into()),
                    confidence: None,
                    rule_id: None,
                    occurrence_key: None,
                })
                .collect(),
            benchmark_demands: Vec::new(),
            disputes: Vec::new(),
        }
    }

    #[test]
    fn a_confirmation_is_skipped_only_when_the_warm_result_certainly_blocks() {
        assert!(would_close_clean(&output(&[]), Severity::Major));
        assert!(would_close_clean(
            &output(&[Severity::Minor]),
            Severity::Major
        ));
        assert!(
            !would_close_clean(&output(&[Severity::Major]), Severity::Major),
            "a claim at the gate blocks the Round on its own"
        );
        assert!(!would_close_clean(
            &output(&[Severity::Blocker]),
            Severity::Major
        ));
        assert!(
            !would_close_clean(&output(&[Severity::Minor]), Severity::Minor),
            "a lower gate makes a minor claim blocking"
        );
    }

    #[test]
    fn a_closeout_slot_is_never_the_node_it_confirms() {
        assert_eq!(closeout_slot("correctness"), "correctness#cold-closeout");
        assert_ne!(closeout_slot("correctness"), "correctness");
    }

    #[test]
    fn a_recorded_failure_reason_is_bounded_and_never_empty() {
        assert_eq!(bounded_reason("timed out"), "timed out");
        assert!(!bounded_reason("   ").trim().is_empty());
        assert!(bounded_reason(&"x".repeat(2000)).len() <= 512);
    }
}
