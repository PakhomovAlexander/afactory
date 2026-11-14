//! Persist scheduling progress before bounded work, independently from source/delivery cursors.
use super::*;
use af_contracts::task::wake::delivery::{
    WakeHostV1, WakeRouteTraversalV1, WakeRouteV1, WakeTraversalV1,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
pub enum WakeTraversalStage {
    Extraction,
    Native,
}

impl EventStore {
    /// Reserve one visit over the complete registration set of this exact admitted Pi route.
    /// The scope is fixed by Store authority, never by a caller-provided candidate subset.
    /// Route-local polls and foreground drains therefore cannot reset each other's progress.
    /// Return the registration read in this transaction so concurrent registration cannot
    /// leave the caller resolving a newly reserved ID against an older registration list.
    pub fn next_wake_pi_target(
        &self,
        route: &WakeRouteV1,
        now: u64,
    ) -> Result<Option<WakeRegistration>, StoreError> {
        self.wake_incarnation()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if route.host != WakeHostV1::Pi || self.wake_route(&route.route_id, now)? != *route {
            return Err(refusal("Wrong Pi traversal route"));
        }
        let registrations = self.wake_registrations()?;
        let mut candidates = BTreeMap::new();
        for registration in registrations {
            let sub = &registration.subscription;
            if sub.recipient_ref == route.route_id {
                if sub.task != route.task {
                    return Err(refusal("Wrong Pi traversal subscription scope"));
                }
                candidates.insert(sub.subscription_id.clone(), registration);
            }
        }
        let retained: Option<(String, u64, String)> = tx.query_row(
            "SELECT subscription_id,expires,record FROM wake_route_traversal_v1 WHERE route_id=?1",
            [&route.route_id],
            |r| Ok((r.get(0)?, u64_column(r, 1)?, r.get(2)?)),
        ).optional()?;
        let after = if let Some((id, expires, text)) = retained {
            if text.len() > MAX_WAKE_BYTES {
                return Err(refusal("Wake route traversal too large"));
            }
            let value: WakeRouteTraversalV1 = serde_json::from_str(&text)?;
            value.validate().map_err(|e| refusal(&e))?;
            if value.route_id != route.route_id
                || value.extraction_after != id
                || value.expires_unix_ms != expires
                || expires != route.expires_unix_ms
                || !candidates.contains_key(&id)
            {
                return Err(refusal("Wake route traversal scope mismatch"));
            }
            Some(id)
        } else {
            None
        };
        let next = candidates
            .iter()
            .find(|(id, _)| after.as_ref().is_none_or(|last| *id > last))
            .or_else(|| candidates.first_key_value())
            .map(|(_, registration)| registration.clone());
        if let Some(registration) = &next {
            let id = &registration.subscription.subscription_id;
            let value = WakeRouteTraversalV1 {
                schema: "af/task-wake-route-traversal@1".into(),
                route_id: route.route_id.clone(),
                extraction_after: id.clone(),
                expires_unix_ms: route.expires_unix_ms,
            };
            value.validate().map_err(|e| refusal(&e))?;
            let text = serde_json::to_string(&value)?;
            if text.len() > MAX_WAKE_BYTES {
                return Err(refusal("Wake route traversal too large"));
            }
            tx.execute(
                "INSERT INTO wake_route_traversal_v1 VALUES(?1,?2,?3,?4)
                 ON CONFLICT(route_id) DO UPDATE SET subscription_id=excluded.subscription_id,
                 expires=excluded.expires,record=excluded.record",
                params![route.route_id, id, sql_number(value.expires_unix_ms)?, text],
            )?;
            capacity(&tx)?;
        }
        tx.commit()?;
        Ok(next)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_pi_traversal_capacity_refuses_and_rolls_back_rows_or_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        for overflow_rows in [true, false] {
            let tx = rusqlite::Transaction::new_unchecked(
                &store.conn,
                rusqlite::TransactionBehavior::Immediate,
            )
            .unwrap();
            if overflow_rows {
                for n in 0..=MAX_SUBSCRIPTIONS {
                    tx.execute(
                        "INSERT INTO wake_route_traversal_v1 VALUES(?1,?2,1,?3)",
                        params![format!("scope-{n}"), "position", "bounded body"],
                    )
                    .unwrap();
                    if n + 1 == MAX_SUBSCRIPTIONS {
                        capacity(&tx).unwrap();
                    }
                }
            } else {
                // Corrupt oversized persisted data must also consume the same control
                // allowance. A low row count cannot bypass accounting of the bodies.
                tx.execute(
                    "INSERT INTO wake_route_traversal_v1 VALUES('scope','position',1,?1)",
                    ["x".repeat(MAX_CONTROL_BYTES as usize + 1)],
                )
                .unwrap();
            }
            assert!(matches!(capacity(&tx), Err(StoreError::Conflict(_))));
            drop(tx);
            assert_eq!(
                store
                    .conn
                    .query_row("SELECT COUNT(*) FROM wake_route_traversal_v1", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "failed traversal reservations commit neither count nor bytes"
            );
        }
    }
}
