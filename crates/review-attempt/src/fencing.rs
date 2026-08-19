//! Attempt lifecycle and the fencing that makes a late result harmless.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    pub charged: u64,
}

/// What an attempt delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub attempt: AttemptId,
    pub output: String,
    pub cost: u64,
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
    /// The current epoch per node. A receipt from any earlier epoch is late.
    current: BTreeMap<String, Epoch>,
    /// Outputs that may feed downstream, in the order they were selected.
    selected: Vec<(String, String)>,
}

impl AttemptLedger {
    /// Dispatch a new attempt for a node, superseding any earlier one.
    pub fn dispatch(&mut self, node: &str) -> AttemptId {
        let epoch = self.current.get(node).map(|e| e + 1).unwrap_or(0);
        // Dispatching a second attempt fences the first: two live attempts for one node would
        // mean two outputs racing for the same slot, and whichever landed first would win.
        if epoch > 0 {
            self.fence(node);
        }
        self.current.insert(node.to_string(), epoch);
        let id = AttemptId::of(node, epoch);
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
        let id = AttemptId::of(node, epoch);
        if let Some(attempt) = self.attempts.get_mut(&id)
            && attempt.state == AttemptState::Running
        {
            attempt.state = AttemptState::Fenced;
        }
    }

    /// Admit a receipt.
    ///
    /// The single decision that matters: was this attempt still current? A fenced attempt's
    /// output is charged and recorded but never selected — so a late delivery cannot change the
    /// run, whatever it contains.
    pub fn admit(&mut self, receipt: &Receipt) -> Selection {
        let Some(attempt) = self.attempts.get_mut(&receipt.attempt) else {
            return Selection::Quarantined;
        };
        attempt.charged = receipt.cost;

        let current = self.current.get(&attempt.node).copied();
        let is_current = current == Some(attempt.epoch) && attempt.state != AttemptState::Fenced;

        if is_current {
            attempt.state = AttemptState::Selected;
            self.selected
                .push((attempt.node.clone(), receipt.output.clone()));
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
    pub fn total_charged(&self) -> u64 {
        self.attempts.values().map(|a| a.charged).sum()
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
