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

use std::time::{Duration, SystemTime};

use review_core::{
    ColdCloseoutDispatchedPayloadV1, EventType, LegacyStageOutput, ReviewerResultContract, Severity,
};
use review_graph::{ArtifactMap, Node};
use review_runner::TokenUsage;
use review_sandbox::Mode;
use review_store::NewEvent;

use super::{Kernel, reviewer_inputs, reviewer_output, reviewer_work};

/// What one cold confirmation produced, in the shape its Attempt lifecycle records.
struct ColdConfirmation {
    result_artifact: String,
    provenance_artifact: String,
    raw_artifact: String,
    /// The exact inputs the confirmation ran on: the same ones its warm Attempt received.
    input_artifacts: Vec<String>,
    cost_tokens: u64,
    usage: TokenUsage,
    started: SystemTime,
    elapsed: Duration,
}

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

    /// Every compiled cold confirmation this Round owes, decided once every warm result is in.
    ///
    /// The condition is the Round's, not one reviewer's: a confirmation is skipped exactly when
    /// some warm result of this Round already carries a claim at or above the convergence gate,
    /// because the Round then blocks whatever a second opinion says. It stays deliberately
    /// one-sided. A prior-Round claim that this Round's reduction may or may not close is not
    /// read here, so a confirmation is dispatched when in doubt and never skipped on a guess:
    /// skipping it wrongly is what would let convergence close on warm results alone.
    pub(crate) fn run_round_closeouts(&self) -> Result<(), String> {
        if self.cold_closeout_nodes.is_empty() {
            return Ok(());
        }
        let selections: Vec<(String, super::SelectedReviewer)> = self
            .domain
            .reviewer_selections
            .lock()
            .expect("reviewer selections")
            .iter()
            .map(|(node, selection)| (node.clone(), selection.clone()))
            .collect();
        let mut warm = Vec::with_capacity(selections.len());
        for (node, selection) in selections {
            let value = self
                .domain
                .cas
                .get_json(&selection.result_artifact)
                .map_err(|error| error.to_string())?;
            let (contract, output) = super::reviewer_stage_output(value)
                .map_err(|error| format!("artifact {}: {error}", selection.result_artifact))?;
            warm.push((node, selection, contract, output));
        }
        let round_closes_clean = warm
            .iter()
            .all(|(_, _, _, output)| would_close_clean(output, self.convergence_gate));
        for (node, selection, contract, _) in warm {
            if !self.has_cold_closeout(&node) {
                continue;
            }
            if !round_closes_clean {
                self.release_cold_closeout(&node);
                continue;
            }
            self.run_cold_closeout(&node, &selection, contract)?;
        }
        Ok(())
    }

    /// One compiled cold confirmation: a second Attempt of the same node, carrying no warm
    /// layer of any kind, run through the same durable Attempt lifecycle as the warm Attempt it
    /// confirms. Its dispatch is durable before the provider is called, so a crash between them
    /// leaves an Attempt the next kernel run fences and charges rather than an invisible spend,
    /// and `ColdCloseoutDispatched@1` records the whole outcome with the exact warm Attempt and
    /// result it closes over. A Round that already holds that record for this warm Attempt
    /// dispatches nothing again.
    fn run_cold_closeout(
        &self,
        node_id: &str,
        warm: &super::SelectedReviewer,
        result_contract: ReviewerResultContract,
    ) -> Result<(), String> {
        if self
            .recorded_closeout(node_id)?
            .is_some_and(|recorded| recorded.warm_attempt_id == warm.attempt_id)
        {
            return Ok(());
        }
        let Some((node, node_inputs)) = self
            .closeout_subjects
            .lock()
            .expect("closeout subjects")
            .get(node_id)
            .cloned()
        else {
            return Err(format!(
                "node `{node_id}` owes a cold confirmation but this kernel run holds no exact Worker Input for it"
            ));
        };
        let slot = closeout_slot(node_id);
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
            .dispatch(&slot);
        self.domain.append(
            NewEvent::new(
                EventType::AttemptDispatchedV1,
                serde_json::to_value(review_core::event::AttemptDispatchedPayloadV1 {
                    reserved: reserved_tokens,
                    prior_findings: None,
                })
                .map_err(|error| error.to_string())?,
            )
            .node(&slot)
            .attempt(cold_attempt.to_string()),
        )?;

        let confirmation = self.invoke_cold_closeout(
            &node,
            &node_inputs,
            &cold_attempt.to_string(),
            reserved_tokens,
            result_contract,
        );
        let (cold_result_artifact_id, failed, charged_tokens) =
            match confirmation {
                Ok(confirmation) => {
                    self.record_attempt_wall(
                        &slot,
                        &cold_attempt,
                        confirmation.started,
                        confirmation.elapsed,
                        Some(&confirmation.usage),
                    );
                    let selection = self.attempts.lock().expect("attempt ledger").admit(
                        &review_attempt::Receipt {
                            attempt: cold_attempt.clone(),
                            output: confirmation.raw_artifact.clone(),
                            cost: confirmation.cost_tokens,
                        },
                    );
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, confirmation.cost_tokens);
                    }
                    self.domain.append(
                        NewEvent::new(
                            EventType::AttemptAdmittedV1,
                            serde_json::to_value(review_core::event::AttemptAdmittedPayloadV1 {
                                selection: match selection {
                                    review_attempt::Selection::Selected => "selected",
                                    review_attempt::Selection::Quarantined => "quarantined",
                                }
                                .to_string(),
                                cost_tokens: confirmation.cost_tokens,
                                result_artifact: Some(confirmation.result_artifact.clone()),
                                provenance_artifact: Some(confirmation.provenance_artifact.clone()),
                            })
                            .map_err(|error| error.to_string())?,
                        )
                        .node(&slot)
                        .attempt(cold_attempt.to_string())
                        .referencing(vec![
                            confirmation.result_artifact.clone(),
                            confirmation.provenance_artifact.clone(),
                            confirmation.raw_artifact.clone(),
                        ]),
                    )?;
                    // The confirmation is an Attempt of its own slot, never the reviewer's
                    // selection: the Ledger folds it as its own stage, and the record below says
                    // which warm result it closes over.
                    self.domain
                        .reviewer_selections
                        .lock()
                        .expect("reviewer selections")
                        .insert(
                            slot.clone(),
                            super::SelectedReviewer {
                                attempt_id: cold_attempt.to_string(),
                                result_artifact: confirmation.result_artifact.clone(),
                                proposal_candidate: None,
                            },
                        );
                    self.domain
                        .reviewer_input_artifacts
                        .lock()
                        .expect("reviewer inputs")
                        .insert(slot.clone(), confirmation.input_artifacts.clone());
                    (
                        Some(confirmation.result_artifact),
                        None,
                        confirmation.cost_tokens,
                    )
                }
                // A confirmation that could not answer is recorded as a failure, not forgotten: the
                // Round has no cold confirmation, the Ledger refuses to reduce without it, and the
                // reservation is charged in full exactly as a failed warm Attempt is.
                Err(error) => {
                    let charged = reserved_tokens.unwrap_or(0);
                    self.fail_started_attempt(
                        &slot,
                        &cold_attempt,
                        reservation.as_ref(),
                        &error,
                        charged,
                        super::AttemptFailureEvidence::default(),
                    )?;
                    (None, Some(bounded_reason(&error)), charged)
                }
            };
        let payload = ColdCloseoutDispatchedPayloadV1 {
            node: node_id.to_string(),
            round: self.domain.authority.round,
            warm_attempt_id: warm.attempt_id.clone(),
            warm_result_artifact_id: warm.result_artifact.clone(),
            cold_attempt_id: cold_attempt.to_string(),
            reserved_tokens,
            charged_tokens,
            cold_result_artifact_id: cold_result_artifact_id.clone(),
            failed,
        };
        payload.validate()?;
        let mut refs = vec![warm.result_artifact.clone()];
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
        cold_attempt_id: &str,
        reserved_tokens: Option<u64>,
        result_contract: ReviewerResultContract,
    ) -> Result<ColdConfirmation, String> {
        let node_id = node.id.as_str();
        let binding_node = self.domain.reviewer_binding_node(node_id);
        let adapter = self
            .reviewers
            .get(&binding_node)
            .ok_or_else(|| format!("no reviewer bound to node {node_id}"))?;
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
        let invocation = reviewer_work::invoke(
            self.domain.cas,
            adapter.as_ref(),
            sandbox.root(),
            &inputs,
            None,
        );
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
        Ok(ColdConfirmation {
            result_artifact: captured.metadata.result_artifact_id,
            provenance_artifact: captured.metadata.provenance_artifact_id,
            raw_artifact: returned.raw_artifact,
            input_artifacts: super::artifact_ids(node_inputs),
            cost_tokens: returned.cost_tokens.max(receipted.usage.chargeable_tokens),
            usage: receipted.usage,
            started: invocation.started,
            elapsed: invocation.elapsed,
        })
    }

    /// The closeout this Round already recorded for the node, if any. The caller compares its
    /// warm Attempt with the one in hand, so a record left by a superseded warm Attempt never
    /// suppresses the confirmation its replacement owes.
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
