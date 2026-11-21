//! Project-side settlement and idempotent local delivery of accepted backlog assessments.

use super::*;
use crate::backlog::{PROCESSING, VerifiedItem, read_record};
use af_contracts::RunEvent;
use af_contracts::backlog::{
    BacklogRecordV2, Batch as BatchRecord, Decision, Disposition as Verdict, ItemRevision,
    LocalMatch, MAX_EVIDENCE_BYTES, PublicFields, Publication,
    batch::assessment::{Decision as AssessedDecision, MAX_TRACKED_ITEMS, TrackedItem},
};
use af_source_git::manifest::{EntryKind, Manifest};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub(super) mod github;

pub fn local_destination(project_id: &str) -> Result<String, String> {
    value_id(&("af/backlog-local-sink@1", project_id))
}

/// The application constructs this from the accepted Task and its captured source.
pub struct NativeAssessment<'a> {
    pub collection: &'a CollectedBatch,
    pub decisions: &'a [AssessedDecision],
    pub manifest: &'a Manifest,
    pub limits: &'a Limits,
    pub deadline: Instant,
    pub result_id: &'a str,
    pub verification_id: &'a str,
}

impl ProjectStore {
    fn processing(&self) -> Result<Vec<RunEvent>, String> {
        self.events.replay(PROCESSING).map_err(|e| e.to_string())
    }

    fn append_processing(
        &mut self,
        batch_id: &str,
        records: Vec<BacklogRecordV2>,
    ) -> Result<(), String> {
        let events = records
            .into_iter()
            .map(|record| {
                record.validate()?;
                let refs = match &record {
                    BacklogRecordV2::Publication(publication) => {
                        publication.provider_state_id.iter().cloned().collect()
                    }
                    _ => Vec::new(),
                };
                let payload = serde_json::to_value(record).map_err(|e| e.to_string())?;
                Ok(NewEvent::new(EventType::BacklogRecordStoredV2, payload)
                    .correlating(batch_id)
                    .referencing(refs))
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.events
            .append_batch(PROCESSING, &self.cas, &events)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(super) fn tracked_backlog(
        &self,
        max_bytes: usize,
    ) -> Result<(BTreeMap<String, TrackedItem>, bool), String> {
        let current: BTreeSet<_> = self
            .verified_backlog()?
            .into_iter()
            .map(|item| item.revision_id)
            .collect();
        let mut revisions = BTreeMap::new();
        let mut verification = BTreeMap::new();
        for event in self.processing()? {
            match read_record(&self.project_id, &event)? {
                BacklogRecordV2::ItemRevision(revision) => {
                    let id =
                        af_store::content_id(&event.payload).map_err(|error| error.to_string())?;
                    if current.contains(&id) {
                        revisions.insert(id, revision);
                    }
                }
                BacklogRecordV2::Decision(decision)
                    if decision.disposition == Verdict::Verified =>
                {
                    if let Some(id) = decision.independent_verification_id {
                        verification.insert(decision.revision_id, id);
                    }
                }
                _ => {}
            }
        }
        let mut items = BTreeMap::new();
        let mut bytes = 0;
        for (revision_id, revision) in revisions {
            let Some(verification_id) = verification.remove(&revision_id) else {
                continue;
            };
            let key = revision.item_id.clone();
            let item = TrackedItem {
                revision_id,
                revision,
                verification_id,
            };
            // The tuple's brackets and separator cover the entry's JSON
            // punctuation, including the comma between successive entries.
            let size = template_text_bytes(&(&key, &item))?;
            if bytes + size > max_bytes || items.len() == MAX_TRACKED_ITEMS {
                continue;
            }
            bytes += size;
            items.insert(key, item);
        }
        let truncated = items.len() < current.len();
        Ok((items, truncated))
    }

    pub(super) fn native_batch_exists(&self, batch_id: &str) -> Result<bool, String> {
        Ok(self
            .processing()?
            .iter()
            .any(|event| event.correlation_id.as_deref() == Some(batch_id)))
    }

    pub fn settle_native_backlog(
        &mut self,
        proof: &NativeAssessment<'_>,
        cancellation: &AtomicBool,
    ) -> Result<String, String> {
        let (mut state, intake) = self.batch_state()?;
        let batch = proof.collection;
        let reference = &batch.reference;
        let batch_id = &reference.batch_id;
        let pin = self.check_native_batch(&state, reference, &batch.scenario_id, proof.limits)?;
        let events = self.processing()?;
        let receipt = settlement_id(proof)?;
        let mut outcomes = BTreeMap::new();
        let settled = events
            .iter()
            .any(|event| event.correlation_id.as_deref() == Some(batch_id));
        if settled {
            check_settlement(&self.project_id, &events, proof)?;
        } else {
            let current = self.collect_backlog_batch(
                reference.clone(),
                &batch.scenario_id,
                proof.limits,
                batch.source_revision.clone(),
            )?;
            if current != *batch
                || state.batches[batch_id]
                    .members
                    .values()
                    .any(|member| member.settlement.is_some())
            {
                return Err(
                    "native backlog settlement differs from current frozen authority".into(),
                );
            }
        }
        let members: BTreeMap<_, _> = batch.members.iter().map(|m| (&m.member_id, m)).collect();
        let record = BacklogRecordV2::Batch(BatchRecord {
            project_id: self.project_id.clone(),
            authority_id: batch.authority_id.clone(),
            first_sequence: batch
                .members
                .iter()
                .map(|m| m.sequence)
                .min_by_key(|n| n.get())
                .unwrap_or(reference.high_water),
            last_sequence: reference.high_water,
            report_ids: batch
                .members
                .iter()
                .map(|m| m.report_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            scenario_id: batch.scenario_id.clone(),
            policy_id: reference.policy_id.clone(),
            settlement_id: Some(receipt.clone()),
        });
        let batch_record_id = value_id(&record)?;
        let mut records = vec![record];
        let mut revised_items = BTreeSet::new();
        for decision in proof.decisions {
            let group = decision
                .members
                .iter()
                .map(|id| {
                    members
                        .get(id)
                        .copied()
                        .ok_or("Assessment member is outside the frozen batch")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let evidence_available = || {
                group.iter().all(|member| {
                    decision.citations.iter().any(|citation| {
                        proof.manifest.get(&citation.path).is_some_and(|entry| {
                            entry.kind != EntryKind::Symlink
                                && entry.size <= MAX_EVIDENCE_BYTES as u64
                                && member.evidence_ids.contains(&entry.content)
                                && decision.evidence_ids.contains(&entry.content)
                        })
                    })
                })
            };
            let positive = matches!(
                decision.disposition,
                Verdict::Verified | Verdict::AlreadyTracked
            );
            let (verdict, mut reason) = if positive && pin.policy.hold_policy == HoldPolicy::HoldAll
            {
                (
                    Verdict::Held,
                    "Project policy holds publication and verification of backlog items.".into(),
                )
            } else if positive && !evidence_available() {
                (
                    Verdict::Held,
                    "Each member needs digest-matched evidence in the cited committed source."
                        .into(),
                )
            } else {
                (decision.disposition, decision.reason.clone())
            };
            if let Some(index) = &batch.provider_index {
                use std::fmt::Write as _;
                write!(
                    reason,
                    "\nProvider observation {} (GitHub repository ID {}).",
                    index.observation_id, index.observation.repository_id
                )
                .expect("writing to a String cannot fail");
                if verdict == Verdict::AlreadyTracked
                    && let Some(evidence) = &decision.provider_match
                {
                    let issue = index
                        .observation
                        .matched_issue(evidence)
                        .ok_or("Provider match differs from the frozen observation")?;
                    write!(
                        reason,
                        " Verified GitHub issue ID {} (#{}).",
                        issue.id, issue.number
                    )
                    .expect("writing to a String cannot fail");
                }
            }
            let local_match = if verdict == Verdict::AlreadyTracked {
                decision
                    .tracked_id
                    .as_ref()
                    .map(|item_id| {
                        let tracked = batch
                            .tracked_items
                            .get(item_id)
                            .ok_or("Local match is outside the frozen tracked items")?;
                        Ok::<_, String>(LocalMatch {
                            item_id: item_id.clone(),
                            revision_id: tracked.revision_id.clone(),
                        })
                    })
                    .transpose()?
            } else {
                None
            };
            let report_ids = group
                .iter()
                .map(|m| m.report_id.clone())
                .collect::<BTreeSet<_>>();
            let pinned_items: BTreeSet<_> =
                group.iter().filter_map(|m| m.item_id.clone()).collect();
            if pinned_items.len() > 1 {
                return Err("A backlog group names conflicting reconsideration items".into());
            }
            let first = report_ids.first().ok_or("Empty backlog group")?;
            let item_id = match pinned_items.into_iter().next() {
                Some(item) => item,
                None => value_id(&("af/backlog-native-item@1", &self.project_id, first))?,
            };
            if !revised_items.insert(item_id.clone()) {
                return Err("A backlog assessment revises an item twice".into());
            }
            let proposed = if verdict == Verdict::Verified {
                decision
                    .proposed
                    .clone()
                    .ok_or("Verified backlog decision lacks public fields")?
            } else {
                PublicFields {
                    title: "Private backlog decision".into(),
                    body: "See the private decision record.".into(),
                }
            };
            let item = BacklogRecordV2::ItemRevision(ItemRevision {
                project_id: self.project_id.clone(),
                item_id,
                report_ids: report_ids.into_iter().collect(),
                claim: decision.claim.clone(),
                evidence_ids: decision.evidence_ids.clone(),
                source_revision: batch.source_revision.clone(),
                proposed,
            });
            let revision_id = value_id(&item)?;
            records.push(item);
            records.push(BacklogRecordV2::Decision(Decision {
                project_id: self.project_id.clone(),
                batch_id: batch_record_id.clone(),
                revision_id,
                task_results: vec![proof.result_id.into(), proof.verification_id.into()],
                independent_verification_id: Some(proof.verification_id.into()),
                disposition: verdict,
                reason,
                evidence_ids: decision.evidence_ids.clone(),
                local_match,
            }));
            for member in group {
                let disposition = match verdict {
                    Verdict::Held => Settlement::Held,
                    Verdict::NeedsEvidence => Settlement::NeedsEvidence,
                    _ => Settlement::Accounted,
                };
                if outcomes
                    .insert(member.member_id.clone(), disposition)
                    .is_some()
                {
                    return Err("Repeated frozen member in native settlement".into());
                }
            }
        }
        if outcomes.len() != batch.members.len() {
            return Err("Native backlog settlement must account for all frozen members".into());
        }
        let mut journal = Vec::new();
        for (member_id, settlement) in outcomes {
            let record = Journal::Settled {
                batch_id: batch_id.clone(),
                member_id,
                settlement,
            };
            if state.apply(&intake, record.clone())? {
                journal.push(record);
            }
        }
        running(cancellation, proof.deadline)?;
        if !settled {
            self.append_processing(batch_id, records)?;
        }
        if !journal.is_empty() {
            self.append_batch_journal(journal)?;
        }
        Ok(receipt)
    }

    pub fn dispatch_native_backlog(
        &mut self,
        proof: &NativeAssessment<'_>,
        settlement: &str,
        cancellation: &AtomicBool,
    ) -> Result<Option<String>, String> {
        let (state, intake) = self.batch_state()?;
        let batch = proof.collection;
        let batch_id = &batch.reference.batch_id;
        let pin =
            self.check_native_batch(&state, &batch.reference, &batch.scenario_id, proof.limits)?;
        let events = self.processing()?;
        let destination = &pin.policy.destination_id;
        let local = *destination == local_destination(&self.project_id)?;
        if settlement_id(proof)? != settlement {
            return Err("Backlog dispatch differs from the native settlement".into());
        }
        check_settlement(&self.project_id, &events, proof)?;
        if state.batches[batch_id]
            .members
            .values()
            .any(|member| member.settlement.is_none())
        {
            return Err("backlog dispatch requires all frozen member dispositions".into());
        }
        let items: BTreeMap<_, _> = self
            .verified_backlog()?
            .into_iter()
            .map(|item| (item.revision_id.clone(), item))
            .collect();
        let mut published_or_reserved = BTreeSet::new();
        let mut eligible = BTreeMap::new();
        let mut source_revisions = BTreeSet::new();
        let mut authorized_batches = BTreeSet::new();
        let mut pending_deliveries = Vec::new();
        let mut resolved_deliveries = Vec::new();
        let mut admitted = BTreeMap::new();
        let mut mapped_items = BTreeMap::new();
        let mut other_pending = BTreeMap::new();
        for (position, event) in events.iter().enumerate() {
            if event.correlation_id.is_none() {
                continue;
            }
            match read_record(&self.project_id, event)? {
                BacklogRecordV2::Batch(record) if record.policy_id == batch.reference.policy_id => {
                    authorized_batches.insert(value_id(&BacklogRecordV2::Batch(record))?);
                }
                BacklogRecordV2::ItemRevision(item)
                    if item.source_revision == batch.source_revision =>
                {
                    source_revisions.insert(value_id(&BacklogRecordV2::ItemRevision(item))?);
                }
                BacklogRecordV2::Decision(decision)
                    if decision.disposition == Verdict::Verified
                        && decision.independent_verification_id.is_some()
                        && authorized_batches.contains(&decision.batch_id) =>
                {
                    if let Some(item) = items.get(&decision.revision_id)
                        && source_revisions.contains(&item.revision_id)
                    {
                        let decision_id = value_id(&BacklogRecordV2::Decision(decision))?;
                        if event.correlation_id.as_deref() == Some(batch_id)
                            || state
                                .pending_deliveries
                                .get(&item.revision_id)
                                .is_some_and(|pending| pending.decision_id == decision_id)
                        {
                            eligible.insert(&item.revision_id, (position, decision_id));
                        }
                    }
                }
                BacklogRecordV2::Publication(publication)
                    if publication.destination_id == *destination =>
                {
                    if event.correlation_id.as_deref() == Some(batch_id) {
                        admitted.insert(publication.intent_id.clone(), publication.clone());
                    } else {
                        other_pending.insert(
                            publication.item_id.clone(),
                            publication.receipt_id.is_none(),
                        );
                    }
                    if let Some(remote) = publication.remote_id
                        && mapped_items
                            .insert(publication.item_id, remote.clone())
                            .is_some_and(|mapped| mapped != remote)
                        && !local
                    {
                        return Err("Conflicting GitHub issue identities in Project history".into());
                    }
                    published_or_reserved.insert(publication.revision_id);
                }
                _ => {}
            }
        }
        let reservations = state.batches[batch_id].publication_reservations.len();
        let slots = (pin.policy.publication_limit as usize)
            .checked_sub(admitted.len().saturating_add(reservations))
            .ok_or("Backlog publication intents exceed the pinned batch cap")?;
        if state.batches[batch_id].closed_unix_ms.is_none()
            && pin.policy.hold_policy == HoldPolicy::AllowLocalReservations
        {
            let mut eligible = eligible.iter().collect::<Vec<_>>();
            eligible.sort_by_key(|(_, (position, _))| *position);
            let mut intents = Vec::new();
            for (revision, (_, decision_id)) in eligible {
                if published_or_reserved.contains(*revision) {
                    continue;
                }
                if intents.len() == slots {
                    if !state.pending_deliveries.contains_key(*revision) {
                        pending_deliveries.push(PendingDelivery {
                            revision_id: (*revision).clone(),
                            decision_id: decision_id.clone(),
                        });
                    }
                    continue;
                }
                let item = &items[*revision];
                if !local && other_pending.get(&item.item_id) == Some(&true) {
                    return Ok(None);
                }
                let domain = if local {
                    "af/backlog-local-intent@1"
                } else {
                    "af/backlog-github-intent@1"
                };
                let intent_id = value_id(&(domain, destination, revision))?;
                let attempt_ids = if local {
                    vec![value_id(&("af/backlog-local-attempt@1", &intent_id))?]
                } else {
                    github::check_fields(&item.public)?;
                    Vec::new()
                };
                let intent = Publication {
                    project_id: self.project_id.clone(),
                    item_id: item.item_id.clone(),
                    revision_id: item.revision_id.clone(),
                    decision_id: decision_id.clone(),
                    destination_id: destination.clone(),
                    intent_id: intent_id.clone(),
                    attempt_ids,
                    remote_id: None,
                    provider_state_id: None,
                    receipt_id: None,
                };
                admitted.insert(intent_id, intent.clone());
                intents.push(BacklogRecordV2::Publication(intent));
            }
            if !intents.is_empty() {
                self.check_native_batch(
                    &state,
                    &batch.reference,
                    &batch.scenario_id,
                    proof.limits,
                )?;
                if !local && !github::check_destination(self, destination) {
                    return Ok(None);
                }
                running(cancellation, proof.deadline)?;
                self.append_processing(batch_id, intents)?;
            }
        }
        let mut receipts = BTreeMap::new();
        let mut delivered = BTreeSet::new();
        for mut publication in admitted.into_values() {
            if publication.receipt_id.is_none() {
                running(cancellation, proof.deadline)?;
                if eligible
                    .get(&publication.revision_id)
                    .is_none_or(|(_, decision)| decision != &publication.decision_id)
                {
                    return Err(
                        "Backlog delivery lacks the current verified revision and decision".into(),
                    );
                }
                let live = self.check_native_batch(
                    &state,
                    &batch.reference,
                    &batch.scenario_id,
                    proof.limits,
                )?;
                if live.policy.hold_policy != HoldPolicy::AllowLocalReservations {
                    return Err("Project hold prevents backlog delivery".into());
                }
                let item = items
                    .get(&publication.revision_id)
                    .ok_or("Backlog delivery revision is no longer current")?;
                if !local {
                    if !self.publish_github_backlog(
                        proof,
                        &state,
                        &mut publication,
                        item,
                        &mapped_items,
                        cancellation,
                    )? {
                        return Ok(None);
                    }
                    if let Some(remote) = &publication.remote_id {
                        mapped_items.insert(publication.item_id.clone(), remote.clone());
                    }
                    delivered.insert(publication.revision_id.clone());
                    receipts.insert(
                        publication.intent_id,
                        publication.receipt_id.ok_or("Missing GitHub receipt")?,
                    );
                    continue;
                }
                let Ok(receipt) = self.deliver_local(item, &publication.intent_id) else {
                    // Keep the exact durable intent pending when the local sink cannot confirm.
                    return Ok(None);
                };
                publication.remote_id = Some(publication.intent_id.clone());
                publication.receipt_id = Some(receipt);
                if self
                    .append_processing(
                        batch_id,
                        vec![BacklogRecordV2::Publication(publication.clone())],
                    )
                    .is_err()
                {
                    return Ok(None);
                }
            }
            delivered.insert(publication.revision_id.clone());
            receipts.insert(
                publication.intent_id,
                publication
                    .receipt_id
                    .ok_or("Missing backlog delivery receipt")?,
            );
        }
        for revision_id in state.pending_deliveries.keys() {
            let outcome =
                if published_or_reserved.contains(revision_id) || delivered.contains(revision_id) {
                    DeliveryOutcome::Delivered
                } else if !eligible.contains_key(revision_id) {
                    DeliveryOutcome::RequiresReverification
                } else {
                    continue;
                };
            resolved_deliveries.push(DeliveryResolution {
                revision_id: revision_id.clone(),
                outcome,
            });
        }
        let mut closed = state;
        running(cancellation, proof.deadline)?;
        let at_unix_ms = closed.clock.max(crate::now_unix_ms());
        let close = Journal::Closed {
            batch_id: batch_id.clone(),
            at_unix_ms,
            pending_deliveries,
            resolved_deliveries,
        };
        if closed.apply(&intake, close.clone())? {
            self.append_batch_journal(vec![close])?;
        }
        value_id(&("af/backlog-dispatch-proof@1", settlement, receipts)).map(Some)
    }

    fn deliver_local(&self, item: &VerifiedItem, intent: &str) -> Result<String, String> {
        let directory = self.directory.join("backlog-local-sink");
        local_fs::create_private_dir_all(&directory).map_err(|e| e.to_string())?;
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::symlink_metadata(&directory).map_err(|e| e.to_string())?;
        if !metadata.is_dir()
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err("Backlog local sink must be an owned directory".into());
        }
        let filename = intent
            .strip_prefix("sha256:")
            .filter(|_| af_contracts::is_digest(intent))
            .ok_or("Invalid backlog local sink intent")?;
        let path = directory.join(format!("{filename}.json"));
        let bytes = serde_json::to_vec(item).map_err(|e| e.to_string())?;
        let mut staged = tempfile::NamedTempFile::new_in(&directory).map_err(|e| e.to_string())?;
        staged
            .write_all(&bytes)
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        match staged.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        if local_fs::read_bounded(&path, bytes.len() as u64).map_err(|e| e.to_string())? != bytes {
            return Err("Local backlog sink already contains different delivery bytes".into());
        }
        std::fs::File::open(&directory)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(af_store::canonical::blob_content_id(&bytes))
    }
}

fn running(cancellation: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        return Err("Backlog action cancelled before local mutation".into());
    }
    if Instant::now() >= deadline {
        return Err("Backlog action reached its deadline before local mutation".into());
    }
    Ok(())
}

fn settlement_id(proof: &NativeAssessment<'_>) -> Result<String, String> {
    value_id(&(
        "af/backlog-settlement-proof@1",
        &proof.collection.reference,
        proof.result_id,
        proof.verification_id,
    ))
}

fn check_settlement(
    project: &str,
    events: &[RunEvent],
    proof: &NativeAssessment<'_>,
) -> Result<(), String> {
    let mut count = 0;
    let mut batch_found = false;
    let expected = settlement_id(proof)?;
    for event in events.iter().filter(|event| {
        event.correlation_id.as_deref() == Some(&proof.collection.reference.batch_id)
    }) {
        let record = read_record(project, event)?;
        if let BacklogRecordV2::Batch(batch) = &record {
            if batch_found || batch.settlement_id.as_deref() != Some(&expected) {
                return Err("Backlog batch was settled by a different native receipt".into());
            }
            batch_found = true;
        }
        if let BacklogRecordV2::Decision(decision) = record {
            if decision.task_results != [proof.result_id, proof.verification_id]
                || decision.independent_verification_id.as_deref() != Some(proof.verification_id)
            {
                return Err("Backlog batch was settled by a different native receipt".into());
            }
            count += 1;
        }
    }
    if count != proof.decisions.len() || !batch_found {
        return Err("Backlog batch lacks the complete native settlement".into());
    }
    Ok(())
}
