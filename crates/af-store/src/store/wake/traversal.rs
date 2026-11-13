//! Persist scheduling progress before bounded work, independently from source/delivery cursors.
use super::*;
use af_contracts::task::wake::delivery::WakeTraversalV1;
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub enum WakeTraversalStage {
    Extraction,
    Native,
}

impl EventStore {
    /// Reserve the next sorted target under the admitted Store's serialization boundary.
    /// A crash can skip a visit until the next cycle, but cannot restart every cycle at the
    /// first target. This grants no authority to extract, claim, deliver or invoke a host:
    /// those operations still revalidate their own source, expiry, lease and session fences.
    pub fn next_wake_target(
        &self,
        stage: WakeTraversalStage,
        candidates: &BTreeSet<String>,
    ) -> Result<Option<String>, StoreError> {
        self.wake_incarnation()?;
        if candidates.len() > MAX_SUBSCRIPTIONS as usize
            || candidates.iter().any(|id| !af_contracts::is_digest(id))
        {
            return Err(refusal("Invalid bounded wake traversal candidates"));
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let text: Option<String> = tx
            .query_row(
                "SELECT record FROM wake_traversal_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let mut traversal = if let Some(text) = text {
            if text.len() > MAX_WAKE_BYTES {
                return Err(refusal("Wake traversal too large"));
            }
            let value: WakeTraversalV1 = serde_json::from_str(&text)?;
            value.validate().map_err(|e| refusal(&e))?;
            value
        } else {
            WakeTraversalV1 {
                schema: "af/task-wake-traversal@1".into(),
                extraction_after: None,
                native_after: None,
            }
        };
        let known: BTreeSet<_> = self
            .wake_registrations()?
            .into_iter()
            .map(|r| match stage {
                WakeTraversalStage::Extraction => r.subscription.subscription_id,
                WakeTraversalStage::Native => r.subscription.recipient_ref,
            })
            .collect();
        if !candidates.is_subset(&known) {
            return Err(refusal("Unknown wake traversal target"));
        }
        let after = match stage {
            WakeTraversalStage::Extraction => &mut traversal.extraction_after,
            WakeTraversalStage::Native => &mut traversal.native_after,
        };
        let next = candidates
            .iter()
            .find(|id| after.as_ref().is_none_or(|last| *id > last))
            .or_else(|| candidates.first())
            .cloned();
        if let Some(next) = &next {
            *after = Some(next.clone());
            traversal.validate().map_err(|e| refusal(&e))?;
            tx.execute(
                "INSERT INTO wake_traversal_v1 VALUES(1,?1)
                 ON CONFLICT(singleton) DO UPDATE SET record=excluded.record",
                [serde_json::to_string(&traversal)?],
            )?;
            capacity(&tx)?;
        }
        tx.commit()?;
        Ok(next)
    }
}
