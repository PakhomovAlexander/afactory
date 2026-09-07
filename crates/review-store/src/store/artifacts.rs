//! Typed artifact admission: every artifact an event references is typed by the plan's ports
//! and its payload validated before the publication barrier.

use std::sync::Arc;

use review_core::EventType;
use review_core::definition::PortContractSpec;
use serde_json::Value;

use crate::store::{NewEvent, PreparedArtifacts, StoreError, validate_reviewer_result};

pub(crate) fn typed_json_artifacts(
    events: &[NewEvent],
) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
    let mut artifacts = std::collections::BTreeMap::new();
    for event in events {
        let ports = match event.event_type {
            EventType::NodeInvocationV1 => {
                serde_json::from_value::<review_core::NodeInvocationPayloadV1>(
                    event.payload.clone(),
                )?
                .inputs
            }
            EventType::NodeOutputReceiptV1 => {
                serde_json::from_value::<review_core::NodeOutputReceiptPayloadV1>(
                    event.payload.clone(),
                )?
                .outputs
            }
            EventType::AttemptInputV1 => {
                let payload: review_core::event::AttemptInputPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.refusal_history_id,
                    review_core::contract::REFUSAL_HISTORY_V1.into(),
                )?;
                continue;
            }
            EventType::AttemptFeedbackV1 => {
                let payload: review_core::event::AttemptFeedbackPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.refusal_history_id,
                    review_core::contract::REFUSAL_HISTORY_V1.into(),
                )?;
                continue;
            }
            EventType::CacheSnapshotMaterializedV1 => {
                let snapshot: review_core::RunCacheSnapshotV5 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    snapshot.source_digest,
                    review_core::contract::CACHE_MANIFEST_V1.into(),
                )?;
                continue;
            }
            EventType::RunReportV5 => {
                let report: review_core::RunReportPayloadV5 =
                    serde_json::from_value(event.payload.clone())?;
                for snapshot in report.cache_snapshots {
                    insert_artifact_type(
                        &mut artifacts,
                        snapshot.source_digest,
                        review_core::contract::CACHE_MANIFEST_V1.into(),
                    )?;
                }
                continue;
            }
            EventType::SliceSetAcceptedV1 => {
                let payload: review_core::SliceSetAcceptedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.slice_set_artifact_id,
                    review_core::contract::SLICE_SET_V1.into(),
                )?;
                continue;
            }
            EventType::ShardSetRecordedV1 | EventType::SemanticClosureCheckedV1 => {
                let payload: review_core::RecordedSetPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                let artifact_type = if event.event_type == EventType::ShardSetRecordedV1 {
                    review_core::contract::SHARD_SET_V1
                } else {
                    review_core::contract::SEMANTIC_CLOSURE_V1
                };
                insert_artifact_type(&mut artifacts, payload.record_id, artifact_type.into())?;
                continue;
            }
            EventType::IntegrationPreparedV1 => {
                let payload: review_core::IntegrationPreparedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.plan_artifact_id,
                    review_core::contract::INTEGRATION_PLAN_V1.into(),
                )?;
                continue;
            }
            EventType::IntegrationChecksCompletedV1 => {
                let payload: review_core::IntegrationChecksCompletedPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.checks_artifact_id,
                    review_core::contract::INTEGRATION_CHECKS_V1.into(),
                )?;
                continue;
            }
            EventType::DemandRecordedV1
            | EventType::DemandWaivedV1
            | EventType::EvidenceAddedV1
            | EventType::EvidenceReuseAdmittedV1
            | EventType::EvidenceSatisfiedV1
            | EventType::ChangeAttestedV1
            | EventType::FixVerifiedV1
            | EventType::FindingResolutionRecordedV1
            | EventType::FindingResolutionChallengedV1
            | EventType::PolicyTimeAdvancedV1 => {
                let payload: review_core::RecordedArtifactPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                let artifact_type = match event.event_type {
                    EventType::DemandRecordedV1 => review_core::contract::DEMAND_V1,
                    EventType::DemandWaivedV1 => review_core::contract::DEMAND_WAIVER_V1,
                    EventType::EvidenceAddedV1 => review_core::contract::EVIDENCE_V1,
                    EventType::EvidenceReuseAdmittedV1 => {
                        review_core::contract::EVIDENCE_REUSE_ADMISSION_V1
                    }
                    EventType::EvidenceSatisfiedV1 => {
                        review_core::contract::EVIDENCE_SATISFACTION_V1
                    }
                    EventType::ChangeAttestedV1 => review_core::contract::CHANGE_ATTESTATION_V1,
                    EventType::FixVerifiedV1 => review_core::contract::FIX_VERIFICATION_V1,
                    EventType::FindingResolutionRecordedV1 => {
                        review_core::contract::FINDING_RESOLUTION_V1
                    }
                    EventType::FindingResolutionChallengedV1 => {
                        review_core::contract::RESOLUTION_CHALLENGE_V1
                    }
                    EventType::PolicyTimeAdvancedV1 => review_core::contract::POLICY_TIME_V1,
                    _ => unreachable!(),
                };
                insert_artifact_type(&mut artifacts, payload.artifact_id, artifact_type.into())?;
                continue;
            }
            EventType::FindingsGroupedV1 | EventType::FindingsUngroupedV1 => {
                let payload: review_core::FindingGroupingEventPayloadV1 =
                    serde_json::from_value(event.payload.clone())?;
                insert_artifact_type(
                    &mut artifacts,
                    payload.grouping_artifact_id,
                    review_core::contract::FINDING_GROUPING_V1.into(),
                )?;
                continue;
            }
            _ => continue,
        };
        for port in ports {
            for artifact_id in port.artifact_ids {
                insert_artifact_type(&mut artifacts, artifact_id, port.artifact_type.clone())?;
            }
        }
    }
    Ok(artifacts)
}

fn insert_artifact_type(
    artifacts: &mut std::collections::BTreeMap<String, String>,
    artifact_id: String,
    artifact_type: String,
) -> Result<(), StoreError> {
    if let Some(previous) = artifacts.insert(artifact_id.clone(), artifact_type.clone())
        && previous != artifact_type
    {
        return Err(StoreError::Conflict(format!(
            "artifact {artifact_id} is assigned conflicting types"
        )));
    }
    Ok(())
}

pub(crate) fn validate_plan_ports(
    prepared: &PreparedArtifacts,
    expected: &[PortContractSpec],
    actual: &[review_core::PortArtifactsV1],
    subject_snapshot_id: &str,
    subject_base_snapshot_id: Option<&str>,
    subject_change_set_id: Option<&str>,
) -> Result<(), StoreError> {
    if expected.len() != actual.len() {
        return Err(StoreError::Conflict(
            "durable port map does not cover the pinned node contract".into(),
        ));
    }
    let actual: std::collections::BTreeMap<&str, &review_core::PortArtifactsV1> = actual
        .iter()
        .map(|port| (port.port.as_str(), port))
        .collect();
    for expected in expected {
        let port = actual.get(expected.name()).ok_or_else(|| {
            StoreError::Conflict(format!(
                "durable port map omits pinned port '{}'",
                expected.name()
            ))
        })?;
        if port.artifact_type != expected.artifact_type()
            || port.cardinality != expected.cardinality()
            || port.optional != expected.optional()
            || port.snapshot_affinity != expected.snapshot_affinity()
        {
            return Err(StoreError::Conflict(format!(
                "durable port '{}' contradicts the pinned contract",
                expected.name()
            )));
        }
        let same_subject = port.snapshot_affinity == review_core::SnapshotAffinity::SameSubject;
        if same_subject && port.subject_snapshot_id.as_deref() != Some(subject_snapshot_id) {
            return Err(StoreError::Conflict(format!(
                "durable port '{}' is bound to the wrong Subject snapshot",
                expected.name()
            )));
        }
        let mut validated_change_set = None;
        for artifact in &port.artifact_ids {
            if let Some(change_set) =
                validate_artifact_payload(prepared, &port.artifact_type, artifact)?
            {
                validated_change_set = Some(change_set);
            }
            if same_subject
                && let Some(value) = prepared.json.get(artifact)
                && value.get("type").is_some()
            {
                let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())
                    .map_err(|error| {
                        StoreError::Conflict(format!(
                            "typed artifact {artifact} is not an envelope: {error}"
                        ))
                    })?;
                if envelope.subject_snapshot_id.as_deref() != Some(subject_snapshot_id) {
                    return Err(StoreError::Conflict(format!(
                        "typed artifact {artifact} is bound to the wrong Subject snapshot"
                    )));
                }
            }
        }
        if port.artifact_type == review_core::contract::CHANGE_SET_V1 {
            let expected = subject_change_set_id.ok_or_else(|| {
                StoreError::Conflict("whole-tree Subject cannot carry a ChangeSet@1 port".into())
            })?;
            if port.artifact_ids.first().map(String::as_str) != Some(expected)
                || port.artifact_ids.len() != 1
            {
                return Err(StoreError::Conflict(
                    "ChangeSet@1 port does not carry the Subject's exact Change Set".into(),
                ));
            }
            let change_set = validated_change_set
                .ok_or_else(|| StoreError::Conflict("ChangeSet@1 port was not validated".into()))?;
            if change_set.head_snapshot_id != subject_snapshot_id
                || Some(change_set.base_snapshot_id.as_str()) != subject_base_snapshot_id
            {
                return Err(StoreError::Conflict(
                    "ChangeSet@1 Base or head contradicts the active Subject".into(),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_artifact_payload(
    prepared: &PreparedArtifacts,
    artifact_type: &str,
    artifact_id: &str,
) -> Result<Option<Arc<review_core::ChangeSetV1>>, StoreError> {
    if !prepared.verified.contains(artifact_id) {
        return Err(StoreError::Conflict(format!(
            "typed artifact {artifact_id} is absent from the event's verified references"
        )));
    }
    if artifact_type == review_core::contract::OPAQUE_V1 {
        return Ok(None);
    }
    if artifact_type == review_core::contract::CHANGE_SET_V1
        && let Some(change_set) = prepared.change_sets.get(artifact_id)
    {
        // Publication preparation re-established current CAS integrity for this exact reference
        // and made every Change Set in the batch independently available to validation.
        return Ok(Some(Arc::clone(change_set)));
    }
    let value = prepared.json.get(artifact_id).ok_or_else(|| {
        StoreError::Conflict(format!(
            "typed artifact {artifact_id} has no value from publication preparation"
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        StoreError::Conflict(format!("{artifact_type} artifact is not a JSON object"))
    })?;
    match artifact_type {
        review_core::contract::CHANGE_SET_V1 => {
            return Err(StoreError::Conflict(format!(
                "ChangeSet@1 artifact {artifact_id} was not prepared for validation"
            )));
        }
        review_core::contract::GATE_DECISION_V1 => {
            exact_keys(
                object,
                &["outcome", "blocking", "reasons", "executed", "required"],
                artifact_type,
            )?;
            if !matches!(value["outcome"].as_str(), Some("Passed" | "Blocked"))
                || !string_array(&value["blocking"])
                || !string_array(&value["reasons"])
                || value["executed"].as_u64().is_none()
                || value["required"].as_u64().is_none()
            {
                return Err(StoreError::Conflict(
                    "GateDecision@1 artifact violates its payload contract".into(),
                ));
            }
        }
        review_core::contract::PRIOR_FINDINGS_V1 => {
            exact_keys(
                object,
                &["subject_id", "round", "prior_findings"],
                artifact_type,
            )?;
            if value["subject_id"].as_str().is_none()
                || value["round"].as_u64().is_none()
                || value["prior_findings"].as_array().is_none()
            {
                return Err(StoreError::Conflict(
                    "PriorFindings@1 artifact violates its payload contract".into(),
                ));
            }
        }
        review_core::contract::REVIEWER_RESULT_V1 => validate_reviewer_result(value)?,
        review_core::contract::REVIEWER_RESULT_V2 => {
            review_core::validate_reviewer_result_v2(value).map_err(StoreError::Conflict)?
        }
        review_core::contract::REVIEW_SLICE_V1 => {
            let payload: review_core::ReviewSliceV1 =
                validated_envelope_payload(value, review_core::contract::REVIEW_SLICE_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::SLICE_SET_V1 => {
            let payload: review_core::SliceSetV1 =
                validated_envelope_payload(value, review_core::contract::SLICE_SET_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::SHARD_SET_V1 => {
            let payload: review_core::ShardSetV1 =
                validated_envelope_payload(value, review_core::contract::SHARD_SET_V1)?;
            payload.validate_shape().map_err(StoreError::Conflict)?;
        }
        review_core::contract::SEMANTIC_CLOSURE_V1 => {
            let payload: review_core::SemanticClosureV1 =
                validated_envelope_payload(value, review_core::contract::SEMANTIC_CLOSURE_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::INTEGRATION_PLAN_V1 => {
            let payload: review_core::IntegrationPlanV1 = serde_json::from_value(value.clone())?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::INTEGRATION_CHECKS_V1 => {
            let payload: review_core::IntegrationChecksV1 = serde_json::from_value(value.clone())?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::FINDING_DISPOSITION_V1 => {
            if value.get("type").is_none() {
                return Err(StoreError::Conflict(
                    "FindingDisposition@1 artifact is not an envelope".into(),
                ));
            }
            let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())
                .map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingDisposition@1 artifact is not an envelope: {error}"
                    ))
                })?;
            crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
            if envelope.artifact_type != review_core::contract::FINDING_DISPOSITION_V1 {
                return Err(StoreError::Conflict(
                    "FindingDisposition@1 envelope carries the wrong type".into(),
                ));
            }
            let payload: review_core::FindingDispositionV1 =
                serde_json::from_value(envelope.payload).map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingDisposition@1 envelope has an invalid payload: {error}"
                    ))
                })?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::FINDING_GROUPING_V1 => {
            if value.get("type").is_none() {
                return Err(StoreError::Conflict(
                    "FindingGrouping@1 artifact is not an envelope".into(),
                ));
            }
            let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())
                .map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingGrouping@1 artifact is not an envelope: {error}"
                    ))
                })?;
            crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
            if envelope.artifact_type != review_core::contract::FINDING_GROUPING_V1 {
                return Err(StoreError::Conflict(
                    "FindingGrouping@1 envelope carries the wrong type".into(),
                ));
            }
            let payload: review_core::FindingGroupingV1 = serde_json::from_value(envelope.payload)
                .map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingGrouping@1 envelope has an invalid payload: {error}"
                    ))
                })?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::DEMAND_V1 => {
            let payload: review_core::DemandV1 =
                validated_envelope_payload(value, review_core::contract::DEMAND_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::DEMAND_SET_V1 => {
            let payload: review_core::DemandSetV1 =
                validated_envelope_payload(value, review_core::contract::DEMAND_SET_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::EVIDENCE_V1 => {
            let payload: review_core::EvidenceV1 =
                validated_envelope_payload(value, review_core::contract::EVIDENCE_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::EVIDENCE_SATISFACTION_V1 => {
            let payload: review_core::EvidenceSatisfactionV1 =
                validated_envelope_payload(value, review_core::contract::EVIDENCE_SATISFACTION_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::EVIDENCE_REUSE_ADMISSION_V1 => {
            let payload: review_core::EvidenceReuseAdmissionV1 = validated_envelope_payload(
                value,
                review_core::contract::EVIDENCE_REUSE_ADMISSION_V1,
            )?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::DEMAND_WAIVER_V1 => {
            let payload: review_core::DemandWaiverV1 =
                validated_envelope_payload(value, review_core::contract::DEMAND_WAIVER_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::CHANGE_ATTESTATION_V1 => {
            let payload: review_core::ChangeAttestationV1 =
                validated_envelope_payload(value, review_core::contract::CHANGE_ATTESTATION_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::FIX_VERIFICATION_V1 => {
            let payload: review_core::FixVerificationV1 =
                validated_envelope_payload(value, review_core::contract::FIX_VERIFICATION_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::FINDING_RESOLUTION_V1 => {
            let payload: review_core::FindingResolutionV1 =
                validated_envelope_payload(value, review_core::contract::FINDING_RESOLUTION_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::RESOLUTION_CHALLENGE_V1 => {
            let payload: review_core::ResolutionChallengeV1 =
                validated_envelope_payload(value, review_core::contract::RESOLUTION_CHALLENGE_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::POLICY_TIME_V1 => {
            let payload: review_core::PolicyTimeV1 =
                validated_envelope_payload(value, review_core::contract::POLICY_TIME_V1)?;
            payload.validate().map_err(StoreError::Conflict)?;
        }
        review_core::contract::REPORT_SET_V1 => {
            if object.is_empty()
                || object.values().any(|ids| {
                    ids.as_array().is_none_or(|ids| {
                        ids.is_empty()
                            || ids
                                .iter()
                                .any(|id| id.as_str().is_none_or(|id| !is_digest(id)))
                    })
                })
            {
                return Err(StoreError::Conflict(
                    "ReportSet@1 artifact violates its payload contract".into(),
                ));
            }
        }
        review_core::contract::FINDING_SET_V1 => {
            if value.get("type").is_some() {
                let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value.clone())
                    .map_err(|error| {
                        StoreError::Conflict(format!(
                            "FindingSet@1 artifact is not an envelope: {error}"
                        ))
                    })?;
                crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
                if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
                    return Err(StoreError::Conflict(
                        "FindingSet@1 envelope carries the wrong type".into(),
                    ));
                }
                let payload: review_core::FindingSetV1 = serde_json::from_value(envelope.payload)
                    .map_err(|error| {
                    StoreError::Conflict(format!(
                        "FindingSet@1 envelope has an invalid payload: {error}"
                    ))
                })?;
                payload.validate().map_err(StoreError::Conflict)?;
            } else {
                // Permanent reader for the pre-M3 summary artifact.
                exact_keys(object, &["round", "sources", "findings"], artifact_type)?;
                if value["round"].as_u64().is_none()
                    || !string_array(&value["sources"])
                    || value["findings"].as_u64().is_none()
                {
                    return Err(StoreError::Conflict(
                        "FindingSet@1 artifact violates its payload contract".into(),
                    ));
                }
            }
        }
        _ => {
            return Err(StoreError::Conflict(format!(
                "no payload validator is registered for {artifact_type}"
            )));
        }
    }
    Ok(None)
}

pub(crate) fn validate_prepared_refusal_history(
    prepared: &PreparedArtifacts,
    artifact_id: &str,
    event_type: &str,
) -> Result<(), StoreError> {
    let history = prepared
        .json
        .get(artifact_id)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            StoreError::Conflict(format!(
                "{event_type} refusal history is not a verified JSON array"
            ))
        })?;
    if history.is_empty()
        || history
            .iter()
            .any(|entry| entry.as_str().is_none_or(|entry| entry.trim().is_empty()))
    {
        return Err(StoreError::Conflict(format!(
            "{event_type} has empty refusal history"
        )));
    }
    Ok(())
}

pub(crate) fn validated_envelope_payload<T: serde::de::DeserializeOwned>(
    value: &Value,
    expected_type: &str,
) -> Result<T, StoreError> {
    let envelope: review_core::ArtifactEnvelope =
        serde_json::from_value(value.clone()).map_err(|error| {
            StoreError::Conflict(format!(
                "{expected_type} artifact is not an envelope: {error}"
            ))
        })?;
    crate::canonical::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
    if envelope.artifact_type != expected_type {
        return Err(StoreError::Conflict(format!(
            "{expected_type} envelope carries type {}",
            envelope.artifact_type
        )));
    }
    serde_json::from_value(envelope.payload).map_err(StoreError::from)
}

fn exact_keys(
    object: &serde_json::Map<String, Value>,
    expected: &[&str],
    artifact_type: &str,
) -> Result<(), StoreError> {
    if object.len() != expected.len() || object.keys().any(|key| !expected.contains(&key.as_str()))
    {
        return Err(StoreError::Conflict(format!(
            "{artifact_type} artifact has unexpected or missing fields"
        )));
    }
    Ok(())
}

fn string_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().all(|item| item.as_str().is_some()))
}

pub(crate) fn is_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
