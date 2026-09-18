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

use review_core::{
    ColdCloseoutDispatchedPayloadV1, EventType, LegacyStageOutput, ReviewerResultContract, Severity,
};
use review_graph::{ArtifactMap, Node};
use review_runner::ReviewerAdapter;
use review_sandbox::Mode;
use review_store::NewEvent;

use super::{Kernel, reviewer_inputs, reviewer_output, reviewer_work};

/// Whether this result, on its own, leaves the Round clean at the pinned severity gate.
///
/// Deliberately one-sided: the confirmation is skipped only when the warm result *certainly*
/// blocks, because skipping it wrongly is what would let convergence close on a warm result
/// alone. A warm result that merely corroborates a prior claim still gets its cold Attempt.
pub(crate) fn would_close_clean(output: &LegacyStageOutput, gate: Severity) -> bool {
    output
        .findings
        .iter()
        .all(|finding| finding.severity.rank() < gate.rank())
}

impl Kernel<'_> {
    /// Whether this node owes a compiled cold confirmation under the pinned policy.
    pub(crate) fn has_cold_closeout(&self, node_id: &str) -> bool {
        self.cold_closeout_nodes.contains(node_id)
    }

    /// Take the confirmation Attempt's reservation before the node's warm Attempt is reserved,
    /// once per node per kernel run. An infeasible policy refuses here, before the warm Attempt
    /// spends anything, rather than after a clean result has already been produced.
    pub(crate) fn reserve_cold_closeout(&self, node_id: &str) -> Result<(), String> {
        if !self.has_cold_closeout(node_id) {
            return Ok(());
        }
        let mut held = self
            .closeout_reservations
            .lock()
            .expect("closeout reservations");
        if held.contains_key(node_id) {
            return Ok(());
        }
        let Some(budgets) = &self.budgets else {
            held.insert(node_id.to_string(), None);
            return Ok(());
        };
        let amount = self.attempt_reservation(node_id, budgets);
        // The confirmation reserves under its own slot, never the reviewer's node scope: a node
        // that declared its own Attempt cap is limited there to the one Attempt the pipeline
        // sized for it, and a confirmation is an extra Attempt the run cap pays for.
        let reservation = budgets
            .ledger
            .lock()
            .expect("budget ledger")
            .reserve(
                &[
                    review_attempt::BudgetScope::Node(closeout_slot(node_id)),
                    review_attempt::BudgetScope::Run,
                ],
                amount,
            )
            .map_err(|error| {
                format!(
                    "node `{node_id}` cannot protect its Cold Closeout reservation: {error}; raise the run cap or set cold_closeout = \"none\""
                )
            })?;
        held.insert(node_id.to_string(), Some(reservation));
        Ok(())
    }

    /// Give the protected reservation back when the Round turns out not to need it: the warm
    /// result already blocks, the confirmation ran, or the node failed and there is no clean
    /// result to confirm.
    pub(crate) fn release_cold_closeout(&self, node_id: &str) {
        let held = self
            .closeout_reservations
            .lock()
            .expect("closeout reservations")
            .remove(node_id);
        if let (Some(budgets), Some(Some(reservation))) = (&self.budgets, held) {
            budgets
                .ledger
                .lock()
                .expect("budget ledger")
                .release(&reservation);
        }
    }

    /// Dispatch the compiled cold confirmation for one would-be-clean warm result, or release
    /// its protected reservation when the Round already has a reason not to be clean.
    ///
    /// The whole outcome is published as one `ColdCloseoutDispatched@1`, so a crash never leaves
    /// a dispatch without a result. A resumed Round that already holds the record dispatches
    /// nothing again.
    // One exact Attempt boundary; grouping the arguments would obscure what the confirmation is
    // allowed to see, which is the whole point of a cold Attempt.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_cold_closeout(
        &self,
        node: &Node,
        node_inputs: &ArtifactMap,
        adapter: &dyn ReviewerAdapter,
        warm_attempt_id: &str,
        warm_result_artifact: &str,
        warm_output: &LegacyStageOutput,
        result_contract: ReviewerResultContract,
    ) -> Result<(), String> {
        let node_id = node.id.as_str();
        if !self.has_cold_closeout(node_id) {
            return Ok(());
        }
        if self.recorded_closeout(node_id)?.is_some() {
            return Ok(());
        }
        if !would_close_clean(warm_output, self.convergence_gate) {
            self.release_cold_closeout(node_id);
            return Ok(());
        }
        let reservation = self
            .closeout_reservations
            .lock()
            .expect("closeout reservations")
            .remove(node_id)
            .flatten();
        let reserved_tokens = reservation.as_ref().map(|reservation| reservation.amount);
        let cold_attempt = self
            .attempts
            .lock()
            .expect("attempt ledger")
            .dispatch(&closeout_slot(node_id));
        let charge = |charged: u64| {
            if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                budgets
                    .ledger
                    .lock()
                    .expect("budget ledger")
                    .charge(reservation, charged);
            }
            self.attempts
                .lock()
                .expect("attempt ledger")
                .charge(&cold_attempt, charged);
        };
        let outcome = self.invoke_cold_closeout(
            node,
            node_inputs,
            adapter,
            &cold_attempt.to_string(),
            reserved_tokens,
            result_contract,
        );
        let (cold_result_artifact_id, failed, charged_tokens) = match outcome {
            Ok((artifact, charged)) => (Some(artifact), None, charged),
            // A confirmation that could not answer is recorded as a failure, not forgotten: the
            // Round has no cold confirmation, and the report says so. The reservation is
            // charged in full, exactly as a failed warm Attempt is.
            Err(error) => (
                None,
                Some(bounded_reason(&error)),
                reserved_tokens.unwrap_or(0),
            ),
        };
        charge(charged_tokens);
        let payload = ColdCloseoutDispatchedPayloadV1 {
            node: node_id.to_string(),
            round: self.domain.authority.round,
            warm_attempt_id: warm_attempt_id.to_string(),
            warm_result_artifact_id: warm_result_artifact.to_string(),
            cold_attempt_id: cold_attempt.to_string(),
            reserved_tokens,
            charged_tokens,
            cold_result_artifact_id: cold_result_artifact_id.clone(),
            failed,
        };
        payload.validate()?;
        let mut refs = vec![warm_result_artifact.to_string()];
        refs.extend(cold_result_artifact_id);
        self.domain.append(
            NewEvent::new(
                EventType::ColdCloseoutDispatchedV1,
                serde_json::to_value(&payload).map_err(|error| error.to_string())?,
            )
            .node(node_id)
            .attempt(cold_attempt.to_string())
            .referencing(refs),
        )
    }

    /// One cold Attempt of the same node: fresh inputs with no warm layer of any kind, a fresh
    /// sandbox, and the result captured as an immutable artifact. It makes no Proposal and
    /// leaves no Notes — node-private layers come from the warm Attempt, and a confirmation is
    /// not a source of state for the next Round.
    fn invoke_cold_closeout(
        &self,
        node: &Node,
        node_inputs: &ArtifactMap,
        adapter: &dyn ReviewerAdapter,
        cold_attempt_id: &str,
        reserved_tokens: Option<u64>,
        result_contract: ReviewerResultContract,
    ) -> Result<(String, u64), String> {
        let node_id = node.id.as_str();
        let binding_node = self.domain.reviewer_binding_node(node_id);
        let mut inputs = reviewer_inputs::prepare(
            self.domain.cas,
            &self.domain.authority,
            self.domain.pipeline_version,
            node,
            node_inputs,
        )?;
        reviewer_inputs::bind_attempt(
            &mut inputs,
            &self.domain.authority,
            &binding_node,
            cold_attempt_id,
            reserved_tokens,
        );
        let sandbox = self.domain.sandbox_for(node_id, Mode::EphemeralWrite)?;
        let invocation =
            reviewer_work::invoke(self.domain.cas, adapter, sandbox.root(), &inputs, None);
        let receipted = match invocation.result {
            Ok(receipted) => receipted,
            Err(reviewer_work::InvocationFailure::Adapter(error)) => {
                return Err(error.to_string());
            }
            Err(reviewer_work::InvocationFailure::Panicked) => {
                return Err(format!("reviewer adapter panicked for node {node_id}"));
            }
        };
        let assigned_finding_ids: Vec<String> = inputs
            .prior_findings
            .as_ref()
            .and_then(|value| value.get("findings"))
            .and_then(serde_json::Value::as_array)
            .map(|findings| {
                findings
                    .iter()
                    .filter_map(|finding| finding.get("finding_id"))
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let returned = receipted.returned;
        let result_value =
            super::reviewer_result_value(&returned.output, result_contract, &assigned_finding_ids)
                .map_err(|error| error.to_string())?;
        let captured = reviewer_output::capture_result(
            self.domain.cas,
            &self.domain.authority,
            sandbox,
            reviewer_output::ReviewerResultCapture {
                node_id,
                attempt_id: cold_attempt_id,
                result: &result_value,
                result_contract,
                proposal: Ok(None),
                notes: Ok(None),
                notes_max_bytes: None,
                head_manifest: &self.domain.snapshot,
                assigned_finding_ids: &assigned_finding_ids,
                report_count: returned.output.findings.len(),
                cost_tokens: returned.cost_tokens,
                usage: &receipted.usage,
                context_manifest: &receipted.context_manifest,
                raw_artifact: &returned.raw_artifact,
            },
        )?;
        Ok((
            captured.metadata.result_artifact_id,
            returned.cost_tokens.max(receipted.usage.chargeable_tokens),
        ))
    }

    /// The closeout this Round already recorded for the node, if any.
    fn recorded_closeout(
        &self,
        node_id: &str,
    ) -> Result<Option<ColdCloseoutDispatchedPayloadV1>, String> {
        let events = self
            .domain
            .store
            .lock()
            .expect("event store")
            .replay(&self.domain.run_id)
            .map_err(|error| error.to_string())?;
        let Some(event) = events.iter().find(|event| {
            event.event_type == EventType::ColdCloseoutDispatchedV1
                && event.node_id.as_deref() == Some(node_id)
                && event.causation_id.as_deref()
                    == Some(self.domain.authority.round_event_id.as_str())
        }) else {
            return Ok(None);
        };
        let payload: ColdCloseoutDispatchedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
        payload.validate()?;
        Ok(Some(payload))
    }
}

/// The Attempt ledger slot the confirmation runs in. It is an accounting name, never a node: the
/// durable record says the Attempt belongs to the reviewer, and keeping the slot separate is
/// what stops a confirmation from fencing or replacing the warm Attempt it confirms.
pub(crate) fn closeout_slot(node_id: &str) -> String {
    format!("{node_id}#cold-closeout")
}

fn bounded_reason(error: &str) -> String {
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
