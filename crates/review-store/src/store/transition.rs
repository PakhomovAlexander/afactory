//! Campaign transition validation: the rules every appended batch must satisfy against the
//! Campaign's durable state before its rows land.

use review_core::EventType;
use review_core::definition::{NodeKindSpec, SlicingSpec};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::cas::Cas;
use crate::store::artifacts::{
    validate_artifact_payload, validate_plan_ports, validate_prepared_refusal_history,
    validated_envelope_payload,
};
use crate::store::authority::{
    AuthorityPlan, cache_kind_name, dynamic_node_authority, load_authority_plan,
    load_authority_plan_id, pinned_cache_kind,
};
use crate::store::integration::{
    IntegrationTarget, validate_integration_commit_authority, validate_integration_plan_authority,
};
use crate::store::proposals::{
    manifest_entries, prepared_proposal_authority, proposal_candidate, selected_attempt_result,
    validate_accepted_proposal_claims, validate_candidate_manifest,
};
use crate::store::reports::{
    report_closes, validate_report_cache_snapshots, validate_report_gate_bindings,
    validate_report_plan, validate_report_receipts,
};
use crate::store::{
    NewEvent, PreparedArtifacts, StoreError, derive_event_id, latest_round,
    round_has_terminal_report,
};

pub(crate) fn validate_campaign_transition(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    events: &[NewEvent],
    first_sequence: i64,
    prepared: &PreparedArtifacts,
) -> Result<(), StoreError> {
    let campaign_opened: i64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND type = 'CampaignOpened@1'",
        params![run_id],
        |row| row.get(0),
    )?;
    let mut opened = campaign_opened > 0;
    let needs_authority_plan = events
        .iter()
        .any(|event| event_uses_authority_plan(event.event_type));
    let mut authority_plan = if opened && needs_authority_plan {
        Some(load_authority_plan(tx, cas, run_id)?)
    } else {
        None
    };
    let mut active = latest_round(tx, run_id)?;
    let mut active_subject: Option<(String, review_core::SubjectV1)> = None;
    let mut terminal = match &active {
        Some((event_id, _)) => round_has_terminal_report(tx, run_id, event_id)?,
        None => false,
    };
    let mut pending_supersession: Option<review_core::RoundInputSupersededPayloadV1> = None;
    let mut pending_fences = std::collections::BTreeSet::new();
    let mut batch_dispatches = std::collections::BTreeMap::new();
    let mut batch_latest_dispatch = std::collections::BTreeMap::new();
    let mut batch_attempt_inputs = std::collections::BTreeMap::new();
    let mut batch_attempt_feedback = std::collections::BTreeMap::new();
    let mut batch_terminals: std::collections::BTreeMap<String, EventType> =
        std::collections::BTreeMap::new();
    let mut batch_terminal_nodes = std::collections::BTreeMap::new();
    let mut batch_selected = std::collections::BTreeMap::new();
    let mut batch_proposal_attempts = std::collections::BTreeSet::new();
    let mut batch_prepared_proposals = std::collections::BTreeMap::new();
    let mut batch_accepted_proposals = std::collections::BTreeSet::new();
    let mut batch_invocations = std::collections::BTreeSet::new();
    let mut batch_receipts = std::collections::BTreeSet::new();
    let mut batch_findings = std::collections::BTreeSet::new();
    let mut batch_demands = std::collections::BTreeSet::new();
    let batch_integration_attestations: std::collections::BTreeSet<String> = events
        .iter()
        .filter(|event| event.event_type == EventType::ChangeAttestedV1)
        .filter_map(|event| {
            serde_json::from_value::<review_core::RecordedArtifactPayloadV1>(event.payload.clone())
                .ok()
                .map(|payload| payload.artifact_id)
        })
        .collect();
    let mut active_groupings = load_active_groupings(tx, run_id)?;
    let mut batch_provider_operations: std::collections::BTreeMap<
        String,
        review_core::ProviderOperationTransitionPayloadV1,
    > = std::collections::BTreeMap::new();

    for (offset, event) in events.iter().enumerate() {
        let sequence = first_sequence
            .checked_add(i64::try_from(offset).map_err(|_| {
                StoreError::Conflict("event batch is too large for transition validation".into())
            })?)
            .ok_or_else(|| StoreError::Conflict("event sequence overflow".into()))?;
        let event_id = derive_event_id(run_id, sequence);
        match event.event_type {
            EventType::CampaignOpenedV1 => {
                if opened {
                    return Err(StoreError::Conflict(
                        "CampaignOpened@1 already exists for this run".into(),
                    ));
                }
                opened = true;
                let payload: review_core::CampaignOpenedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                if !event.artifact_refs.contains(&payload.campaign_manifest_id)
                    || !event.artifact_refs.contains(&payload.authority_snapshot_id)
                {
                    return Err(StoreError::Conflict(
                        "CampaignOpened@1 does not publish its manifest and authority snapshot"
                            .into(),
                    ));
                }
                authority_plan = Some(load_authority_plan_id(
                    cas,
                    &payload.campaign_manifest_id,
                    &payload.authority_snapshot_id,
                )?);
            }
            EventType::RoundInputSupersededV1 => {
                let payload: review_core::RoundInputSupersededPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                let Some((active_id, active_payload)) = &active else {
                    return Err(StoreError::Conflict(
                        "cannot supersede a Campaign with no active Round".into(),
                    ));
                };
                if event.causation_id.as_deref() != Some(active_id)
                    || terminal
                    || payload.round != active_payload.round
                    || payload.old_epoch != active_payload.epoch
                    || payload.old_subject_id != active_payload.subject_id
                {
                    return Err(StoreError::Conflict(
                        "RoundInputSuperseded@1 does not match the active Round epoch".into(),
                    ));
                }
                let published: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events
                     WHERE run_id = ?1 AND sequence > (
                         SELECT sequence FROM events WHERE event_id = ?2
                     ) AND (type = 'FindingReported@1'
                         OR (type = 'FindingResolved@1' AND causation_id = ?2))",
                    params![run_id, active_id],
                    |row| row.get(0),
                )?;
                if published > 0 {
                    return Err(StoreError::Conflict(
                        "cannot supersede a Round after it published finding state".into(),
                    ));
                }
                let mut statement = tx.prepare(
                    "SELECT dispatch.attempt_id FROM events AS dispatch
                     WHERE dispatch.run_id = ?1 AND dispatch.causation_id = ?2
                       AND dispatch.type = 'AttemptDispatched@1'
                       AND NOT EXISTS (
                           SELECT 1 FROM events AS terminal
                           WHERE terminal.run_id = dispatch.run_id
                             AND terminal.causation_id = dispatch.causation_id
                             AND terminal.attempt_id = dispatch.attempt_id
                             AND terminal.type IN ('AttemptAdmitted@1', 'AttemptFailed@1',
                                                   'AttemptFenced@1', 'AttemptReleased@1')
                       )",
                )?;
                pending_fences = statement
                    .query_map(params![run_id, active_id], |row| row.get::<_, String>(0))?
                    .collect::<Result<_, _>>()?;
                pending_supersession = Some(payload);
            }
            EventType::RoundStartedV1 => {
                let payload: review_core::RoundStartedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                if !opened {
                    return Err(StoreError::Conflict(
                        "RoundStarted@1 requires a durable CampaignOpened@1".into(),
                    ));
                }
                if let Some(superseded) = pending_supersession.take() {
                    let Some((active_id, _)) = &active else {
                        return Err(StoreError::Conflict(
                            "replacement RoundStarted@1 has no active predecessor".into(),
                        ));
                    };
                    if payload.round != superseded.round
                        || payload.epoch != superseded.new_epoch
                        || payload.subject_id != superseded.replacement_subject_id
                        || payload.campaign_manifest_id != superseded.campaign_manifest_id
                        || event.causation_id.as_deref() != Some(active_id)
                    {
                        return Err(StoreError::Conflict(
                            "replacement RoundStarted@1 disagrees with its supersession".into(),
                        ));
                    }
                    if !pending_fences.is_empty() {
                        return Err(StoreError::Conflict(format!(
                            "replacement RoundStarted@1 leaves {} outstanding attempts unfenced",
                            pending_fences.len()
                        )));
                    }
                } else if let Some((_, prior)) = &active {
                    if !terminal
                        || prior.round.checked_add(1) != Some(payload.round)
                        || payload.epoch != 1
                    {
                        return Err(StoreError::Conflict(
                            "RoundStarted@1 is neither the next closed Round nor an atomic supersession"
                                .into(),
                        ));
                    }
                } else if payload.round != 1 || payload.epoch != 1 {
                    return Err(StoreError::Conflict(
                        "the first RoundStarted@1 must be round 1 epoch 1".into(),
                    ));
                }
                active = Some((event_id, payload));
                active_subject = None;
                terminal = false;
            }
            event_type if round_runtime_event(event_type) => {
                if active.is_none() {
                    if event.legacy_import
                        && matches!(
                            event_type,
                            EventType::CheckCompletedV1
                                | EventType::FindingReportedV1
                                | EventType::GenerationAdvancedV1
                        )
                    {
                        if event_type == EventType::FindingReportedV1 {
                            let key = event.payload["key"].as_str().ok_or_else(|| {
                                StoreError::Conflict(
                                    "legacy FindingReported@1 has no finding key".into(),
                                )
                            })?;
                            batch_findings.insert(key.to_string());
                        }
                        continue;
                    }
                    return Err(StoreError::Conflict(format!(
                        "{event_type} requires an active Round"
                    )));
                }
                if event_type == EventType::RunReportV1 {
                    return Err(StoreError::Conflict(
                        "RunReport@1 is replay-only and cannot be appended".into(),
                    ));
                }
                if let Some((active_id, active_payload)) = &active {
                    let plan = authority_plan.as_ref();
                    if active_subject
                        .as_ref()
                        .is_none_or(|(id, _)| id != &active_payload.subject_id)
                    {
                        let subject: review_core::SubjectV1 = serde_json::from_value(
                            cas.get_json(&active_payload.subject_id)
                                .map_err(|error| StoreError::Conflict(error.to_string()))?,
                        )?;
                        active_subject = Some((active_payload.subject_id.clone(), subject));
                    }
                    let subject = &active_subject.as_ref().expect("active Subject cached").1;
                    let subject_snapshot_id = &subject.head_snapshot_id;
                    let subject_base_snapshot_id = &subject.base_snapshot_id;
                    let subject_change_set_id = &subject.change_set_id;
                    let authority_revoked_receipt =
                        if event_type == EventType::BrokerOperationCompletedV1 {
                            let receipt: review_core::BrokerOperationReceiptV1 =
                                serde_json::from_value(event.payload.clone())?;
                            receipt.outcome == review_core::BrokerOperationOutcomeV1::Revoked
                                && receipt.failure_reason
                                    == Some(review_core::BrokerFailureReasonV1::AuthorityRevoked)
                        } else {
                            false
                        };
                    if terminal && !authority_revoked_receipt {
                        return Err(StoreError::Conflict(format!(
                            "{event_type} cannot publish after the active Round concluded"
                        )));
                    }
                    if event.causation_id.as_deref() != Some(active_id)
                        && !authority_revoked_receipt
                    {
                        return Err(StoreError::Conflict(format!(
                            "{event_type} is not bound to the active Round epoch"
                        )));
                    }
                    if event.causation_id.as_deref() != Some(active_id) {
                        let receipt_round = event.causation_id.as_deref().ok_or_else(|| {
                            StoreError::Conflict(
                                "late revoked Broker receipt has no Round causation".into(),
                            )
                        })?;
                        let prior_round: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events
                             WHERE run_id = ?1 AND event_id = ?2 AND type = 'RoundStarted@1'",
                            params![run_id, receipt_round],
                            |row| row.get(0),
                        )?;
                        if prior_round != 1 {
                            return Err(StoreError::Conflict(
                                "late revoked Broker receipt has no durable prior Round".into(),
                            ));
                        }
                    }
                    if event.attempt_id.as_deref().is_some_and(|attempt| {
                        attempt.len() != 26
                            || !attempt
                                .bytes()
                                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                    }) {
                        return Err(StoreError::Conflict(format!(
                            "{event_type} carries a non-schema attempt ID"
                        )));
                    }
                    match event_type {
                        EventType::ReviewerExecutionBoundV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(
                                    "ReviewerExecutionBound@1 has no node ID".into(),
                                )
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(
                                    "ReviewerExecutionBound@1 has no Attempt ID".into(),
                                )
                            })?;
                            let binding: review_core::ReviewerExecutionBindingV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if binding.node != node
                                || binding.attempt_id != attempt
                                || !binding.admitted
                            {
                                return Err(StoreError::Conflict(
                                    "ReviewerExecutionBound@1 metadata or admission disagrees with its payload"
                                        .into(),
                                ));
                            }
                            let expected = plan
                                .and_then(|plan| plan.reviewer_execution_for(node))
                                .ok_or_else(|| {
                                    StoreError::Conflict(format!(
                                        "reviewer Execution Binding node '{node}' is absent from pinned v4 authority"
                                    ))
                                })?;
                            if binding.credential_mode != expected.credential_mode
                                || binding.auto_apply != expected.auto_apply
                                || binding.operations != expected.operations
                            {
                                return Err(StoreError::Conflict(
                                    "ReviewerExecutionBound@1 contradicts pinned reviewer authority"
                                        .into(),
                                ));
                            }
                            let dispatched: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'AttemptDispatched@1' AND node_id = ?3 AND attempt_id = ?4",
                                params![run_id, active_id, node, attempt],
                                |row| row.get(0),
                            )?;
                            let duplicate: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'ReviewerExecutionBound@1' AND attempt_id = ?3",
                                params![run_id, active_id, attempt],
                                |row| row.get(0),
                            )?;
                            let node_dispatches: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'AttemptDispatched@1' AND node_id = ?3",
                                params![run_id, active_id, node],
                                |row| row.get(0),
                            )?;
                            if dispatched != 1
                                || duplicate != 0
                                || u64::try_from(node_dispatches).ok() != Some(binding.lease_epoch)
                            {
                                return Err(StoreError::Conflict(
                                    "Reviewer Execution Binding requires the exact current dispatch epoch and may bind an Attempt only once"
                                        .into(),
                                ));
                            }
                        }
                        EventType::BrokerOperationCompletedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(
                                    "BrokerOperationCompleted@1 has no node ID".into(),
                                )
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(
                                    "BrokerOperationCompleted@1 has no Attempt ID".into(),
                                )
                            })?;
                            let receipt: review_core::BrokerOperationReceiptV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if receipt.node != node || receipt.attempt_id != attempt {
                                return Err(StoreError::Conflict(
                                    "BrokerOperationCompleted@1 metadata disagrees with its receipt"
                                        .into(),
                                ));
                            }
                            let receipt_round_id =
                                event.causation_id.as_deref().ok_or_else(|| {
                                    StoreError::Conflict(
                                        "BrokerOperationCompleted@1 has no Round causation".into(),
                                    )
                                })?;
                            let latest_attempt: Option<String> = tx
                                .query_row(
                                    "SELECT attempt_id FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2
                                       AND type = 'AttemptDispatched@1' AND node_id = ?3
                                     ORDER BY sequence DESC LIMIT 1",
                                    params![run_id, receipt_round_id, node],
                                    |row| row.get(0),
                                )
                                .optional()?;
                            let terminal: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2 AND attempt_id = ?3
                                   AND type IN ('AttemptAdmitted@1', 'AttemptFailed@1',
                                                'AttemptFenced@1', 'AttemptReleased@1')",
                                params![run_id, receipt_round_id, attempt],
                                |row| row.get(0),
                            )?;
                            if receipt.outcome != review_core::BrokerOperationOutcomeV1::Revoked
                                && (latest_attempt.as_deref() != Some(attempt) || terminal > 0)
                            {
                                return Err(StoreError::AttemptNotCurrent);
                            }
                            if receipt_round_id != active_id.as_str() && terminal == 0 {
                                return Err(StoreError::Conflict(
                                    "late revoked Broker receipt has no fenced prior Attempt"
                                        .into(),
                                ));
                            }
                            let binding_payload: String = tx
                                .query_row(
                                    "SELECT payload FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2
                                       AND type = 'ReviewerExecutionBound@1'
                                       AND node_id = ?3 AND attempt_id = ?4
                                     ORDER BY sequence DESC LIMIT 1",
                                    params![run_id, receipt_round_id, node, attempt],
                                    |row| row.get(0),
                                )
                                .optional()?
                                .ok_or_else(|| {
                                    StoreError::Conflict(
                                        "Broker operation has no durable reviewer Execution Binding"
                                            .into(),
                                    )
                                })?;
                            let binding: review_core::ReviewerExecutionBindingV1 =
                                serde_json::from_str(&binding_payload)?;
                            if binding.credential_mode
                                != review_core::BrokerCredentialModeV1::Brokered
                                || binding.broker_handle.as_deref()
                                    != Some(receipt.handle_id.as_str())
                                || binding.lease_epoch != receipt.lease_epoch
                            {
                                return Err(StoreError::Conflict(
                                    "Broker operation receipt contradicts its durable handle binding"
                                        .into(),
                                ));
                            }
                            let policy = binding
                                .operations
                                .iter()
                                .find(|policy| policy.name == receipt.operation)
                                .ok_or_else(|| {
                                    StoreError::Conflict(
                                        "Broker operation is absent from durable project authority"
                                            .into(),
                                    )
                                })?;
                            if policy.destination != receipt.destination
                                || policy.method != receipt.method
                            {
                                return Err(StoreError::Conflict(
                                    "Broker operation receipt exceeds or contradicts its pinned policy"
                                        .into(),
                                ));
                            }
                            let mut statement = tx.prepare(
                                "SELECT payload FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'BrokerOperationCompleted@1'
                                   AND node_id = ?3 AND attempt_id = ?4
                                 ORDER BY sequence",
                            )?;
                            let prior_receipts = statement
                                .query_map(
                                    params![run_id, receipt_round_id, node, attempt],
                                    |row| row.get::<_, String>(0),
                                )?
                                .collect::<Result<Vec<_>, _>>()?;
                            if prior_receipts.len().checked_add(1)
                                != usize::try_from(receipt.ordinal).ok()
                            {
                                return Err(StoreError::Conflict(
                                    "Broker operation receipt ordinal is not dense for its Attempt"
                                        .into(),
                                ));
                            }
                            let mut prior_calls = 0_u32;
                            let mut prior_charged = 0_u64;
                            let mut prior_total_charged = 0_u64;
                            let mut prior_terminal_broker_state = false;
                            for prior in prior_receipts {
                                let prior: review_core::BrokerOperationReceiptV1 =
                                    serde_json::from_str(&prior)?;
                                prior_total_charged = prior_total_charged
                                    .checked_add(prior.charged_usage)
                                    .ok_or_else(|| {
                                        StoreError::Conflict("Broker Attempt usage overflow".into())
                                    })?;
                                if prior.operation != receipt.operation {
                                    if broker_receipt_terminates_handle(&prior) {
                                        prior_terminal_broker_state = true;
                                    }
                                    continue;
                                }
                                if broker_receipt_consumes_call(&prior) {
                                    prior_calls = prior_calls.checked_add(1).ok_or_else(|| {
                                        StoreError::Conflict(
                                            "Broker operation call count overflow".into(),
                                        )
                                    })?;
                                    prior_charged = prior_charged
                                        .checked_add(prior.charged_usage.min(prior.reserved_usage))
                                        .ok_or_else(|| {
                                            StoreError::Conflict(
                                                "Broker operation usage overflow".into(),
                                            )
                                        })?;
                                }
                                if broker_receipt_terminates_handle(&prior) {
                                    prior_terminal_broker_state = true;
                                }
                            }
                            let projected_usage = prior_charged.checked_add(receipt.reserved_usage);
                            let consumes_call = broker_receipt_consumes_call(&receipt);
                            if prior_terminal_broker_state
                                && !(receipt.outcome
                                    == review_core::BrokerOperationOutcomeV1::Revoked
                                    && receipt.failure_reason
                                        == Some(
                                            review_core::BrokerFailureReasonV1::AuthorityRevoked,
                                        )
                                    && !consumes_call
                                    && receipt.response_digest.is_none()
                                    && receipt.response_bytes == 0
                                    && receipt.charged_usage == 0)
                            {
                                return Err(StoreError::Conflict(
                                    "Broker operation receipt follows terminal handle revocation"
                                        .into(),
                                ));
                            }
                            let exact_policy_result = match (
                                receipt.outcome,
                                receipt.failure_reason,
                                consumes_call,
                            ) {
                                (
                                    review_core::BrokerOperationOutcomeV1::Refused,
                                    Some(review_core::BrokerFailureReasonV1::RequestTooLarge),
                                    false,
                                ) => receipt.request_bytes > policy.max_request_bytes,
                                (
                                    review_core::BrokerOperationOutcomeV1::Refused,
                                    Some(review_core::BrokerFailureReasonV1::QuotaExceeded),
                                    false,
                                ) => {
                                    receipt.request_bytes <= policy.max_request_bytes
                                        && (receipt.reserved_usage == 0
                                            || prior_calls >= policy.max_calls
                                            || projected_usage
                                                .is_none_or(|usage| usage > policy.max_usage))
                                }
                                (_, _, true) => receipt.request_bytes <= policy.max_request_bytes
                                    && receipt.reserved_usage > 0
                                    && prior_calls < policy.max_calls
                                    && projected_usage
                                        .is_some_and(|usage| usage <= policy.max_usage)
                                    && match receipt.failure_reason {
                                        None => receipt.response_bytes <= policy.max_response_bytes,
                                        Some(
                                            review_core::BrokerFailureReasonV1::ResponseTooLarge,
                                        ) => receipt.response_bytes > policy.max_response_bytes,
                                        Some(
                                            review_core::BrokerFailureReasonV1::ConnectorFailed
                                            | review_core::BrokerFailureReasonV1::AuthorityRevoked,
                                        ) => true,
                                        Some(
                                            review_core::BrokerFailureReasonV1::CredentialExposure,
                                        ) => receipt.charged_usage <= receipt.reserved_usage,
                                        Some(review_core::BrokerFailureReasonV1::UsageOverrun) => {
                                            receipt.charged_usage > receipt.reserved_usage
                                        }
                                        _ => false,
                                    },
                                (
                                    review_core::BrokerOperationOutcomeV1::Revoked,
                                    Some(review_core::BrokerFailureReasonV1::AuthorityRevoked),
                                    false,
                                ) => true,
                                _ => false,
                            };
                            if !exact_policy_result {
                                return Err(StoreError::Conflict(
                                    "Broker operation receipt exceeds or contradicts its pinned policy"
                                        .into(),
                                ));
                            }
                            if terminal > 0
                                && receipt.outcome == review_core::BrokerOperationOutcomeV1::Revoked
                            {
                                prior_total_charged
                                    .checked_add(receipt.charged_usage)
                                    .ok_or_else(|| {
                                        StoreError::Conflict("Broker Attempt usage overflow".into())
                                    })?;
                                // Late observed usage can exceed a conservative fence. The exact
                                // receipt remains durable, and spend projections charge the
                                // greater of terminal settlement and observed receipt usage.
                            }
                        }
                        EventType::ProviderOperationTransitionV1 => {
                            let transition: review_core::ProviderOperationTransitionPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if transition.round != active_payload.round
                                || transition.round_epoch != active_payload.epoch
                                || event.node_id.as_deref() != Some(transition.node_id.as_str())
                                || event.attempt_id != transition.attempt_id
                                || event.correlation_id.as_deref()
                                    != Some(transition.operation_id.as_str())
                            {
                                return Err(StoreError::Conflict(
                                    "ProviderOperationTransition@1 is not bound to its active Round, node, attempt, and operation".into(),
                                ));
                            }
                            let previous = match batch_provider_operations
                                .get(&transition.operation_id)
                            {
                                Some(previous) => Some(previous.clone()),
                                None => tx
                                    .query_row(
                                        "SELECT payload FROM events WHERE run_id = ?1 AND type = 'ProviderOperationTransition@1' AND correlation_id = ?2 ORDER BY sequence DESC LIMIT 1",
                                        params![run_id, transition.operation_id],
                                        |row| row.get::<_, String>(0),
                                    )
                                    .optional()?
                                    .map(|payload| serde_json::from_str(&payload))
                                    .transpose()?,
                            };
                            transition
                                .validate_after(previous.as_ref())
                                .map_err(StoreError::Conflict)?;
                            batch_provider_operations
                                .insert(transition.operation_id.clone(), transition);
                        }
                        EventType::GateExecutionBoundV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("GateExecutionBound@1 has no node ID".into())
                            })?;
                            let binding: review_core::RunExecutionBindingV4 =
                                serde_json::from_value(event.payload.clone())?;
                            if binding.node != node {
                                return Err(StoreError::Conflict(
                                    "GateExecutionBound@1 metadata disagrees with its payload"
                                        .into(),
                                ));
                            }
                            if plan.is_none_or(|plan| !plan.gate_nodes.contains(node)) {
                                return Err(StoreError::Conflict(format!(
                                    "Gate Execution Binding node '{node}' is absent from the pinned Gate plan"
                                )));
                            }
                        }
                        EventType::CacheSnapshotMaterializedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(
                                    "CacheSnapshotMaterialized@1 has no node ID".into(),
                                )
                            })?;
                            let snapshot: review_core::RunCacheSnapshotV5 =
                                serde_json::from_value(event.payload.clone())?;
                            if snapshot.node != node {
                                return Err(StoreError::Conflict(
                                    "CacheSnapshotMaterialized@1 metadata disagrees with its payload"
                                        .into(),
                                ));
                            }
                            if plan.is_none_or(|plan| !plan.gate_nodes.contains(node)) {
                                return Err(StoreError::Conflict(format!(
                                    "Cache Snapshot node '{node}' is absent from the pinned Gate plan"
                                )));
                            }
                            let kind = pinned_cache_kind(snapshot.kind);
                            if plan.is_none_or(|plan| !plan.cache_kinds.contains(&kind)) {
                                return Err(StoreError::Conflict(format!(
                                    "Cache Snapshot kind '{}' is absent from pinned authority",
                                    cache_kind_name(kind)
                                )));
                            }
                            let has_manifest = event
                                .artifact_refs
                                .iter()
                                .any(|artifact| artifact == &snapshot.source_digest);
                            let receipt_bytes = crate::canonical::canonicalize(&event.payload)
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            let receipt_artifact =
                                crate::canonical::blob_content_id(&receipt_bytes);
                            let has_receipt = event.artifact_refs.contains(&receipt_artifact);
                            if !has_manifest || !has_receipt {
                                return Err(StoreError::Conflict(
                                    "CacheSnapshotMaterialized@1 lacks its manifest or exact receipt artifact"
                                        .into(),
                                ));
                            }
                            let manifest =
                                prepared.json.get(&snapshot.source_digest).ok_or_else(|| {
                                    StoreError::Conflict(
                                        "Cache Snapshot manifest was not prepared".into(),
                                    )
                                })?;
                            let manifest: review_core::CacheManifestV1 =
                                serde_json::from_value(manifest.clone())?;
                            manifest.validate().map_err(StoreError::Conflict)?;
                            if manifest.kind != snapshot.kind
                                || u64::try_from(manifest.entries.len()).ok()
                                    != Some(snapshot.files)
                                || manifest.bytes() != snapshot.bytes
                            {
                                return Err(StoreError::Conflict(
                                    "Cache Snapshot receipt contradicts CacheManifest@1".into(),
                                ));
                            }
                        }
                        EventType::NodeInvocationV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("NodeInvocation@1 has no node ID".into())
                            })?;
                            let invocation: review_core::NodeInvocationPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if invocation.node != node {
                                return Err(StoreError::Conflict(
                                    "NodeInvocation@1 metadata disagrees with its payload".into(),
                                ));
                            }
                            if let Some(plan) = plan {
                                if let Some(expected) = plan.nodes.get(node) {
                                    validate_plan_ports(
                                        prepared,
                                        &expected.inputs,
                                        &invocation.inputs,
                                        subject_snapshot_id,
                                        subject_base_snapshot_id.as_deref(),
                                        subject_change_set_id.as_deref(),
                                    )?;
                                } else {
                                    let dynamic = dynamic_node_authority(
                                        tx, cas, run_id, active_id, plan, node,
                                    )?
                                    .ok_or_else(|| {
                                        StoreError::Conflict(format!(
                                            "node '{node}' is absent from the pinned Campaign plan and accepted Slice Sets"
                                        ))
                                    })?;
                                    validate_plan_ports(
                                        prepared,
                                        &dynamic.inputs,
                                        &invocation.inputs,
                                        subject_snapshot_id,
                                        subject_base_snapshot_id.as_deref(),
                                        subject_change_set_id.as_deref(),
                                    )?;
                                    let slice_record = invocation
                                        .inputs
                                        .iter()
                                        .find(|port| port.port == "slice")
                                        .and_then(|port| port.artifact_ids.first())
                                        .ok_or_else(|| {
                                            StoreError::Conflict(
                                                "dynamic invocation has no exact Slice artifact"
                                                    .into(),
                                            )
                                        })?;
                                    let value =
                                        prepared.json.get(slice_record).ok_or_else(|| {
                                            StoreError::Conflict(
                                                "dynamic invocation Slice was not prepared".into(),
                                            )
                                        })?;
                                    let slice: review_core::ReviewSliceV1 =
                                        validated_envelope_payload(
                                            value,
                                            review_core::contract::REVIEW_SLICE_V1,
                                        )?;
                                    if slice != dynamic.slice {
                                        return Err(StoreError::Conflict(
                                            "dynamic invocation Slice contradicts accepted SliceSet authority"
                                                .into(),
                                        ));
                                    }
                                }
                            }
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'NodeInvocation@1' AND node_id = ?3",
                                params![run_id, active_id, node],
                                |row| row.get(0),
                            )?;
                            if existing > 0 || !batch_invocations.insert(node.to_string()) {
                                return Err(StoreError::Conflict(format!(
                                    "node '{node}' already has a durable invocation"
                                )));
                            }
                        }
                        EventType::AttemptDispatchedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptDispatched@1 has no node ID".into())
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptDispatched@1 has no attempt ID".into())
                            })?;
                            let dispatch: review_core::event::AttemptDispatchedPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if plan.is_some_and(|plan| plan.budgeted) && dispatch.reserved.is_none()
                            {
                                return Err(StoreError::Conflict(
                                    "budgeted AttemptDispatched@1 has no reservation".into(),
                                ));
                            }
                            let provider: Option<String> = tx
                                .query_row(
                                    "SELECT payload FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2 AND node_id = ?3
                                       AND type = 'ProviderOperationTransition@1'
                                     ORDER BY sequence DESC LIMIT 1",
                                    params![run_id, active_id, node],
                                    |row| row.get(0),
                                )
                                .optional()?;
                            if let Some(provider) = provider {
                                let provider: review_core::ProviderOperationTransitionPayloadV1 =
                                    serde_json::from_str(&provider)?;
                                if provider.state != review_core::ProviderOperationStateV1::Done {
                                    return Err(StoreError::Conflict(format!(
                                        "attempt for node '{node}' dispatched without completed Provider Admission"
                                    )));
                                }
                            }
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2 AND attempt_id = ?3",
                                params![run_id, active_id, attempt],
                                |row| row.get(0),
                            )?;
                            if existing > 0
                                || batch_dispatches
                                    .insert(attempt.to_string(), node.to_string())
                                    .is_some()
                            {
                                return Err(StoreError::Conflict(format!(
                                    "attempt '{attempt}' was already dispatched"
                                )));
                            }
                            batch_latest_dispatch.insert(node.to_string(), attempt.to_string());
                        }
                        EventType::AttemptAdmittedV1
                        | EventType::AttemptFailedV1
                        | EventType::AttemptFencedV1
                        | EventType::AttemptReleasedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(format!("{event_type} has no node ID"))
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(format!("{event_type} has no attempt ID"))
                            })?;
                            let dispatched: Option<String> = tx
                                .query_row(
                                    "SELECT node_id FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2
                                       AND attempt_id = ?3 AND type = 'AttemptDispatched@1'
                                     LIMIT 1",
                                    params![run_id, active_id, attempt],
                                    |row| row.get(0),
                                )
                                .optional()?;
                            let dispatched =
                                dispatched.or_else(|| batch_dispatches.get(attempt).cloned());
                            if dispatched.as_deref() != Some(node) {
                                return Err(StoreError::Conflict(format!(
                                    "{event_type} has no matching dispatch"
                                )));
                            }
                            let admitted = (event_type == EventType::AttemptAdmittedV1)
                                .then(|| {
                                    serde_json::from_value::<
                                        review_core::event::AttemptAdmittedPayloadV1,
                                    >(event.payload.clone())
                                })
                                .transpose()?;
                            let quarantined = admitted
                                .as_ref()
                                .is_some_and(|payload| payload.selection == "quarantined");
                            let admitted_cost =
                                admitted.as_ref().map(|payload| payload.cost_tokens);
                            let existing_terminal: Option<String> = tx
                                .query_row(
                                    "SELECT type FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2 AND attempt_id = ?3
                                   AND type IN ('AttemptAdmitted@1', 'AttemptFailed@1',
                                                'AttemptFenced@1', 'AttemptReleased@1')
                                 ORDER BY sequence DESC LIMIT 1",
                                    params![run_id, active_id, attempt],
                                    |row| row.get(0),
                                )
                                .optional()?;
                            let prior_terminal = batch_terminals
                                .get(attempt)
                                .map(|event_type| event_type.as_str())
                                .or(existing_terminal.as_deref());
                            if prior_terminal.is_some()
                                && !(quarantined
                                    && prior_terminal == Some(EventType::AttemptFencedV1.as_str()))
                            {
                                return Err(StoreError::Conflict(format!(
                                    "attempt '{attempt}' already has a terminal event"
                                )));
                            }
                            if !quarantined {
                                batch_terminals.insert(attempt.to_string(), event_type);
                                batch_terminal_nodes.insert(attempt.to_string(), node.to_string());
                            }
                            if plan.is_some_and(|plan| plan.budgeted) {
                                let settled = match event_type {
                                    EventType::AttemptFailedV1 => {
                                        serde_json::from_value::<
                                            review_core::event::AttemptFailedPayloadV1,
                                        >(
                                            event.payload.clone()
                                        )?
                                        .charged
                                    }
                                    EventType::AttemptFencedV1 => {
                                        serde_json::from_value::<
                                            review_core::event::AttemptFencedPayloadV1,
                                        >(
                                            event.payload.clone()
                                        )?
                                        .charged
                                    }
                                    EventType::AttemptReleasedV1 => {
                                        serde_json::from_value::<
                                            review_core::event::AttemptReleasedPayloadV1,
                                        >(
                                            event.payload.clone()
                                        )?
                                        .released
                                    }
                                    EventType::AttemptAdmittedV1 => Some(0),
                                    _ => unreachable!(),
                                };
                                if settled.is_none() {
                                    return Err(StoreError::Conflict(format!(
                                        "budgeted {event_type} has no settled accounting"
                                    )));
                                }
                            }
                            if event_type == EventType::AttemptFencedV1 {
                                pending_fences.remove(attempt);
                            }
                            if let Some(admitted) = admitted {
                                if admitted.selection == "selected" {
                                    let latest: Option<String> = tx
                                        .query_row(
                                            "SELECT attempt_id FROM events
                                             WHERE run_id = ?1 AND causation_id = ?2
                                               AND node_id = ?3 AND type = 'AttemptDispatched@1'
                                             ORDER BY sequence DESC LIMIT 1",
                                            params![run_id, active_id, node],
                                            |row| row.get(0),
                                        )
                                        .optional()?;
                                    let latest =
                                        batch_latest_dispatch.get(node).cloned().or(latest);
                                    if latest.as_deref() != Some(attempt) {
                                        return Err(StoreError::Conflict(
                                            "only the latest reviewer attempt may be selected"
                                                .into(),
                                        ));
                                    }
                                    let result = admitted.result_artifact.ok_or_else(|| {
                                        StoreError::Conflict(
                                            "selected AttemptAdmitted@1 has no result artifact"
                                                .into(),
                                        )
                                    })?;
                                    let provenance =
                                        admitted.provenance_artifact.ok_or_else(|| {
                                            StoreError::Conflict(
                                                "selected AttemptAdmitted@1 has no provenance artifact"
                                                    .into(),
                                            )
                                        })?;
                                    if !event.artifact_refs.contains(&result)
                                        || !event.artifact_refs.contains(&provenance)
                                    {
                                        return Err(StoreError::Conflict(
                                            "selected AttemptAdmitted@1 does not publish its result and provenance"
                                                .into(),
                                        ));
                                    }
                                    batch_selected
                                        .insert(attempt.to_string(), (node.to_string(), result));
                                }
                            }
                            let expected_execution =
                                plan.and_then(|plan| plan.reviewer_execution_for(node));
                            let execution_binding = if expected_execution.is_some() {
                                tx.query_row(
                                    "SELECT payload FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2
                                       AND type = 'ReviewerExecutionBound@1'
                                       AND node_id = ?3 AND attempt_id = ?4
                                     ORDER BY sequence DESC LIMIT 1",
                                    params![run_id, active_id, node, attempt],
                                    |row| row.get::<_, String>(0),
                                )
                                .optional()?
                                .map(|payload| serde_json::from_str(&payload))
                                .transpose()?
                            } else {
                                None
                            };
                            if event_type == EventType::AttemptAdmittedV1
                                && expected_execution.is_some()
                                && execution_binding.is_none()
                            {
                                return Err(StoreError::Conflict(
                                    "v4 reviewer admission has no durable Execution Binding".into(),
                                ));
                            }
                            if let Some(binding) = execution_binding.as_ref().filter(
                                |binding: &&review_core::ReviewerExecutionBindingV1| {
                                    binding.credential_mode
                                        == review_core::BrokerCredentialModeV1::Brokered
                                },
                            ) {
                                let mut statement = tx.prepare(
                                    "SELECT payload FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2
                                       AND type = 'BrokerOperationCompleted@1'
                                       AND node_id = ?3 AND attempt_id = ?4",
                                )?;
                                let mut broker_charged = 0_u64;
                                for payload in statement
                                    .query_map(params![run_id, active_id, node, attempt], |row| {
                                        row.get::<_, String>(0)
                                    })?
                                {
                                    let receipt: review_core::BrokerOperationReceiptV1 =
                                        serde_json::from_str(&payload?)?;
                                    broker_charged = broker_charged
                                        .checked_add(receipt.charged_usage)
                                        .ok_or_else(|| {
                                            StoreError::Conflict(
                                                "Broker Attempt usage overflow".into(),
                                            )
                                        })?;
                                }
                                let authority_bound =
                                    review_core::broker_authority_usage(&binding.operations)
                                        .map_err(StoreError::Conflict)?;
                                let dispatch_payload: String = tx.query_row(
                                    "SELECT payload FROM events
                                     WHERE run_id = ?1 AND causation_id = ?2
                                       AND type = 'AttemptDispatched@1'
                                       AND node_id = ?3 AND attempt_id = ?4
                                     ORDER BY sequence DESC LIMIT 1",
                                    params![run_id, active_id, node, attempt],
                                    |row| row.get(0),
                                )?;
                                let dispatch: review_core::event::AttemptDispatchedPayloadV1 =
                                    serde_json::from_str(&dispatch_payload)?;
                                let settled = match event_type {
                                    EventType::AttemptAdmittedV1 => {
                                        admitted_cost.expect("parsed admitted payload")
                                    }
                                    EventType::AttemptFailedV1 => serde_json::from_value::<
                                        review_core::event::AttemptFailedPayloadV1,
                                    >(
                                        event.payload.clone()
                                    )?
                                    .charged
                                    .unwrap_or(0),
                                    EventType::AttemptFencedV1 => serde_json::from_value::<
                                        review_core::event::AttemptFencedPayloadV1,
                                    >(
                                        event.payload.clone()
                                    )?
                                    .charged
                                    .unwrap_or(0),
                                    EventType::AttemptReleasedV1 => 0,
                                    _ => unreachable!(),
                                };
                                let required = if event_type == EventType::AttemptFencedV1 {
                                    broker_charged
                                        .max(authority_bound)
                                        .max(dispatch.reserved.unwrap_or(0))
                                } else {
                                    broker_charged
                                };
                                let reconciled = if event_type == EventType::AttemptAdmittedV1 {
                                    settled == required
                                } else {
                                    settled >= required
                                };
                                if !reconciled {
                                    return Err(StoreError::Conflict(
                                        "Attempt settlement under-reports durable Broker usage or fence authority"
                                            .into()
                                    ));
                                }
                            }
                        }
                        EventType::AttemptInputV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptInput@1 has no node ID".into())
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptInput@1 has no attempt ID".into())
                            })?;
                            let input: review_core::event::AttemptInputPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if !event.artifact_refs.contains(&input.refusal_history_id) {
                                return Err(StoreError::Conflict(
                                    "AttemptInput@1 does not reference its refusal history".into(),
                                ));
                            }
                            validate_prepared_refusal_history(
                                prepared,
                                &input.refusal_history_id,
                                "AttemptInput@1",
                            )?;
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'AttemptInput@1' AND node_id = ?3 AND attempt_id = ?4",
                                params![run_id, active_id, node, attempt],
                                |row| row.get(0),
                            )?;
                            if existing > 0
                                || batch_attempt_inputs
                                    .insert(attempt.to_string(), node.to_string())
                                    .is_some()
                            {
                                return Err(StoreError::Conflict(
                                    "attempt has duplicate durable input events".into(),
                                ));
                            }
                        }
                        EventType::AttemptFeedbackV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptFeedback@1 has no node ID".into())
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("AttemptFeedback@1 has no attempt ID".into())
                            })?;
                            let feedback: review_core::event::AttemptFeedbackPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if !event.artifact_refs.contains(&feedback.refusal_history_id) {
                                return Err(StoreError::Conflict(
                                    "AttemptFeedback@1 does not reference its refusal history"
                                        .into(),
                                ));
                            }
                            validate_prepared_refusal_history(
                                prepared,
                                &feedback.refusal_history_id,
                                "AttemptFeedback@1",
                            )?;
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'AttemptFeedback@1' AND attempt_id = ?3",
                                params![run_id, active_id, attempt],
                                |row| row.get(0),
                            )?;
                            if existing > 0
                                || batch_attempt_feedback
                                    .insert(attempt.to_string(), node.to_string())
                                    .is_some()
                            {
                                return Err(StoreError::Conflict(
                                    "attempt has duplicate durable feedback events".into(),
                                ));
                            }
                        }
                        EventType::NodeOutputReceiptV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("NodeOutputReceipt@1 has no node ID".into())
                            })?;
                            let receipt: review_core::NodeOutputReceiptPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            if receipt.node != node {
                                return Err(StoreError::Conflict(
                                    "NodeOutputReceipt@1 metadata disagrees with its payload"
                                        .into(),
                                ));
                            }
                            if let Some(plan) = plan {
                                let reviewer = match plan.nodes.get(node) {
                                    Some(expected) => {
                                        validate_plan_ports(
                                            prepared,
                                            &expected.outputs,
                                            &receipt.outputs,
                                            subject_snapshot_id,
                                            subject_base_snapshot_id.as_deref(),
                                            subject_change_set_id.as_deref(),
                                        )?;
                                        expected.kind == NodeKindSpec::Reviewer
                                    }
                                    None => {
                                        let dynamic = dynamic_node_authority(
                                            tx, cas, run_id, active_id, plan, node,
                                        )?
                                        .ok_or_else(|| {
                                            StoreError::Conflict(format!(
                                                "node '{node}' is absent from the pinned Campaign plan and accepted Slice Sets"
                                            ))
                                        })?;
                                        validate_plan_ports(
                                            prepared,
                                            &dynamic.outputs,
                                            &receipt.outputs,
                                            subject_snapshot_id,
                                            subject_base_snapshot_id.as_deref(),
                                            subject_change_set_id.as_deref(),
                                        )?;
                                        true
                                    }
                                };
                                if reviewer && event.attempt_id.is_none() {
                                    return Err(StoreError::Conflict(
                                        "reviewer receipt has no selected attempt ID".into(),
                                    ));
                                }
                            }
                            let invocation: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'NodeInvocation@1' AND node_id = ?3",
                                params![run_id, active_id, node],
                                |row| row.get(0),
                            )?;
                            let receipts: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2
                                   AND type = 'NodeOutputReceipt@1' AND node_id = ?3",
                                params![run_id, active_id, node],
                                |row| row.get(0),
                            )?;
                            if invocation == 0 && !batch_invocations.contains(node) {
                                return Err(StoreError::Conflict(format!(
                                    "node '{node}' receipt has no durable invocation"
                                )));
                            }
                            if receipts > 0 || !batch_receipts.insert(node.to_string()) {
                                return Err(StoreError::Conflict(format!(
                                    "node '{node}' already has a durable output receipt"
                                )));
                            }
                            if let Some(attempt) = event.attempt_id.as_deref() {
                                let selected: Option<(String, String)> = tx
                                    .query_row(
                                        "SELECT node_id, payload FROM events
                                         WHERE run_id = ?1 AND causation_id = ?2
                                           AND attempt_id = ?3 AND type = 'AttemptAdmitted@1'
                                         LIMIT 1",
                                        params![run_id, active_id, attempt],
                                        |row| {
                                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                                        },
                                    )
                                    .optional()?
                                    .and_then(|(node, raw)| {
                                        serde_json::from_str::<
                                            review_core::event::AttemptAdmittedPayloadV1,
                                        >(&raw)
                                        .ok()
                                        .and_then(|payload| {
                                            (payload.selection == "selected")
                                                .then_some(payload.result_artifact)
                                                .flatten()
                                                .map(|result| (node, result))
                                        })
                                    })
                                    .or_else(|| batch_selected.get(attempt).cloned());
                                let Some((selected_node, result)) = selected else {
                                    return Err(StoreError::Conflict(
                                        "reviewer receipt has no selected admitted attempt".into(),
                                    ));
                                };
                                let outputs: Vec<&String> = receipt
                                    .outputs
                                    .iter()
                                    .flat_map(|port| &port.artifact_ids)
                                    .collect();
                                if selected_node != node
                                    || outputs.len() != 1
                                    || outputs[0] != &result
                                {
                                    return Err(StoreError::Conflict(
                                        "reviewer receipt contradicts its selected admitted result"
                                            .into(),
                                    ));
                                }
                            }
                        }
                        EventType::ProposalPreparedV1 | EventType::ProposalRefusedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(format!("{event_type} has no node ID"))
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict(format!("{event_type} has no Attempt ID"))
                            })?;
                            let selected_result = selected_attempt_result(
                                tx,
                                run_id,
                                active_id,
                                &batch_selected,
                                node,
                                attempt,
                            )?;
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events
                                 WHERE run_id = ?1 AND causation_id = ?2 AND attempt_id = ?3
                                   AND type IN ('ProposalPrepared@1', 'ProposalRefused@1')",
                                params![run_id, active_id, attempt],
                                |row| row.get(0),
                            )?;
                            if existing != 0 || !batch_proposal_attempts.insert(attempt.to_string())
                            {
                                return Err(StoreError::Conflict(
                                    "selected Attempt has duplicate Proposal disposition".into(),
                                ));
                            }
                            if event_type == EventType::ProposalPreparedV1 {
                                let payload: review_core::ProposalPreparedPayloadV1 =
                                    serde_json::from_value(event.payload.clone())?;
                                let candidate = proposal_candidate(
                                    cas,
                                    prepared,
                                    &payload.candidate_artifact_id,
                                )?;
                                if payload.result_artifact_id != selected_result
                                    || candidate.result_artifact_id != selected_result
                                    || candidate.base_snapshot_id != subject_snapshot_id.as_str()
                                {
                                    return Err(StoreError::Conflict(
                                        "ProposalPrepared@1 contradicts its selected Attempt or Subject"
                                            .into(),
                                    ));
                                }
                                validate_candidate_manifest(cas, subject, &candidate)?;
                                let mut expected = vec![
                                    payload.candidate_artifact_id.clone(),
                                    selected_result.clone(),
                                    candidate.patch_artifact_id.clone(),
                                    candidate.derived_manifest_artifact_id.clone(),
                                ];
                                expected.extend(candidate.evidence_ids.iter().cloned());
                                require_exact_round_artifact_refs(
                                    tx,
                                    run_id,
                                    active_payload,
                                    subject,
                                    event,
                                    expected,
                                    "ProposalPrepared@1",
                                )?;
                                batch_prepared_proposals.insert(
                                    payload.candidate_artifact_id,
                                    (node.to_string(), attempt.to_string(), selected_result),
                                );
                            } else {
                                let payload: review_core::ProposalRefusedPayloadV1 =
                                    serde_json::from_value(event.payload.clone())?;
                                if payload.result_artifact_id != selected_result {
                                    return Err(StoreError::Conflict(
                                        "ProposalRefused@1 contradicts its selected Attempt".into(),
                                    ));
                                }
                                require_exact_round_artifact_refs(
                                    tx,
                                    run_id,
                                    active_payload,
                                    subject,
                                    event,
                                    vec![selected_result],
                                    "ProposalRefused@1",
                                )?;
                            }
                        }
                        EventType::ProposalAcceptedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("ProposalAccepted@1 has no node ID".into())
                            })?;
                            let attempt = event.attempt_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("ProposalAccepted@1 has no Attempt ID".into())
                            })?;
                            let payload: review_core::ProposalAcceptedPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            payload
                                .validate()
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            let selected_result = selected_attempt_result(
                                tx,
                                run_id,
                                active_id,
                                &batch_selected,
                                node,
                                attempt,
                            )?;
                            let prepared_authority = prepared_proposal_authority(
                                tx,
                                run_id,
                                active_id,
                                &batch_prepared_proposals,
                                &payload.candidate_artifact_id,
                            )?;
                            if prepared_authority
                                != (
                                    node.to_string(),
                                    attempt.to_string(),
                                    selected_result.clone(),
                                )
                            {
                                return Err(StoreError::Conflict(
                                    "ProposalAccepted@1 contradicts its durable preparation".into(),
                                ));
                            }
                            let candidate =
                                proposal_candidate(cas, prepared, &payload.candidate_artifact_id)?;
                            validate_candidate_manifest(cas, subject, &candidate)?;
                            let envelope_value = prepared
                                .json
                                .get(&payload.proposal_artifact_id)
                                .cloned()
                                .map(Ok)
                                .unwrap_or_else(|| {
                                    cas.get_json(&payload.proposal_artifact_id)
                                        .map_err(|error| StoreError::Conflict(error.to_string()))
                                })?;
                            let envelope: review_core::ArtifactEnvelope =
                                serde_json::from_value(envelope_value)?;
                            crate::canonical::validate_envelope(&envelope)
                                .map_err(StoreError::Conflict)?;
                            if envelope.artifact_type != review_core::contract::PATCH_PROPOSAL_V1
                                || envelope.artifact_id != payload.proposal_id
                                || envelope.subject_snapshot_id.as_deref()
                                    != Some(subject_snapshot_id.as_str())
                            {
                                return Err(StoreError::Conflict(
                                    "ProposalAccepted@1 contradicts its typed Proposal envelope"
                                        .into(),
                                ));
                            }
                            match &envelope.producer {
                                review_core::Producer::Attempt {
                                    run_id: producer_run,
                                    node_id: producer_node,
                                    attempt_id: producer_attempt,
                                } if producer_run == run_id
                                    && producer_node == node
                                    && producer_attempt == attempt => {}
                                _ => {
                                    return Err(StoreError::Conflict(
                                        "accepted Proposal producer is not its selected Attempt"
                                            .into(),
                                    ));
                                }
                            }
                            let proposal: review_core::PatchProposal =
                                serde_json::from_value(envelope.payload.clone())?;
                            proposal
                                .check_shape()
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            validate_accepted_proposal_claims(
                                tx, run_id, active_id, node, &candidate, &proposal,
                            )?;
                            if proposal.base_snapshot_id != candidate.base_snapshot_id
                                || proposal.patch_artifact_id != candidate.patch_artifact_id
                                || proposal.evidence_ids != candidate.evidence_ids
                                || proposal.paths != candidate.paths
                                || proposal.description != candidate.description
                                || proposal.auto_apply_nominated != candidate.auto_apply_nominated
                            {
                                return Err(StoreError::Conflict(
                                    "accepted Proposal contradicts its sealed candidate".into(),
                                ));
                            }
                            let mut expected_inputs = vec![
                                payload.candidate_artifact_id.clone(),
                                candidate.result_artifact_id.clone(),
                                candidate.patch_artifact_id.clone(),
                                candidate.derived_manifest_artifact_id.clone(),
                            ];
                            expected_inputs.extend(candidate.evidence_ids.iter().cloned());
                            if normalized_ids(envelope.input_artifacts.clone())
                                != normalized_ids(expected_inputs)
                            {
                                return Err(StoreError::Conflict(
                                    "accepted Proposal envelope omits exact candidate inputs"
                                        .into(),
                                ));
                            }
                            require_exact_round_artifact_refs(
                                tx,
                                run_id,
                                active_payload,
                                subject,
                                event,
                                vec![
                                    payload.proposal_artifact_id.clone(),
                                    payload.candidate_artifact_id.clone(),
                                ],
                                "ProposalAccepted@1",
                            )?;
                            let existing: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM events WHERE run_id = ?1
                                 AND causation_id = ?2 AND type = 'ProposalAccepted@1'
                                 AND (json_extract(payload, '$.proposal_id') = ?3
                                   OR json_extract(payload, '$.candidate_artifact_id') = ?4)",
                                params![
                                    run_id,
                                    active_id,
                                    payload.proposal_id,
                                    payload.candidate_artifact_id
                                ],
                                |row| row.get(0),
                            )?;
                            if existing != 0
                                || !batch_accepted_proposals.insert(payload.proposal_id)
                            {
                                return Err(StoreError::Conflict(
                                    "Proposal was accepted more than once".into(),
                                ));
                            }
                        }
                        EventType::FindingReportedV1 => {
                            let key = event.payload["key"].as_str().ok_or_else(|| {
                                StoreError::Conflict("FindingReported@1 has no finding key".into())
                            })?;
                            if event.correlation_id.as_deref() != Some(key) {
                                return Err(StoreError::Conflict(
                                    "FindingReported@1 correlation disagrees with its key".into(),
                                ));
                            }
                            match event.payload.get("report_id").and_then(Value::as_str) {
                                Some(report_id)
                                    if event.artifact_refs.as_slice() == [report_id] => {}
                                Some(_) => {
                                    return Err(StoreError::Conflict(
                                        "FindingReported@1 report ID disagrees with its artifact reference"
                                            .into(),
                                    ));
                                }
                                None if event.payload.get("imported").and_then(Value::as_bool)
                                    == Some(true)
                                    && event.artifact_refs.is_empty() => {}
                                None => {
                                    return Err(StoreError::Conflict(
                                        "FindingReported@1 has no authoritative report artifact"
                                            .into(),
                                    ));
                                }
                            }
                            batch_findings.insert(key.to_string());
                        }
                        EventType::DemandRecordedV1 => {
                            let demand: review_core::DemandV1 = validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::DEMAND_V1,
                            )?;
                            if event.correlation_id.as_deref() != Some(demand.demand_id.as_str())
                                || demand.round != active_payload.round
                                || demand.subject_id != active_payload.subject_id
                            {
                                return Err(StoreError::Conflict(
                                    "DemandRecorded@1 is not bound to its Demand, Round, and Subject"
                                        .into(),
                                ));
                            }
                            batch_demands.insert(demand.demand_id);
                        }
                        EventType::SliceSetAcceptedV1 => {
                            let node = event.node_id.as_deref().ok_or_else(|| {
                                StoreError::Conflict("SliceSetAccepted@1 has no Slicer node".into())
                            })?;
                            let payload: review_core::SliceSetAcceptedPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            payload
                                .validate()
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            if payload.slice_set_id != payload.slice_set_artifact_id
                                || !event.artifact_refs.contains(&payload.slice_set_artifact_id)
                                || plan
                                    .and_then(|plan| plan.nodes.get(node))
                                    .is_none_or(|node| node.kind != NodeKindSpec::Slicer)
                            {
                                return Err(StoreError::Conflict(
                                    "SliceSetAccepted@1 contradicts pinned Slicer authority".into(),
                                ));
                            }
                            let value = prepared
                                .json
                                .get(&payload.slice_set_artifact_id)
                                .ok_or_else(|| {
                                    StoreError::Conflict(
                                        "SliceSetAccepted@1 artifact was not prepared".into(),
                                    )
                                })?;
                            let set: review_core::SliceSetV1 = validated_envelope_payload(
                                value,
                                review_core::contract::SLICE_SET_V1,
                            )?;
                            set.validate().map_err(StoreError::Conflict)?;
                            let plan = plan.ok_or_else(|| {
                                StoreError::Conflict(
                                    "SliceSetAccepted@1 has no captured pipeline authority".into(),
                                )
                            })?;
                            let slicer = plan.nodes.get(node).ok_or_else(|| {
                                StoreError::Conflict(
                                    "SliceSetAccepted@1 Slicer is absent from the captured plan"
                                        .into(),
                                )
                            })?;
                            let policy = slicer.slicing.as_ref().ok_or_else(|| {
                                StoreError::Conflict(
                                    "SliceSetAccepted@1 Slicer has no captured slicing policy"
                                        .into(),
                                )
                            })?;
                            let expected = expected_slice_set(
                                cas,
                                plan,
                                policy,
                                &active_payload.subject_id,
                                subject,
                            )?;
                            if set.subject_id != active_payload.subject_id || set != expected {
                                return Err(StoreError::Conflict(
                                    "SliceSetAccepted@1 contradicts the exact captured slicing policy"
                                        .into(),
                                ));
                            }
                        }
                        EventType::ShardSetRecordedV1 | EventType::SemanticClosureCheckedV1 => {
                            let payload: review_core::RecordedSetPayloadV1 =
                                serde_json::from_value(event.payload.clone())?;
                            payload
                                .validate()
                                .map_err(|error| StoreError::Conflict(error.to_string()))?;
                            if payload.artifact_id != payload.record_id
                                || !event.artifact_refs.contains(&payload.record_id)
                            {
                                return Err(StoreError::Conflict(format!(
                                    "{} contradicts its recorded artifact",
                                    event.event_type
                                )));
                            }
                            let value = prepared.json.get(&payload.record_id).ok_or_else(|| {
                                StoreError::Conflict(format!(
                                    "{} artifact was not prepared",
                                    event.event_type
                                ))
                            })?;
                            if event.event_type == EventType::ShardSetRecordedV1 {
                                let set: review_core::ShardSetV1 = validated_envelope_payload(
                                    value,
                                    review_core::contract::SHARD_SET_V1,
                                )?;
                                set.validate_shape().map_err(StoreError::Conflict)?;
                                if set.subject_id != active_payload.subject_id {
                                    return Err(StoreError::Conflict(
                                        "ShardSetRecorded@1 belongs to another Subject".into(),
                                    ));
                                }
                            } else {
                                let closure: review_core::SemanticClosureV1 =
                                    validated_envelope_payload(
                                        value,
                                        review_core::contract::SEMANTIC_CLOSURE_V1,
                                    )?;
                                closure.validate().map_err(StoreError::Conflict)?;
                                if closure.subject_id != active_payload.subject_id {
                                    return Err(StoreError::Conflict(
                                        "SemanticClosureChecked@1 belongs to another Subject"
                                            .into(),
                                    ));
                                }
                            }
                        }
                        _ => {}
                    }
                    if event_type.is_run_report() {
                        let closes = report_closes(event_type, &event.payload)?;
                        // RunReport@5 is the first incomplete report that carries new execution
                        // authority. Validate its plan, binding, cache, and receipt claims even
                        // when it keeps the Round open; frozen report versions retain their
                        // existing closing-report admission behavior.
                        if event_type.run_report_requires_receipts()
                            && (closes || event_type == EventType::RunReportV5)
                        {
                            if let Some(plan) = plan {
                                validate_report_plan(plan, event_type, &event.payload)?;
                            }
                            if matches!(event_type, EventType::RunReportV4 | EventType::RunReportV5)
                            {
                                validate_report_gate_bindings(
                                    tx,
                                    run_id,
                                    active_id,
                                    event_type,
                                    &event.payload,
                                )?;
                            }
                            if event_type == EventType::RunReportV5 {
                                validate_report_cache_snapshots(
                                    tx,
                                    run_id,
                                    active_id,
                                    &event.payload,
                                    &event.artifact_refs,
                                    prepared,
                                )?;
                            }
                            validate_report_receipts(
                                tx,
                                cas,
                                run_id,
                                active_id,
                                event_type,
                                &event.payload,
                            )?;
                        }
                        if closes {
                            if terminal {
                                return Err(StoreError::Conflict(
                                    "the active Round epoch already has a terminal conclusion"
                                        .into(),
                                ));
                            }
                            terminal = true;
                        }
                    }
                }
            }
            EventType::FindingResolvedV1 => {
                let key = event.payload["key"].as_str().ok_or_else(|| {
                    StoreError::Conflict("FindingResolved@1 has no finding key".into())
                })?;
                if event.correlation_id.as_deref() != Some(key) {
                    return Err(StoreError::Conflict(
                        "FindingResolved@1 correlation disagrees with its key".into(),
                    ));
                }
                let existing: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events
                     WHERE run_id = ?1 AND type = 'FindingReported@1' AND correlation_id = ?2",
                    params![run_id, key],
                    |row| row.get(0),
                )?;
                if existing == 0 && !batch_findings.contains(key) {
                    return Err(StoreError::Conflict(format!(
                        "FindingResolved@1 names unknown finding key '{key}'"
                    )));
                }
                if let (Some(causation), Some((active_id, _))) =
                    (event.causation_id.as_deref(), &active)
                    && causation != active_id
                {
                    return Err(StoreError::Conflict(
                        "FindingResolved@1 is bound to a stale Round epoch".into(),
                    ));
                }
            }
            EventType::FindingsGroupedV1 | EventType::FindingsUngroupedV1 => {
                let Some((_, active_payload)) = &active else {
                    return Err(StoreError::Conflict(format!(
                        "{} requires an existing Campaign Round",
                        event.event_type
                    )));
                };
                if !terminal || event.causation_id.is_some() {
                    return Err(StoreError::Conflict(format!(
                        "{} is an operator transition allowed only after a closed Round",
                        event.event_type
                    )));
                }
                let payload: review_core::FindingGroupingEventPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                payload.validate().map_err(StoreError::Conflict)?;
                if event.correlation_id.as_deref() != Some(payload.from.as_str())
                    || event.artifact_refs.as_slice() != [payload.grouping_artifact_id.as_str()]
                {
                    return Err(StoreError::Conflict(format!(
                        "{} metadata disagrees with its grouping payload",
                        event.event_type
                    )));
                }
                let action = if event.event_type == EventType::FindingsGroupedV1 {
                    review_core::FindingGroupingAction::Group
                } else {
                    review_core::FindingGroupingAction::Ungroup
                };
                validate_grouping_event_artifact(prepared, &payload, action, active_payload.round)?;
                for key in [&payload.from, &payload.into] {
                    let existing: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM events
                         WHERE run_id = ?1 AND type = 'FindingReported@1' AND correlation_id = ?2",
                        params![run_id, key],
                        |row| row.get(0),
                    )?;
                    if existing == 0 && !batch_findings.contains(key) {
                        return Err(StoreError::Conflict(format!(
                            "{} names unknown Finding `{key}`",
                            event.event_type
                        )));
                    }
                }
                match action {
                    review_core::FindingGroupingAction::Group => {
                        if payload.from == payload.into
                            || active_groupings.contains_key(&payload.from)
                            || grouping_root(&active_groupings, &payload.into)
                                != Some(payload.into.as_str())
                        {
                            return Err(StoreError::Conflict(
                                "FindingsGrouped@1 would create an ambiguous or cyclic grouping"
                                    .into(),
                            ));
                        }
                        active_groupings.insert(payload.from, payload.into);
                    }
                    review_core::FindingGroupingAction::Ungroup => {
                        if active_groupings.get(&payload.from) != Some(&payload.into) {
                            return Err(StoreError::Conflict(
                                "FindingsUngrouped@1 has no matching active grouping".into(),
                            ));
                        }
                        active_groupings.remove(&payload.from);
                    }
                }
            }
            EventType::IntegrationPreparedV1
            | EventType::IntegrationConflictV1
            | EventType::IntegrationChecksCompletedV1
            | EventType::IntegrationCommittedV1 => {
                let Some((_, active_payload)) = &active else {
                    return Err(StoreError::Conflict(format!(
                        "{} requires an existing Campaign Round",
                        event.event_type
                    )));
                };
                if !terminal || event.causation_id.is_some() {
                    return Err(StoreError::Conflict(format!(
                        "{} is an internal transition allowed only after a closed Round",
                        event.event_type
                    )));
                }
                let integration_authority = authority_plan
                    .as_ref()
                    .filter(|plan| plan.version == 5 && plan.integration.is_some())
                    .ok_or_else(|| {
                        StoreError::Conflict(format!(
                            "{} has no captured automatic-Integration authority",
                            event.event_type
                        ))
                    })?;
                match event.event_type {
                    EventType::IntegrationPreparedV1 => {
                        let payload: review_core::IntegrationPreparedPayloadV1 =
                            serde_json::from_value(event.payload.clone())?;
                        let plan = validate_artifact_payload(
                            prepared,
                            review_core::contract::INTEGRATION_PLAN_V1,
                            &payload.plan_artifact_id,
                        )?;
                        if plan.is_some() {
                            unreachable!("IntegrationPlan validation never returns a Change Set")
                        }
                        let plan: review_core::IntegrationPlanV1 = serde_json::from_value(
                            prepared
                                .json
                                .get(&payload.plan_artifact_id)
                                .expect("validated IntegrationPlan")
                                .clone(),
                        )?;
                        if plan.subject_id != active_payload.subject_id
                            || event.correlation_id.as_deref()
                                != Some(active_payload.subject_id.as_str())
                        {
                            return Err(StoreError::Conflict(
                                "IntegrationPrepared@1 contradicts the active Subject or its references"
                                    .into(),
                            ));
                        }
                        let source: review_core::SourceSnapshot = serde_json::from_value(
                            cas.get_json(&payload.derived_snapshot_id)
                                .map_err(|error| {
                                    StoreError::Conflict(format!(
                                        "prepared derived Snapshot was not readable: {error}"
                                    ))
                                })?,
                        )?;
                        if !source.is_derived()
                            || source.parent_snapshot_id.as_deref()
                                != Some(plan.base_snapshot_id.as_str())
                            || source.artifact_manifest.as_deref()
                                != Some(plan.derived_manifest_artifact_id.as_str())
                        {
                            return Err(StoreError::Conflict(
                                "IntegrationPrepared@1 derived Snapshot contradicts its plan"
                                    .into(),
                            ));
                        }
                        validate_integration_plan_authority(
                            tx,
                            cas,
                            run_id,
                            active_payload,
                            integration_authority,
                            &plan,
                            IntegrationTarget {
                                batch_id: &payload.batch_id,
                                derived_snapshot_id: &payload.derived_snapshot_id,
                            },
                        )?;
                        require_exact_artifact_refs(
                            event,
                            vec![
                                payload.plan_artifact_id.clone(),
                                payload.derived_snapshot_id.clone(),
                                plan.derived_manifest_artifact_id.clone(),
                            ],
                            "IntegrationPrepared@1",
                        )?;
                        let duplicate: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationPrepared@1'
                             AND (json_extract(payload, '$.batch_id') = ?2
                               OR json_extract(payload, '$.plan_artifact_id') = ?3)",
                            params![run_id, payload.batch_id, payload.plan_artifact_id],
                            |row| row.get(0),
                        )?;
                        if duplicate != 0 {
                            return Err(StoreError::Conflict(
                                "Integration preparation is duplicated".into(),
                            ));
                        }
                    }
                    EventType::IntegrationConflictV1 => {
                        let payload: review_core::IntegrationConflictPayloadV1 =
                            serde_json::from_value(event.payload.clone())?;
                        payload.validate().map_err(StoreError::Conflict)?;
                        let subject: review_core::SubjectV1 = serde_json::from_value(
                            cas.get_json(&active_payload.subject_id)
                                .map_err(|error| StoreError::Conflict(error.to_string()))?,
                        )?;
                        if payload.base_snapshot_id != subject.head_snapshot_id {
                            return Err(StoreError::Conflict(
                                "IntegrationConflict@1 is not bound to the active head".into(),
                            ));
                        }
                    }
                    EventType::IntegrationChecksCompletedV1 => {
                        let payload: review_core::IntegrationChecksCompletedPayloadV1 =
                            serde_json::from_value(event.payload.clone())?;
                        validate_artifact_payload(
                            prepared,
                            review_core::contract::INTEGRATION_CHECKS_V1,
                            &payload.checks_artifact_id,
                        )?;
                        let checks: review_core::IntegrationChecksV1 = serde_json::from_value(
                            prepared
                                .json
                                .get(&payload.checks_artifact_id)
                                .expect("validated IntegrationChecks")
                                .clone(),
                        )?;
                        if checks.passed() != payload.passed {
                            return Err(StoreError::Conflict(
                                "IntegrationChecksCompleted@1 contradicts its exact results".into(),
                            ));
                        }
                        let prepared_raw: String = tx.query_row(
                            "SELECT payload FROM events WHERE run_id = ?1
                             AND type = 'IntegrationPrepared@1'
                             AND json_extract(payload, '$.batch_id') = ?2",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        let integration_prepared: review_core::IntegrationPreparedPayloadV1 =
                            serde_json::from_str(&prepared_raw)?;
                        let policy = integration_authority
                            .integration
                            .as_ref()
                            .expect("checked Integration authority");
                        let check_names = checks
                            .checks
                            .iter()
                            .map(|check| check.name.as_str())
                            .collect::<Vec<_>>();
                        let expected_names = policy
                            .post_apply_checks
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>();
                        if checks.derived_snapshot_id != integration_prepared.derived_snapshot_id
                            || check_names != expected_names
                            || expected_names.iter().any(|name| {
                                !integration_authority
                                    .check_names
                                    .iter()
                                    .any(|check| check == name)
                            })
                        {
                            return Err(StoreError::Conflict(
                                "Integration checks contradict the prepared Snapshot or captured check policy"
                                    .into(),
                            ));
                        }
                        let mut expected_refs = vec![
                            payload.checks_artifact_id.clone(),
                            integration_prepared.derived_snapshot_id,
                        ];
                        expected_refs.extend(
                            checks
                                .checks
                                .iter()
                                .map(|check| check.result_artifact_id.clone()),
                        );
                        require_exact_artifact_refs(
                            event,
                            expected_refs,
                            "IntegrationChecksCompleted@1",
                        )?;
                        let prepared_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationPrepared@1'
                             AND json_extract(payload, '$.batch_id') = ?2",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        if prepared_count != 1 {
                            return Err(StoreError::Conflict(
                                "Integration checks have no unique durable preparation".into(),
                            ));
                        }
                    }
                    EventType::IntegrationCommittedV1 => {
                        let payload: review_core::IntegrationCommittedPayloadV1 =
                            serde_json::from_value(event.payload.clone())?;
                        if payload.prior_subject_id != active_payload.subject_id {
                            return Err(StoreError::Conflict(
                                "IntegrationCommitted@1 expected Subject is stale".into(),
                            ));
                        }
                        let prepared_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationPrepared@1'
                             AND json_extract(payload, '$.batch_id') = ?2",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        let passed_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationChecksCompleted@1'
                             AND json_extract(payload, '$.batch_id') = ?2
                             AND json_extract(payload, '$.passed') = 1",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        let committed_count: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM events WHERE run_id = ?1
                             AND type = 'IntegrationCommitted@1'
                             AND json_extract(payload, '$.batch_id') = ?2",
                            params![run_id, payload.batch_id],
                            |row| row.get(0),
                        )?;
                        if prepared_count != 1 || passed_count != 1 || committed_count != 0 {
                            return Err(StoreError::Conflict(
                                "Integration commit lacks unique preparation and passing checks, or is duplicated"
                                    .into(),
                            ));
                        }
                        validate_integration_commit_authority(
                            tx,
                            cas,
                            run_id,
                            active_payload,
                            &payload,
                            &batch_integration_attestations,
                            integration_authority,
                        )?;
                        let subject: review_core::SubjectV1 = serde_json::from_value(
                            cas.get_json(&payload.derived_subject_id).map_err(|error| {
                                StoreError::Conflict(format!(
                                    "derived Subject was not readable at commit: {error}"
                                ))
                            })?,
                        )?;
                        subject.validate().map_err(StoreError::Conflict)?;
                        if subject.head_snapshot_id != payload.derived_snapshot_id
                            || !event.artifact_refs.contains(&payload.derived_subject_id)
                            || !event.artifact_refs.contains(&payload.derived_snapshot_id)
                            || !event.artifact_refs.contains(&payload.semantic_closure_id)
                            || payload
                                .attestation_ids
                                .iter()
                                .any(|id| !event.artifact_refs.contains(id))
                        {
                            return Err(StoreError::Conflict(
                                "IntegrationCommitted@1 does not expose its complete atomic authority"
                                    .into(),
                            ));
                        }
                    }
                    _ => unreachable!(),
                }
            }
            EventType::EvidenceAddedV1
            | EventType::EvidenceReuseAdmittedV1
            | EventType::EvidenceSatisfiedV1
            | EventType::DemandWaivedV1 => {
                let Some((_, active_payload)) = &active else {
                    return Err(StoreError::Conflict(format!(
                        "{} requires an existing Campaign Round",
                        event.event_type
                    )));
                };
                if !terminal || event.causation_id.is_some() {
                    return Err(StoreError::Conflict(format!(
                        "{} is an operator transition allowed only after a closed Round",
                        event.event_type
                    )));
                }
                let (demand_id, subject_id) = match event.event_type {
                    EventType::EvidenceAddedV1 => {
                        let value: review_core::EvidenceV1 = validate_recorded_event_artifact(
                            prepared,
                            event,
                            review_core::contract::EVIDENCE_V1,
                        )?;
                        (value.demand_id, value.subject_id)
                    }
                    EventType::EvidenceSatisfiedV1 => {
                        let value: review_core::EvidenceSatisfactionV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::EVIDENCE_SATISFACTION_V1,
                            )?;
                        (value.demand_id, value.subject_id)
                    }
                    EventType::EvidenceReuseAdmittedV1 => {
                        let value: review_core::EvidenceReuseAdmissionV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::EVIDENCE_REUSE_ADMISSION_V1,
                            )?;
                        (value.demand_id, value.subject_id)
                    }
                    EventType::DemandWaivedV1 => {
                        let value: review_core::DemandWaiverV1 = validate_recorded_event_artifact(
                            prepared,
                            event,
                            review_core::contract::DEMAND_WAIVER_V1,
                        )?;
                        (value.demand_id, value.subject_id)
                    }
                    _ => unreachable!(),
                };
                if event.correlation_id.as_deref() != Some(demand_id.as_str())
                    || subject_id != active_payload.subject_id
                {
                    return Err(StoreError::Conflict(format!(
                        "{} is not bound to its Demand and active Subject",
                        event.event_type
                    )));
                }
                let existing: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events
                     WHERE run_id = ?1 AND type = 'DemandRecorded@1' AND correlation_id = ?2",
                    params![run_id, demand_id],
                    |row| row.get(0),
                )?;
                if existing == 0 && !batch_demands.contains(&demand_id) {
                    return Err(StoreError::Conflict(format!(
                        "{} names unknown Demand `{demand_id}`",
                        event.event_type
                    )));
                }
            }
            EventType::ChangeAttestedV1
            | EventType::FixVerifiedV1
            | EventType::FindingResolutionRecordedV1
            | EventType::FindingResolutionChallengedV1
            | EventType::PolicyTimeAdvancedV1 => {
                let Some((active_id, active_payload)) = &active else {
                    return Err(StoreError::Conflict(format!(
                        "{} requires an existing Campaign Round",
                        event.event_type
                    )));
                };
                let automatic_challenge = if event.event_type
                    == EventType::FindingResolutionChallengedV1
                    && event.causation_id.as_deref() == Some(active_id.as_str())
                {
                    let challenge: review_core::ResolutionChallengeV1 =
                        validate_recorded_event_artifact(
                            prepared,
                            event,
                            review_core::contract::RESOLUTION_CHALLENGE_V1,
                        )?;
                    let recorded: review_core::RecordedArtifactPayloadV1 =
                        serde_json::from_value(event.payload.clone())?;
                    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                        prepared
                            .json
                            .get(&recorded.artifact_id)
                            .expect("validated recorded artifact")
                            .clone(),
                    )?;
                    challenge.actor == "review.kernel/resolution-policy@1"
                        && !challenge.evidence_ids.is_empty()
                        && matches!(
                            envelope.producer,
                            review_core::Producer::KernelOperation {
                                run_id: producer_run,
                                node_id: None,
                                operation_id,
                            } if producer_run == run_id
                                && operation_id.starts_with("automatic-resolution-challenge:")
                        )
                } else {
                    false
                };
                if !automatic_challenge && (!terminal || event.causation_id.is_some()) {
                    return Err(StoreError::Conflict(format!(
                        "{} is an operator transition allowed only after a closed Round",
                        event.event_type
                    )));
                }
                let (correlation, subject_id) = match event.event_type {
                    EventType::ChangeAttestedV1 => {
                        let value: review_core::ChangeAttestationV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::CHANGE_ATTESTATION_V1,
                            )?;
                        (value.finding_id, Some(value.subject_id))
                    }
                    EventType::FixVerifiedV1 => {
                        let value: review_core::FixVerificationV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::FIX_VERIFICATION_V1,
                            )?;
                        (value.finding_id, Some(value.subject_id))
                    }
                    EventType::FindingResolutionRecordedV1 => {
                        let value: review_core::FindingResolutionV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::FINDING_RESOLUTION_V1,
                            )?;
                        (value.finding_id, Some(value.subject_id))
                    }
                    EventType::FindingResolutionChallengedV1 => {
                        let value: review_core::ResolutionChallengeV1 =
                            validate_recorded_event_artifact(
                                prepared,
                                event,
                                review_core::contract::RESOLUTION_CHALLENGE_V1,
                            )?;
                        (value.finding_id, Some(value.subject_id))
                    }
                    EventType::PolicyTimeAdvancedV1 => {
                        let _: review_core::PolicyTimeV1 = validate_recorded_event_artifact(
                            prepared,
                            event,
                            review_core::contract::POLICY_TIME_V1,
                        )?;
                        ("policy-time".into(), None)
                    }
                    _ => unreachable!(),
                };
                if event.correlation_id.as_deref() != Some(correlation.as_str())
                    || subject_id
                        .as_deref()
                        .is_some_and(|subject| subject != active_payload.subject_id)
                {
                    return Err(StoreError::Conflict(format!(
                        "{} is not bound to its Finding and active Subject",
                        event.event_type
                    )));
                }
                if event.event_type != EventType::PolicyTimeAdvancedV1 {
                    let existing: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM events
                         WHERE run_id = ?1 AND type = 'FindingReported@1' AND correlation_id = ?2",
                        params![run_id, correlation],
                        |row| row.get(0),
                    )?;
                    if existing == 0 && !batch_findings.contains(&correlation) {
                        return Err(StoreError::Conflict(format!(
                            "{} names unknown Finding `{correlation}`",
                            event.event_type
                        )));
                    }
                }
            }
            _ => {}
        }
    }
    if pending_supersession.is_some() {
        return Err(StoreError::Conflict(
            "RoundInputSuperseded@1 and its replacement RoundStarted@1 must append atomically"
                .into(),
        ));
    }
    for (attempt, node) in batch_attempt_inputs {
        if batch_dispatches.get(&attempt) != Some(&node) {
            return Err(StoreError::Conflict(
                "AttemptInput@1 must append atomically with its matching dispatch".into(),
            ));
        }
    }
    for (attempt, node) in batch_attempt_feedback {
        if batch_terminal_nodes.get(&attempt) != Some(&node)
            || !matches!(
                batch_terminals.get(&attempt),
                Some(EventType::AttemptFailedV1 | EventType::AttemptFencedV1)
            )
        {
            return Err(StoreError::Conflict(
                "AttemptFeedback@1 must append atomically with its matching failed or fenced attempt"
                    .into(),
            ));
        }
    }
    Ok(())
}

fn load_active_groupings(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
    let mut groupings = std::collections::BTreeMap::new();
    let mut statement = tx.prepare(
        "SELECT type, payload FROM events
         WHERE run_id = ?1 AND type IN ('FindingsGrouped@1', 'FindingsUngrouped@1')
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (event_type, raw) = row?;
        let payload: review_core::FindingGroupingEventPayloadV1 = serde_json::from_str(&raw)?;
        if event_type == EventType::FindingsGroupedV1.as_str() {
            groupings.insert(payload.from, payload.into);
        } else {
            groupings.remove(&payload.from);
        }
    }
    Ok(groupings)
}

fn grouping_root<'a>(
    groupings: &'a std::collections::BTreeMap<String, String>,
    key: &'a str,
) -> Option<&'a str> {
    let mut root = key;
    for _ in 0..=groupings.len() {
        let Some(next) = groupings.get(root) else {
            return Some(root);
        };
        root = next;
    }
    None
}

fn validate_grouping_event_artifact(
    prepared: &PreparedArtifacts,
    payload: &review_core::FindingGroupingEventPayloadV1,
    action: review_core::FindingGroupingAction,
    round: u32,
) -> Result<(), StoreError> {
    let value = prepared
        .json
        .get(&payload.grouping_artifact_id)
        .ok_or_else(|| StoreError::Conflict("grouping artifact was not prepared".into()))?;
    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())?;
    crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
    if envelope.artifact_type != review_core::contract::FINDING_GROUPING_V1 {
        return Err(StoreError::Conflict(
            "grouping event references the wrong artifact type".into(),
        ));
    }
    let grouping: review_core::FindingGroupingV1 = serde_json::from_value(envelope.payload)?;
    grouping.validate().map_err(StoreError::Conflict)?;
    if grouping.from != payload.from
        || grouping.into != payload.into
        || grouping.action != action
        || grouping.round != round
    {
        return Err(StoreError::Conflict(
            "grouping event contradicts its immutable artifact".into(),
        ));
    }
    Ok(())
}

fn validate_recorded_event_artifact<T: serde::de::DeserializeOwned>(
    prepared: &PreparedArtifacts,
    event: &NewEvent,
    expected_type: &str,
) -> Result<T, StoreError> {
    let payload: review_core::RecordedArtifactPayloadV1 =
        serde_json::from_value(event.payload.clone())?;
    payload.validate().map_err(StoreError::Conflict)?;
    if event.artifact_refs.as_slice() != [payload.artifact_id.as_str()] {
        return Err(StoreError::Conflict(format!(
            "{} recorded artifact disagrees with its sole reference",
            event.event_type
        )));
    }
    let value = prepared.json.get(&payload.artifact_id).ok_or_else(|| {
        StoreError::Conflict(format!("{} artifact was not prepared", event.event_type))
    })?;
    validated_envelope_payload(value, expected_type)
}

fn exact_subject_paths(
    cas: &Cas,
    subject: &review_core::SubjectV1,
) -> Result<Vec<String>, StoreError> {
    match subject.kind {
        review_core::SubjectKind::Diff => {
            let change_set_id = subject.change_set_id.as_deref().ok_or_else(|| {
                StoreError::Conflict("diff Subject has no ChangeSet authority".into())
            })?;
            let change_set: review_core::ChangeSetV1 = serde_json::from_value(
                cas.get_json(change_set_id)
                    .map_err(|error| StoreError::Conflict(error.to_string()))?,
            )?;
            change_set.validate().map_err(StoreError::Conflict)?;
            if change_set.head_snapshot_id != subject.head_snapshot_id
                || Some(change_set.base_snapshot_id.as_str()) != subject.base_snapshot_id.as_deref()
            {
                return Err(StoreError::Conflict(
                    "Subject path authority contradicts its ChangeSet".into(),
                ));
            }
            Ok(change_set.changed_paths)
        }
        review_core::SubjectKind::WholeTree => {
            let snapshot: review_core::SourceSnapshot = serde_json::from_value(
                cas.get_json(&subject.head_snapshot_id)
                    .map_err(|error| StoreError::Conflict(error.to_string()))?,
            )?;
            let manifest_id = snapshot.artifact_manifest.as_deref().ok_or_else(|| {
                StoreError::Conflict("whole-tree Subject Snapshot has no Manifest".into())
            })?;
            let manifest = cas
                .get_json(manifest_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))?;
            Ok(manifest_entries(&manifest)?.into_keys().collect())
        }
    }
}

fn expected_slice_set(
    cas: &Cas,
    plan: &AuthorityPlan,
    policy: &SlicingSpec,
    subject_id: &str,
    subject: &review_core::SubjectV1,
) -> Result<review_core::SliceSetV1, StoreError> {
    // Non-zero bounds and a consistent closeout are the shape's rules; a plan only exists here
    // after they held.
    let paths = exact_subject_paths(cas, subject)?;
    if paths.is_empty()
        || paths.len().div_ceil(policy.max_paths_per_slice) > policy.max_fanout as usize
    {
        return Err(StoreError::Conflict(
            "captured slicing policy cannot cover the exact Subject paths".into(),
        ));
    }
    let closeout = policy.closeout_policy().map_err(|error| {
        StoreError::Conflict(format!("captured closeout policy is invalid: {error}"))
    })?;
    let slices = paths
        .chunks(policy.max_paths_per_slice)
        .enumerate()
        .map(|(ordinal, paths)| {
            let slice_id = crate::canonical::content_id(&serde_json::json!({
                "domain": "review.kernel/slice-id@1",
                "subject_id": subject_id,
                "paths": paths,
            }))
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
            let runtime_node_id = format!(
                "{}#slice:{}:{}",
                policy.scatter,
                ordinal + 1,
                &slice_id[7..23]
            );
            if plan.nodes.contains_key(&runtime_node_id) {
                return Err(StoreError::Conflict(
                    "captured Slice runtime identity collides with a static node".into(),
                ));
            }
            Ok(review_core::ReviewSliceV1 {
                slice_id,
                runtime_node_id,
                paths: paths.to_vec(),
                overlaps: vec![],
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let set = review_core::SliceSetV1 {
        subject_id: subject_id.to_string(),
        coverage: policy.coverage,
        max_fanout: policy.max_fanout,
        all_shards_required: policy.all_shards_required,
        closeout,
        slices,
    };
    set.validate_coverage(&paths)
        .map_err(StoreError::Conflict)?;
    Ok(set)
}

fn normalized_ids(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

fn require_exact_artifact_refs(
    event: &NewEvent,
    expected: Vec<String>,
    label: &str,
) -> Result<(), StoreError> {
    if normalized_ids(event.artifact_refs.clone()) != normalized_ids(expected) {
        return Err(StoreError::Conflict(format!(
            "{label} does not reference its exact authority"
        )));
    }
    Ok(())
}

fn require_exact_round_artifact_refs(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    active: &review_core::RoundStartedPayloadV1,
    subject: &review_core::SubjectV1,
    event: &NewEvent,
    mut expected: Vec<String>,
    label: &str,
) -> Result<(), StoreError> {
    let opened_raw: String = tx.query_row(
        "SELECT payload FROM events WHERE run_id = ?1 AND type = 'CampaignOpened@1'
         ORDER BY sequence LIMIT 1",
        params![run_id],
        |row| row.get(0),
    )?;
    let opened: review_core::CampaignOpenedPayloadV1 = serde_json::from_str(&opened_raw)?;
    expected.extend([
        opened.authority_snapshot_id,
        active.campaign_manifest_id.clone(),
        active.subject_id.clone(),
        subject.head_snapshot_id.clone(),
    ]);
    expected.sort();
    expected.dedup();
    require_exact_artifact_refs(event, expected, label)
}

fn round_runtime_event(event_type: EventType) -> bool {
    event_type.is_run_report()
        || matches!(
            event_type,
            EventType::BrokerOperationCompletedV1
                | EventType::ReviewerExecutionBoundV1
                | EventType::AttemptAdmittedV1
                | EventType::AttemptDispatchedV1
                | EventType::AttemptFeedbackV1
                | EventType::AttemptInputV1
                | EventType::AttemptFailedV1
                | EventType::AttemptFencedV1
                | EventType::AttemptReleasedV1
                | EventType::CheckCompletedV1
                | EventType::DemandRecordedV1
                | EventType::FindingReportedV1
                | EventType::GateDecisionV1
                | EventType::GateExecutionBoundV1
                | EventType::CacheSnapshotMaterializedV1
                | EventType::GenerationAdvancedV1
                | EventType::NodeInvocationV1
                | EventType::NodeOutputReceiptV1
                | EventType::ProviderOperationTransitionV1
                | EventType::ProposalPreparedV1
                | EventType::ProposalRefusedV1
                | EventType::ProposalAcceptedV1
                | EventType::SliceSetAcceptedV1
                | EventType::ShardSetRecordedV1
                | EventType::SemanticClosureCheckedV1
        )
}

fn event_uses_authority_plan(event_type: EventType) -> bool {
    event_type.is_run_report()
        || matches!(
            event_type,
            EventType::BrokerOperationCompletedV1
                | EventType::ReviewerExecutionBoundV1
                | EventType::AttemptAdmittedV1
                | EventType::AttemptFailedV1
                | EventType::AttemptFencedV1
                | EventType::AttemptReleasedV1
                | EventType::NodeInvocationV1
                | EventType::GateExecutionBoundV1
                | EventType::CacheSnapshotMaterializedV1
                | EventType::AttemptDispatchedV1
                | EventType::NodeOutputReceiptV1
                | EventType::ProposalPreparedV1
                | EventType::ProposalRefusedV1
                | EventType::ProposalAcceptedV1
                | EventType::SliceSetAcceptedV1
                | EventType::ShardSetRecordedV1
                | EventType::SemanticClosureCheckedV1
                | EventType::IntegrationPreparedV1
                | EventType::IntegrationConflictV1
                | EventType::IntegrationChecksCompletedV1
                | EventType::IntegrationCommittedV1
        )
}

fn broker_receipt_consumes_call(receipt: &review_core::BrokerOperationReceiptV1) -> bool {
    matches!(
        receipt.outcome,
        review_core::BrokerOperationOutcomeV1::Succeeded
            | review_core::BrokerOperationOutcomeV1::Failed
    ) || (receipt.outcome == review_core::BrokerOperationOutcomeV1::Revoked
        && (receipt.response_digest.is_some() || receipt.charged_usage > 0))
}

fn broker_receipt_terminates_handle(receipt: &review_core::BrokerOperationReceiptV1) -> bool {
    matches!(
        receipt.failure_reason,
        Some(
            review_core::BrokerFailureReasonV1::AuthorityRevoked
                | review_core::BrokerFailureReasonV1::RequestTooLarge
                | review_core::BrokerFailureReasonV1::QuotaExceeded
                | review_core::BrokerFailureReasonV1::CredentialExposure
                | review_core::BrokerFailureReasonV1::UsageOverrun
        )
    )
}
