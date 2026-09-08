//! One reviewer Attempt's lifecycle: reservation and durable dispatch, release, failure with the
//! retry feedback the next Attempt receives as input, and the buffered node events flushed in
//! canonical order.

use review_attempt::{AttemptId, BudgetScope, Reservation};
use review_core::EventType;
use review_core::event::{AttemptFeedbackPayloadV1, AttemptInputPayloadV1};
use review_graph::NodeFailureClass;
use review_store::NewEvent;

use crate::kernel::{Kernel, PreparedReviewerAttempt};

#[derive(Default)]
pub(crate) struct AttemptFailureEvidence<'a> {
    pub(crate) raw_artifact: Option<&'a str>,
    pub(crate) refusal_history: Option<&'a [String]>,
}

impl Kernel<'_> {
    pub(crate) fn prepare_reviewer_attempt(
        &self,
        node_id: &str,
        prior_findings_artifact: Option<&String>,
        prior_failures: &[String],
    ) -> Result<PreparedReviewerAttempt, String> {
        let refusal_history_id = (!prior_failures.is_empty())
            .then(|| {
                let value =
                    serde_json::to_value(prior_failures).map_err(|error| error.to_string())?;
                self.cas.put_json(&value).map_err(|error| error.to_string())
            })
            .transpose()?;
        let reservation = match &self.budgets {
            Some(budgets) => {
                let base = self.reviewer_binding_node(node_id);
                let mut scopes = vec![BudgetScope::Node(node_id.to_string())];
                if base != node_id {
                    scopes.push(BudgetScope::FanOut(base));
                }
                scopes.push(BudgetScope::Run);
                let amount = self.attempt_reservation(node_id, budgets);
                let result = budgets
                    .ledger
                    .lock()
                    .expect("budget ledger")
                    .reserve(&scopes, amount);
                Some(result.map_err(|error| {
                    if error.scope == BudgetScope::Run {
                        self.failure_classes
                            .lock()
                            .expect("failure classes")
                            .insert(node_id.to_string(), NodeFailureClass::RunBudgetExhausted);
                    }
                    if prior_failures.is_empty() {
                        format!("never dispatched: {error}")
                    } else {
                        format!("{}; retry refused: {error}", prior_failures.join("; "))
                    }
                })?)
            }
            None => None,
        };
        let attempt = self
            .attempts
            .lock()
            .expect("attempt ledger")
            .dispatch(node_id);
        let dispatch = NewEvent::new(
            EventType::AttemptDispatchedV1,
            serde_json::json!({
                "reserved": reservation.as_ref().map(|reservation| reservation.amount),
                "prior_findings": prior_findings_artifact,
            }),
        )
        .node(node_id)
        .attempt(attempt.to_string())
        .referencing(prior_findings_artifact.cloned().into_iter().collect());
        let mut events = Vec::with_capacity(2);
        if let Some(refusal_history_id) = &refusal_history_id {
            events.push(
                NewEvent::new(
                    EventType::AttemptInputV1,
                    serde_json::to_value(AttemptInputPayloadV1 {
                        refusal_history_id: refusal_history_id.clone(),
                    })
                    .map_err(|error| error.to_string())?,
                )
                .node(node_id)
                .attempt(attempt.to_string())
                .referencing(vec![refusal_history_id.clone()]),
            );
        }
        events.push(dispatch);
        if let Err(error) = self.append_batch(&events) {
            if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                budgets
                    .ledger
                    .lock()
                    .expect("budget ledger")
                    .release(reservation);
            }
            self.attempts.lock().expect("attempt ledger").fence(node_id);
            return Err(error);
        }
        Ok(PreparedReviewerAttempt {
            attempt,
            reservation,
            refusal_history_id,
        })
    }

    pub(crate) fn release_prepared_attempt(
        &self,
        node_id: &str,
        attempt: &AttemptId,
        reservation: Option<&Reservation>,
        error: &str,
    ) -> Result<(), String> {
        if let (Some(budgets), Some(reservation)) = (&self.budgets, reservation) {
            budgets
                .ledger
                .lock()
                .expect("budget ledger")
                .release(reservation);
        }
        self.attempts.lock().expect("attempt ledger").fence(node_id);
        self.append(
            NewEvent::new(
                EventType::AttemptReleasedV1,
                serde_json::json!({
                    "error": error,
                    "released": reservation.map(|reservation| reservation.amount),
                }),
            )
            .node(node_id)
            .attempt(attempt.to_string()),
        )
    }

    pub(crate) fn fail_started_attempt(
        &self,
        node_id: &str,
        attempt: &AttemptId,
        reservation: Option<&Reservation>,
        error: &str,
        charged: u64,
        evidence: AttemptFailureEvidence<'_>,
    ) -> Result<(), String> {
        if let (Some(budgets), Some(reservation)) = (&self.budgets, reservation) {
            budgets
                .ledger
                .lock()
                .expect("budget ledger")
                .charge(reservation, charged);
        }
        self.attempts
            .lock()
            .expect("attempt ledger")
            .charge(attempt, charged);
        let mut event = NewEvent::new(
            EventType::AttemptFailedV1,
            serde_json::json!({ "error": error, "charged": charged }),
        )
        .node(node_id)
        .attempt(attempt.to_string());
        if let Some(raw_artifact) = evidence.raw_artifact {
            event = event.referencing(vec![raw_artifact.to_string()]);
        }
        let mut events = vec![event];
        if let Some(refusal_history) = evidence.refusal_history {
            events.push(self.feedback_event(node_id, attempt, refusal_history)?);
        }
        self.append_batch(&events)
    }

    pub(crate) fn feedback_event(
        &self,
        node_id: &str,
        attempt: &AttemptId,
        refusal_history: &[String],
    ) -> Result<NewEvent, String> {
        let refusal_history_id = self
            .cas
            .put_json(&serde_json::to_value(refusal_history).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        Ok(NewEvent::new(
            EventType::AttemptFeedbackV1,
            serde_json::to_value(AttemptFeedbackPayloadV1 {
                refusal_history_id: refusal_history_id.clone(),
            })
            .map_err(|error| error.to_string())?,
        )
        .node(node_id)
        .attempt(attempt.to_string())
        .referencing(vec![refusal_history_id]))
    }

    /// Hold a reviewer-thread event for the canonical-order flush. See `reviewer_events`.
    pub(crate) fn buffer_reviewer_event(&self, node_id: &str, event: NewEvent) {
        let mut seq = self.reviewer_event_seq.lock().expect("reviewer event seq");
        let key = (node_id.to_string(), *seq);
        *seq += 1;
        self.reviewer_events
            .lock()
            .expect("reviewer events")
            .push((key, event));
    }

    /// Append every still-buffered node event, sorted by `(node, emission order)`, then clear the
    /// buffer. This is the terminal drain, and it is only correct where nothing can still reach
    /// `record_outputs`: the run's own report. Mid-run callers use
    /// [`flush_orphaned_reviewer_events`](Self::flush_orphaned_reviewer_events), because
    /// `record_outputs` is the sole publisher for any node that reaches admission and a mid-run
    /// whole-buffer drain would split a still-running sibling's lifecycle from its receipt. It
    /// does not reorder already-durable dispatch/failure events or successful node receipts
    /// across concurrent nodes. Idempotent: a second call on an already-drained buffer is a
    /// no-op.
    pub fn flush_reviewer_events(&self) -> Result<(), String> {
        let mut pending = self.reviewer_events.lock().expect("reviewer events");
        pending.sort_by(|a, b| a.0.cmp(&b.0));
        let events: Vec<NewEvent> = pending.iter().map(|(_, event)| event.clone()).collect();
        self.append_batch(&events)?;
        pending.clear();
        Ok(())
    }

    /// Publish, in `(node, emission order)`, only what belongs to nodes that failed — the nodes
    /// that will never reach `record_outputs`, so nothing else would ever publish it. Every
    /// other node's buffered events stay buffered until its own receipt commits them in one
    /// transaction. Idempotent; a no-op when nothing has failed.
    ///
    /// A node-scoped drain is the whole point. Slots are refilled as they free, so while this
    /// runs a plan-later sibling may be sitting completed and unadmitted with its
    /// `AttemptAdmitted@1` buffered. Draining that here would append it in *this* node's batch
    /// and leave the sibling's receipt to land alone: a crash between the two appends leaves a
    /// durable selected Attempt with no receipt, and the Round becomes unresumable.
    pub(crate) fn flush_orphaned_reviewer_events(&self) -> Result<(), String> {
        let failed = self.failed_nodes.lock().expect("failed nodes").clone();
        if failed.is_empty() {
            return Ok(());
        }
        let mut pending = self.reviewer_events.lock().expect("reviewer events");
        pending.sort_by(|a, b| a.0.cmp(&b.0));
        let events: Vec<NewEvent> = pending
            .iter()
            .filter(|((node, _), _)| failed.contains(node))
            .map(|(_, event)| event.clone())
            .collect();
        if events.is_empty() {
            return Ok(());
        }
        self.append_batch(&events)?;
        pending.retain(|((node, _), _)| !failed.contains(node));
        Ok(())
    }
}

pub(crate) fn failed_retry_context(
    attempt: &str,
    failure_class: &str,
    rejection_code: Option<&str>,
) -> String {
    match rejection_code {
        Some(code) => {
            format!("attempt {attempt} returned an invalid result: {failure_class}:{code}")
        }
        None => format!("attempt {attempt} returned an invalid result: {failure_class}"),
    }
}

pub(crate) fn fenced_retry_context(attempt: &str, reason: &str) -> String {
    format!("attempt {attempt} {reason}")
}
