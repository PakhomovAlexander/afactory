//! Durable local receiver. No process execution and no network or model calls live here.
use super::*;
use af_contracts::task::wake::delivery::*;

pub const RETENTION_MS: u64 = 24 * 60 * 60 * 1000;
pub const CLAIM_MS: u64 = 30_000;

fn hash(value: &impl Serialize) -> Result<String, StoreError> {
    content_id(&serde_json::to_value(value)?).map_err(|e| refusal(&e.to_string()))
}
pub fn route_identity(route: &WakeRouteV1) -> Result<String, StoreError> {
    let mut value = serde_json::to_value(route)?;
    value
        .as_object_mut()
        .ok_or_else(|| refusal("Invalid route"))?
        .remove("route_id");
    hash(&value)
}
pub fn capability_digest(capability: &str) -> Result<String, StoreError> {
    hash(&json!({"domain":"af/wake-capability/1","capability":capability}))
}
pub fn session_key(route: &WakeRouteV1) -> Result<String, StoreError> {
    // Changing a grant, endpoint, Task or executable does not evade a session's uncertainty.
    hash(&json!({"domain":"af/wake-session/1","host":route.host,"session":route.session}))
}
fn receipt(
    record: &WakeDeliveryV1,
    now: u64,
    stage: WakeReceiptStageV1,
) -> Result<WakeReceiptV1, StoreError> {
    let attempt_id = hash(&json!({"delivery":record.envelope.delivery_id,"epoch":record.epoch}))?;
    let receipt_id = hash(&json!({"attempt":attempt_id,"stage":stage}))?;
    Ok(WakeReceiptV1 {
        schema: WAKE_RECEIPT_V1.into(),
        receipt_id,
        delivery_id: record.envelope.delivery_id.clone(),
        payload_digest: record.envelope.payload_digest.clone(),
        attempt_id,
        observed_unix_ms: now,
        stage,
    })
}
fn save(conn: &Connection, record: &WakeDeliveryV1) -> Result<(), StoreError> {
    record.validate().map_err(|e| refusal(&e))?;
    let text = serde_json::to_string(record)?;
    // The retained packet plus three separately bounded receipts.
    if text.len() > MAX_WAKE_BYTES * 2 {
        return Err(refusal("Receiver record too large"));
    }
    conn.execute(
        "INSERT INTO wake_delivery_v1 VALUES(?1,?2,?3,?4)
        ON CONFLICT(id) DO UPDATE SET record=excluded.record",
        params![
            record.envelope.delivery_id,
            record.envelope.subscription_id,
            record.route_id,
            text
        ],
    )?;
    capacity(conn)
}
fn decode(text: &str) -> Result<WakeDeliveryV1, StoreError> {
    if text.len() > MAX_WAKE_BYTES * 2 {
        return Err(refusal("Receiver record too large"));
    }
    let record: WakeDeliveryV1 = serde_json::from_str(text)?;
    record.validate().map_err(|e| refusal(&e))?;
    decode_envelope(&serde_json::to_vec(&record.envelope)?).map_err(|e| refusal(&e))?;
    for retained in &record.receipts {
        let expected = receipt(&record, retained.observed_unix_ms, retained.stage.clone())?;
        if expected != *retained {
            return Err(refusal("Receiver receipt identity mismatch"));
        }
    }
    Ok(record)
}

impl EventStore {
    /// One original host/session has one authoritative Task Store, bound in an independent
    /// accounted machine Store. Other Stores cannot bypass its native admission fence.
    pub fn bind_wake_session_store(&self, key: &str, incarnation: &str) -> Result<(), StoreError> {
        self.wake_incarnation()?;
        if !af_contracts::is_digest(key) || !af_contracts::is_digest(incarnation) {
            return Err(refusal("Invalid wake session Store identity"));
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.validate_wake_session_stores()?;
        let old: Option<String> = tx
            .query_row(
                "SELECT store_incarnation FROM wake_session_stores_v1 WHERE session_key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            if old != incarnation {
                return Err(refusal(
                    "Original host session already belongs to another wake Store",
                ));
            }
        } else {
            let digest = hash(
                &json!({"domain":"af/wake-session-store/1","session":key,"store":incarnation}),
            )?;
            tx.execute(
                "INSERT INTO wake_session_stores_v1 VALUES(?1,?2,?3)",
                params![key, incarnation, digest],
            )?;
            capacity(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }
    fn validate_wake_session_stores(&self) -> Result<(), StoreError> {
        let mut q=self.conn.prepare("SELECT session_key,store_incarnation,binding_digest FROM wake_session_stores_v1 LIMIT 4097")?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > MAX_ENVELOPES as usize {
            return Err(refusal("Wake machine session capacity corrupt"));
        }
        for (key, incarnation, digest) in rows {
            if !af_contracts::is_digest(&key)
                || !af_contracts::is_digest(&incarnation)
                || digest
                    != hash(
                        &json!({"domain":"af/wake-session-store/1","session":key,"store":incarnation}),
                    )?
            {
                return Err(refusal("Corrupt wake machine session binding"));
            }
        }
        Ok(())
    }
    pub fn check_wake_session_store(&self, key: &str, incarnation: &str) -> Result<(), StoreError> {
        self.wake_incarnation()?;
        self.validate_wake_session_stores()?;
        let old: Option<String> = self
            .conn
            .query_row(
                "SELECT store_incarnation FROM wake_session_stores_v1 WHERE session_key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        if old.as_deref() != Some(incarnation) {
            return Err(refusal("Unknown or foreign original-session wake Store"));
        }
        Ok(())
    }
    pub fn install_wake_route(&self, route: &WakeRouteV1) -> Result<(), StoreError> {
        route.validate().map_err(|e| refusal(&e))?;
        let now = task::now()?;
        if route.task.store_incarnation != self.wake_incarnation()?
            || route.route_id != route_identity(route)?
            || route.expires_unix_ms <= now
            || route.expires_unix_ms - now > MAX_SUBSCRIPTION_MS
        {
            return Err(refusal("Wake route identity, Store or expiry mismatch"));
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute(
            "INSERT INTO wake_routes_v1(id,route,expires) VALUES(?1,?2,?3)",
            params![
                route.route_id,
                serde_json::to_string(route)?,
                sql_number(route.expires_unix_ms)?
            ],
        )?;
        capacity(&tx)?;
        tx.commit()?;
        Ok(())
    }
    pub fn wake_route(&self, id: &str, now: u64) -> Result<WakeRouteV1, StoreError> {
        self.wake_incarnation()?;
        let (text, revoked): (String, bool) = self.conn.query_row(
            "SELECT route,revoked FROM wake_routes_v1 WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if text.len() > MAX_WAKE_BYTES {
            return Err(refusal("Wake route too large"));
        }
        let route: WakeRouteV1 = serde_json::from_str(&text)?;
        route.validate().map_err(|e| refusal(&e))?;
        if revoked
            || route.route_id != id
            || route_identity(&route)? != id
            || route.task.store_incarnation != self.wake_incarnation()?
            || now >= route.expires_unix_ms
        {
            return Err(refusal("Wake route revoked, expired or substituted"));
        }
        Ok(route)
    }
    pub fn authenticate_wake_grant(
        &self,
        grant: &WakeGrantV1,
        now: u64,
    ) -> Result<WakeRouteV1, StoreError> {
        grant.validate().map_err(|e| refusal(&e))?;
        let route = self.wake_route(&grant.route_id, now)?;
        if capability_digest(&grant.capability)? != route.capability_digest {
            return Err(refusal("Wrong wake capability"));
        }
        Ok(route)
    }
    pub fn revoke_wake_route(&self, grant: &WakeGrantV1, now: u64) -> Result<(), StoreError> {
        self.authenticate_wake_grant(grant, now)?;
        self.conn.execute(
            "UPDATE wake_routes_v1 SET revoked=1 WHERE id=?1",
            [&grant.route_id],
        )?;
        Ok(())
    }
    pub fn wake_deliveries(&self) -> Result<Vec<WakeDeliveryV1>, StoreError> {
        self.wake_incarnation()?;
        let mut q = self
            .conn
            .prepare("SELECT record FROM wake_delivery_v1 ORDER BY rowid LIMIT 4097")?;
        let rows = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > MAX_ENVELOPES as usize {
            return Err(refusal("Receiver capacity corrupt"));
        }
        rows.iter().map(|s| decode(s)).collect()
    }
    /// Sender claim is durable before local transport. A stale sender cannot accept or back off.
    pub fn claim_wake(
        &self,
        envelope: &WakeEnvelopeV1,
        now: u64,
    ) -> Result<Option<WakeDeliveryV1>, StoreError> {
        decode_envelope(&serde_json::to_vec(envelope)?).map_err(|e| refusal(&e))?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let retained = self.inspect_wake(&envelope.subscription_id)?;
        if !retained.envelopes.contains(envelope) {
            return Err(refusal("Uncommitted wake packet"));
        }
        let mut record = if let Some(text) = tx
            .query_row(
                "SELECT record FROM wake_delivery_v1 WHERE id=?1",
                [&envelope.delivery_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            let r = decode(&text)?;
            if r.envelope != *envelope {
                return Err(refusal("Conflicting receiver packet"));
            }
            r
        } else {
            WakeDeliveryV1 {
                schema: "af/task-wake-delivery@1".into(),
                envelope: envelope.clone(),
                route_id: retained.registration.subscription.recipient_ref,
                attempts: 0,
                epoch: 0,
                claim_until_unix_ms: 0,
                next_attempt_unix_ms: 0,
                state: WakeDeliveryStateV1::Pending,
                reason: None,
                receipts: vec![],
            }
        };
        if record.state != WakeDeliveryStateV1::Pending
            || record.claim_until_unix_ms > now
            || record.next_attempt_unix_ms > now
        {
            return Ok(None);
        }
        if now >= envelope.expires_unix_ms || retained.closure.is_some() {
            record.state = WakeDeliveryStateV1::Expired;
            record.reason = Some(WakeDeliveryReasonV1::Expired);
        } else if record.attempts >= 8 {
            record.state = WakeDeliveryStateV1::Blocked;
            record.reason = Some(WakeDeliveryReasonV1::TransportRetryExhausted);
        } else {
            record.epoch = record
                .epoch
                .checked_add(1)
                .ok_or_else(|| refusal("Wake fence overflow"))?;
            record.attempts += 1;
            record.claim_until_unix_ms = now.saturating_add(CLAIM_MS);
        }
        save(&tx, &record)?;
        tx.commit()?;
        Ok((record.state == WakeDeliveryStateV1::Pending).then_some(record))
    }
    /// Receiver retains exact packet bytes/digest before acknowledging. Replayed accepted
    /// packets return the retained receipt; no new scheduling claim is made.
    pub fn accept_wake(
        &self,
        claim: &WakeDeliveryV1,
        now: u64,
    ) -> Result<WakeDeliveryV1, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let text: String = tx.query_row(
            "SELECT record FROM wake_delivery_v1 WHERE id=?1",
            [&claim.envelope.delivery_id],
            |r| r.get(0),
        )?;
        let mut record = decode(&text)?;
        if record.envelope != claim.envelope || record.epoch != claim.epoch {
            return Err(refusal("Stale wake sender or conflicting packet"));
        }
        if record.state != WakeDeliveryStateV1::Pending {
            return Ok(record);
        }
        if self
            .inspect_wake(&record.envelope.subscription_id)?
            .closure
            .is_some()
        {
            return Err(refusal("Wake subscription is durably closed"));
        }
        let route = self.wake_route(&record.route_id, now)?;
        if record.claim_until_unix_ms <= now
            || now >= record.envelope.expires_unix_ms
            || route.task != record.envelope.observation.task
        {
            return Err(refusal("Wake receiver claim, expiry or scope refused"));
        }
        let inbox_ref = hash(&json!({"inbox":record.envelope.delivery_id,"route":route.route_id}))?;
        record.receipts.push(receipt(
            &record,
            now,
            WakeReceiptStageV1::InboxAccepted { inbox_ref },
        )?);
        record.state = WakeDeliveryStateV1::InboxAccepted;
        record.reason = None;
        record.claim_until_unix_ms = 0;
        save(&tx, &record)?;
        tx.commit()?;
        Ok(record)
    }
    pub fn backoff_wake(&self, claim: &WakeDeliveryV1, now: u64) -> Result<(), StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let text: String = tx.query_row(
            "SELECT record FROM wake_delivery_v1 WHERE id=?1",
            [&claim.envelope.delivery_id],
            |r| r.get(0),
        )?;
        let mut record = decode(&text)?;
        if record.epoch != claim.epoch
            || record.state != WakeDeliveryStateV1::Pending
            || record.claim_until_unix_ms <= now
        {
            return Err(refusal("Stale wake sender"));
        }
        record.claim_until_unix_ms = 0;
        record.next_attempt_unix_ms =
            now.saturating_add((1000u64 << record.attempts.min(8)).min(300_000));
        if record.attempts >= 8 {
            record.state = WakeDeliveryStateV1::Blocked;
            record.reason = Some(WakeDeliveryReasonV1::TransportRetryExhausted);
        } else {
            record.reason = Some(WakeDeliveryReasonV1::RouteUnavailable);
        }
        save(&tx, &record)?;
        tx.commit()?;
        Ok(())
    }
    /// Find the first live committed packet for this original session, including packets
    /// not yet claimed for transport. Outbox insertion order is durable across subscriptions
    /// and restart; receiver insertion order is not delivery order. Call under IMMEDIATE.
    fn wake_native_head(
        &self,
        route: &WakeRouteV1,
        now: u64,
    ) -> Result<Option<WakeDeliveryV1>, StoreError> {
        if self.wake_route(&route.route_id, now)? != *route {
            return Err(refusal("Route changed"));
        }
        self.wake_uncertainty()?;
        let key = session_key(route)?;
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM wake_session_fences_v1 WHERE session_key=?1)",
            [&key],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(None);
        }
        let registrations = self.wake_registrations()?;
        let mut query = self.conn.prepare(
            "SELECT delivery_id,subscription_id,digest,envelope FROM wake_outbox_v1 ORDER BY rowid LIMIT 4097",
        )?;
        let rows = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > MAX_ENVELOPES as usize {
            return Err(refusal("Wake outbox capacity corrupt"));
        }
        for (id, subscription_id, digest, text) in rows {
            let envelope = decode_envelope(text.as_bytes()).map_err(|e| refusal(&e))?;
            let registration = registrations
                .iter()
                .find(|r| r.subscription.subscription_id == subscription_id)
                .ok_or_else(|| refusal("Unknown outbox subscription"))?;
            if envelope.delivery_id != id
                || envelope.subscription_id != subscription_id
                || envelope.payload_digest != digest
                || envelope.observation.task != registration.subscription.task
            {
                return Err(refusal("Conflicting outbox identity"));
            }
            let other = match self.wake_route(&registration.subscription.recipient_ref, now) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if session_key(&other)? != key || envelope.expires_unix_ms <= now {
                continue;
            }
            if self.inspect_wake(&subscription_id)?.closure.is_some() {
                continue;
            }
            // This first packet blocks later packets even before a receiver row exists.
            if other.route_id != route.route_id {
                return Ok(None);
            }
            let Some(text) = self
                .conn
                .query_row(
                    "SELECT record FROM wake_delivery_v1 WHERE id=?1",
                    [&id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(None);
            };
            let record = decode(&text)?;
            if record.envelope != envelope || record.route_id != other.route_id {
                return Err(refusal("Conflicting receiver packet"));
            }
            return Ok((record.state == WakeDeliveryStateV1::InboxAccepted).then_some(record));
        }
        Ok(None)
    }
    /// Reserve a due native preflight BEFORE any endpoint contact. The existing bounded
    /// attempts/claim/deadline fields also own this stage; receipt epochs remain unchanged.
    /// A crashed reservation consumes its attempt and retains backoff. Competing drivers
    /// cannot contact the endpoint while its lease or next-attempt deadline is live.
    pub fn reserve_wake_native(
        &self,
        route: &WakeRouteV1,
        now: u64,
    ) -> Result<Option<WakeDeliveryV1>, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let Some(mut record) = self.wake_native_head(route, now)? else {
            return Ok(None);
        };
        if record.claim_until_unix_ms > now || record.next_attempt_unix_ms > now {
            return Ok(None);
        }
        if record.attempts >= 8 {
            record.state = WakeDeliveryStateV1::Blocked;
            record.reason = Some(WakeDeliveryReasonV1::NativePreflightExhausted);
            save(&tx, &record)?;
            tx.commit()?;
            return Ok(None);
        }
        record.attempts += 1;
        record.claim_until_unix_ms = now.saturating_add(CLAIM_MS);
        record.next_attempt_unix_ms = record
            .claim_until_unix_ms
            .saturating_add((1000u64 << record.attempts).min(300_000));
        save(&tx, &record)?;
        tx.commit()?;
        Ok(Some(record))
    }
    /// Exactly one native intent per original session. Pi pulls directly; Codex must
    /// reserve preflight and authenticate the peer before committing admission intent.
    pub fn take_wake_native(
        &self,
        route: &WakeRouteV1,
        now: u64,
    ) -> Result<Option<WakeDeliveryV1>, StoreError> {
        if route.host != WakeHostV1::Pi {
            return Err(refusal("Native preflight reservation required"));
        }
        self.take_wake_native_inner(route, None, now)
    }
    pub fn take_reserved_wake_native(
        &self,
        route: &WakeRouteV1,
        claim: &WakeDeliveryV1,
        now: u64,
    ) -> Result<Option<WakeDeliveryV1>, StoreError> {
        self.take_wake_native_inner(route, Some(claim), now)
    }
    /// Revalidate a reserved sender immediately before endpoint contact. This also refuses
    /// closure/revocation/expiry or a lost lease while preflight was being prepared.
    pub fn check_reserved_wake_native(
        &self,
        route: &WakeRouteV1,
        claim: &WakeDeliveryV1,
        now: u64,
    ) -> Result<bool, StoreError> {
        let _tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let Some(record) = self.wake_native_head(route, now)? else {
            return Ok(false);
        };
        Self::check_native_claim(&record, claim, now)?;
        Ok(true)
    }
    fn check_native_claim(
        record: &WakeDeliveryV1,
        claim: &WakeDeliveryV1,
        now: u64,
    ) -> Result<(), StoreError> {
        if record.envelope != claim.envelope
            || record.route_id != claim.route_id
            || record.epoch != claim.epoch
            || record.attempts != claim.attempts
            || record.claim_until_unix_ms != claim.claim_until_unix_ms
            || record.claim_until_unix_ms <= now
        {
            return Err(refusal("Stale native preflight sender"));
        }
        Ok(())
    }
    fn take_wake_native_inner(
        &self,
        route: &WakeRouteV1,
        claim: Option<&WakeDeliveryV1>,
        now: u64,
    ) -> Result<Option<WakeDeliveryV1>, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Some(mut record) = self.wake_native_head(route, now)? {
            if let Some(claim) = claim {
                Self::check_native_claim(&record, claim, now)?;
            } else if record.claim_until_unix_ms > now || record.next_attempt_unix_ms > now {
                return Ok(None);
            }
            let key = session_key(route)?;
            let intent_id = hash(
                &json!({"domain":"af/wake-native-intent/1","session":key,"delivery":record.envelope.delivery_id}),
            )?;
            tx.execute(
                "INSERT INTO wake_session_fences_v1 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    key,
                    record.envelope.subscription_id,
                    record.envelope.delivery_id,
                    record.envelope.payload_digest,
                    intent_id,
                    sql_number(record.envelope.expires_unix_ms)?,
                    record.envelope.observation.task.task_id,
                    sql_number(record.envelope.observation.causal.sequence)?,
                    record.envelope.observation.causal.event_id
                ],
            )?;
            record.receipts.push(receipt(
                &record,
                now,
                WakeReceiptStageV1::NativeAdmissionIntent {
                    intent_id: intent_id.clone(),
                },
            )?);
            record.receipts.push(receipt(
                &record,
                now,
                WakeReceiptStageV1::AdmissionUnknown { intent_id },
            )?);
            record.claim_until_unix_ms = 0;
            record.state = WakeDeliveryStateV1::AdmissionUnknown;
            record.reason = Some(WakeDeliveryReasonV1::NativeAdmissionUncertain);
            save(&tx, &record)?;
            tx.commit()?;
            return Ok(Some(record));
        }
        Ok(None)
    }
    /// Expiry is a persisted closure, including never-extracted subscriptions. After the
    /// replay horizon bodies compact; uncertain session fences remain bounded, fail-closed
    /// tombstones. Exhausting that capacity refuses new work, never clears uncertainty.
    pub fn maintain_wakes(&self, now: u64) -> Result<(), StoreError> {
        self.wake_incarnation()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute("INSERT OR IGNORE INTO wake_closed_v1 SELECT id,'expired',?1 FROM wake_subscriptions_v1 WHERE expires<=?1",[sql_number(now)?])?;
        let cutoff = sql_number(now.saturating_sub(RETENTION_MS))?;
        tx.execute("DELETE FROM wake_delivery_v1 WHERE subscription_id IN (SELECT id FROM wake_subscriptions_v1 WHERE expires<=?1)",[cutoff])?;
        tx.execute("DELETE FROM wake_outbox_v1 WHERE expires<=?1", [cutoff])?;
        tx.execute("DELETE FROM wake_closed_v1 WHERE subscription_id IN (SELECT id FROM wake_subscriptions_v1 WHERE expires<=?1)",[cutoff])?;
        tx.execute(
            "DELETE FROM wake_subscriptions_v1 WHERE expires<=?1",
            [cutoff],
        )?;
        tx.execute("DELETE FROM wake_routes_v1 WHERE expires<=?1", [cutoff])?;
        capacity(&tx)?;
        tx.commit()?;
        Ok(())
    }
    /// Known native preflight failure (before intent) remains retryable within the SAME
    /// persisted allowance. No external result can call this to clear an admission fence.
    pub fn defer_wake_native(&self, claim: &WakeDeliveryV1, now: u64) -> Result<(), StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let text: String = tx.query_row(
            "SELECT record FROM wake_delivery_v1 WHERE id=?1",
            [&claim.envelope.delivery_id],
            |r| r.get(0),
        )?;
        let mut record = decode(&text)?;
        Self::check_native_claim(&record, claim, now)?;
        if record.state != WakeDeliveryStateV1::InboxAccepted {
            return Err(refusal("Native admission already uncertain or blocked"));
        }
        record.claim_until_unix_ms = 0;
        record.next_attempt_unix_ms = now.saturating_add((1000u64 << record.attempts).min(300_000));
        if record.attempts >= 8 {
            record.state = WakeDeliveryStateV1::Blocked;
            record.reason = Some(WakeDeliveryReasonV1::NativePreflightExhausted);
        } else {
            record.reason = Some(WakeDeliveryReasonV1::NativePreflightUnavailable);
        }
        save(&tx, &record)?;
        tx.commit()?;
        Ok(())
    }
    pub fn wake_uncertainty(&self) -> Result<serde_json::Value, StoreError> {
        self.wake_incarnation()?;
        let mut q = self.conn.prepare("SELECT session_key,subscription_id,delivery_id,digest,intent_id,expires,task_id,causal_sequence,causal_event_id FROM wake_session_fences_v1 ORDER BY session_key LIMIT 4097")?;
        let rows = q.query_map([],|r|Ok(json!({"session_key":r.get::<_,String>(0)?,"subscription_id":r.get::<_,String>(1)?,
            "delivery_id":r.get::<_,String>(2)?,"payload_digest":r.get::<_,String>(3)?,"intent_id":r.get::<_,String>(4)?,
            "expires_unix_ms":u64_column(r,5)?,"task_id":r.get::<_,String>(6)?,"causal_sequence":u64_column(r,7)?,
            "causal_event_id":r.get::<_,String>(8)?,"state":"admission_unknown","historical":true})))?.collect::<Result<Vec<_>,_>>()?;
        if rows.len() > MAX_ENVELOPES as usize {
            return Err(refusal("Uncertainty capacity corrupt"));
        }
        let mut rows = rows;
        for row in &mut rows {
            for field in [
                "session_key",
                "subscription_id",
                "delivery_id",
                "payload_digest",
                "intent_id",
            ] {
                if !row[field].as_str().is_some_and(af_contracts::is_digest) {
                    return Err(refusal("Corrupt wake uncertainty identity"));
                }
            }
            let expected = hash(
                &json!({"domain":"af/wake-native-intent/1","session":row["session_key"],"delivery":row["delivery_id"]}),
            )?;
            if row["intent_id"] != expected {
                return Err(refusal("Corrupt wake uncertainty binding"));
            }
            let task_id = row["task_id"]
                .as_str()
                .ok_or_else(|| refusal("Invalid uncertainty Task"))?;
            if !af_contracts::task::is_name(task_id) {
                return Err(refusal("Invalid uncertainty Task"));
            }
            row["source_available"] = json!(self.task_tombstone(task_id)?.is_none());
        }
        Ok(json!(rows))
    }
}
