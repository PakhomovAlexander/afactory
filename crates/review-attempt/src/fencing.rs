//! Attempt lifecycle and the fencing that makes a late result harmless.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A monotonic epoch. Fencing revokes an epoch; anything arriving under a revoked one is late by
/// definition, whatever its wall-clock timestamp says.
pub type Epoch = u64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AttemptId(pub String);

impl AttemptId {
    /// Derived from node and epoch, never random: replay must reproduce the same identity, and a
    /// random ID would make two otherwise identical runs incomparable.
    pub fn of(node: &str, epoch: Epoch) -> AttemptId {
        AttemptId(format!("{node}#{epoch}"))
    }

    /// Schema-conforming identity scoped to one durable Round event.
    pub fn scoped(round_event_id: &str, node: &str, epoch: Epoch) -> AttemptId {
        let mut hasher = Sha256::new();
        for part in [
            round_event_id.as_bytes(),
            node.as_bytes(),
            &epoch.to_be_bytes(),
        ] {
            hasher.update((part.len() as u64).to_be_bytes());
            hasher.update(part);
        }
        AttemptId(review_core::hex::encode(&hasher.finalize())[..26].to_string())
    }
}

impl std::fmt::Display for AttemptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Running,
    /// Completed while still current: its output is selected.
    Selected,
    /// Fenced before it delivered. Anything it produces afterwards is quarantined.
    Fenced,
    /// Delivered after being fenced. Recorded and charged; never selected.
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: AttemptId,
    pub node: String,
    pub epoch: Epoch,
    pub state: AttemptState,
    /// Cost charged, whether or not the output was ever used.
    pub charged: u128,
}

/// What an attempt delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub attempt: AttemptId,
    pub output: String,
    pub cost: u64,
}

/// A common Task receipt can include several bounded Provider operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactReceipt {
    pub attempt: AttemptId,
    pub output: String,
    pub cost: u128,
}

/// The outcome of admitting a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// The attempt was current: its output may feed downstream nodes.
    Selected,
    /// The attempt had been fenced: recorded and charged, never downstream.
    Quarantined,
}

#[derive(Debug, Clone, Default)]
pub struct AttemptLedger {
    attempts: BTreeMap<AttemptId, Attempt>,
    /// Updated with the same checked mutation as each Attempt's cumulative charge.
    charged_total: u128,
    /// The current epoch per node. A receipt from any earlier epoch is late.
    current: BTreeMap<String, Epoch>,
    /// Outputs that may feed downstream, in the order they were selected.
    selected: Vec<(String, String)>,
    namespace: Option<String>,
}

impl AttemptLedger {
    /// Reconstruct the next per-node epoch under one durable Round namespace.
    pub fn scoped(
        namespace: impl Into<String>,
        prior_attempt_counts: BTreeMap<String, u64>,
    ) -> Self {
        Self {
            current: prior_attempt_counts
                .into_iter()
                .filter_map(|(node, count)| count.checked_sub(1).map(|epoch| (node, epoch)))
                .collect(),
            namespace: Some(namespace.into()),
            ..Self::default()
        }
    }

    fn id(&self, node: &str, epoch: Epoch) -> AttemptId {
        self.namespace.as_deref().map_or_else(
            || AttemptId::of(node, epoch),
            |namespace| AttemptId::scoped(namespace, node, epoch),
        )
    }

    /// Dispatch a new attempt for a node, superseding any earlier one.
    pub fn dispatch(&mut self, node: &str) -> AttemptId {
        let epoch = self.current.get(node).map(|e| e + 1).unwrap_or(0);
        // Dispatching a second attempt fences the first: two live attempts for one node would
        // mean two outputs racing for the same slot, and whichever landed first would win.
        if epoch > 0 {
            self.fence(node);
        }
        self.current.insert(node.to_string(), epoch);
        let id = self.id(node, epoch);
        self.attempts.insert(
            id.clone(),
            Attempt {
                id: id.clone(),
                node: node.to_string(),
                epoch,
                state: AttemptState::Running,
                charged: 0,
            },
        );
        id
    }

    /// Revoke the node's current epoch. Idempotent — fencing twice is not an error, because the
    /// caller often cannot know whether a timeout beat a cancellation.
    pub fn fence(&mut self, node: &str) {
        let Some(epoch) = self.current.get(node).copied() else {
            return;
        };
        let id = self.id(node, epoch);
        if let Some(attempt) = self.attempts.get_mut(&id)
            && attempt.state == AttemptState::Running
        {
            attempt.state = AttemptState::Fenced;
        }
    }

    /// Record spend for an attempt that produced no receipt, such as a timeout or malformed
    /// provider response. This preserves legacy replacement accounting; common Task callers
    /// use [`Self::charge_exact`] for cumulative floors. Fencing and accounting are independent.
    pub fn charge(&mut self, attempt: &AttemptId, amount: u64) {
        self.set_charge(attempt, u128::from(amount))
            .expect("legacy charge must fit the exact Attempt ledger");
    }

    /// Cumulative charge floors cannot refund earlier observations. Check the aggregate
    /// before mutation so callers can reject an unrepresentable accounting transition.
    pub fn charge_exact(&mut self, attempt: &AttemptId, amount: u128) -> Result<(), String> {
        let Some(existing) = self.attempts.get(attempt) else {
            return Ok(());
        };
        self.set_charge(attempt, amount.max(existing.charged))
    }

    fn set_charge(&mut self, attempt: &AttemptId, amount: u128) -> Result<(), String> {
        let Some(existing) = self.attempts.get(attempt) else {
            return Ok(());
        };
        let total = self
            .charged_total
            .checked_sub(existing.charged)
            .and_then(|total| total.checked_add(amount))
            .ok_or("Attempt charge total overflow")?;
        if let Some(attempt) = self.attempts.get_mut(attempt) {
            attempt.charged = amount;
        }
        self.charged_total = total;
        Ok(())
    }

    /// Admit a receipt.
    ///
    /// The single decision that matters: was this attempt still current? A fenced attempt's
    /// output is charged and recorded but never selected — so a late delivery cannot change the
    /// run, whatever it contains.
    pub fn admit(&mut self, receipt: &Receipt) -> Selection {
        self.charge(&receipt.attempt, receipt.cost);
        self.select_receipt(&receipt.attempt, &receipt.output)
    }

    pub fn admit_exact(&mut self, receipt: &ExactReceipt) -> Result<Selection, String> {
        self.charge_exact(&receipt.attempt, receipt.cost)?;
        Ok(self.select_receipt(&receipt.attempt, &receipt.output))
    }

    fn select_receipt(&mut self, attempt: &AttemptId, output: &str) -> Selection {
        let Some(attempt) = self.attempts.get_mut(attempt) else {
            return Selection::Quarantined;
        };

        let current = self.current.get(&attempt.node).copied();
        let is_current = current == Some(attempt.epoch) && attempt.state != AttemptState::Fenced;

        if is_current {
            attempt.state = AttemptState::Selected;
            self.selected.push((attempt.node.clone(), output.into()));
            Selection::Selected
        } else {
            attempt.state = AttemptState::Quarantined;
            Selection::Quarantined
        }
    }

    /// Outputs eligible to feed downstream nodes, in canonical node order.
    ///
    /// Sorted rather than in arrival order for the same reason gather is: what a downstream node
    /// receives must be a property of the pipeline, not of which attempt happened to land first.
    pub fn selected_outputs(&self) -> Vec<(String, String)> {
        let mut outputs = self.selected.clone();
        outputs.sort();
        outputs
    }

    pub fn attempt(&self, id: &AttemptId) -> Option<&Attempt> {
        self.attempts.get(id)
    }

    pub fn attempts(&self) -> Vec<&Attempt> {
        self.attempts.values().collect()
    }

    /// Everything spent, including on attempts whose output was thrown away.
    pub fn total_charged(&self) -> u128 {
        self.charged_total
    }

    pub fn quarantined(&self) -> Vec<&Attempt> {
        self.attempts
            .values()
            .filter(|a| a.state == AttemptState::Quarantined)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_receipts_keep_their_historical_replacement_accounting() {
        let mut ledger = AttemptLedger::default();
        let attempt = ledger.dispatch("reviewer");
        ledger.charge(&attempt, 20);
        ledger.charge(&attempt, 7);
        assert_eq!(ledger.total_charged(), 7);
        ledger.admit(&Receipt {
            attempt: attempt.clone(),
            output: "legacy".into(),
            cost: 5,
        });
        assert_eq!(ledger.total_charged(), 5);
        assert_eq!(ledger.attempt(&attempt).unwrap().charged, 5);
    }

    #[test]
    fn exact_late_receipts_keep_the_charge_floor_and_remain_quarantined() {
        let mut ledger = AttemptLedger::default();
        let old = ledger.dispatch("reviewer");
        ledger.charge_exact(&old, u128::from(u64::MAX) + 7).unwrap();
        let current = ledger.dispatch("reviewer");
        ledger.admit(&Receipt {
            attempt: current,
            output: "current".into(),
            cost: 5,
        });
        assert_eq!(
            ledger
                .admit_exact(&ExactReceipt {
                    attempt: old.clone(),
                    output: "late".into(),
                    cost: u128::from(u64::MAX) + 8,
                })
                .unwrap(),
            Selection::Quarantined
        );
        ledger.charge_exact(&old, 1).unwrap();
        assert_eq!(
            ledger.attempt(&old).unwrap().charged,
            u128::from(u64::MAX) + 8
        );
        assert_eq!(ledger.total_charged(), u128::from(u64::MAX) + 13);
        assert_eq!(
            ledger.selected_outputs(),
            vec![("reviewer".into(), "current".into())]
        );
    }

    #[test]
    fn exact_attempt_total_overflow_does_not_mutate_charge_or_selection() {
        let mut ledger = AttemptLedger::default();
        let first = ledger.dispatch("first");
        let second = ledger.dispatch("second");
        ledger.charge_exact(&first, u128::MAX).unwrap();
        assert!(
            ledger
                .admit_exact(&ExactReceipt {
                    attempt: second.clone(),
                    output: "overflow".into(),
                    cost: 1,
                })
                .is_err()
        );
        assert_eq!(ledger.total_charged(), u128::MAX);
        assert_eq!(ledger.attempt(&second).unwrap().charged, 0);
        assert_eq!(
            ledger.attempt(&second).unwrap().state,
            AttemptState::Running
        );
        assert!(ledger.selected_outputs().is_empty());
    }

    #[test]
    fn a_second_dispatch_fences_the_first() {
        let mut ledger = AttemptLedger::default();
        let first = ledger.dispatch("deep");
        let second = ledger.dispatch("deep");

        assert_ne!(first, second);
        assert_eq!(ledger.attempt(&first).unwrap().state, AttemptState::Fenced);
        assert_eq!(
            ledger.attempt(&second).unwrap().state,
            AttemptState::Running
        );
    }

    #[test]
    fn fencing_is_idempotent() {
        let mut ledger = AttemptLedger::default();
        let id = ledger.dispatch("deep");
        ledger.fence("deep");
        ledger.fence("deep");
        assert_eq!(ledger.attempt(&id).unwrap().state, AttemptState::Fenced);
    }

    #[test]
    fn fencing_an_unknown_node_is_not_an_error() {
        let mut ledger = AttemptLedger::default();
        ledger.fence("never-dispatched");
        assert!(ledger.attempts().is_empty());
    }

    #[test]
    fn a_receipt_for_an_unknown_attempt_is_quarantined() {
        let mut ledger = AttemptLedger::default();
        let selection = ledger.admit(&Receipt {
            attempt: AttemptId("ghost#0".into()),
            output: "artifact".into(),
            cost: 10,
        });
        assert_eq!(selection, Selection::Quarantined);
        assert!(ledger.selected_outputs().is_empty());
    }
}
