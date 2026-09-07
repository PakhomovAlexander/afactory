//! Proposal admission: the selected Attempt's result, the candidate and its base Snapshot
//! manifest, and the claims an accepted Proposal may name.

use rusqlite::params;
use serde_json::Value;

use crate::cas::Cas;
use crate::store::artifacts::is_digest;
use crate::store::{PreparedArtifacts, StoreError};

pub(crate) fn selected_attempt_result(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    batch_selected: &std::collections::BTreeMap<String, (String, String)>,
    node: &str,
    attempt: &str,
) -> Result<String, StoreError> {
    if let Some((selected_node, result)) = batch_selected.get(attempt) {
        if selected_node == node {
            return Ok(result.clone());
        }
        return Err(StoreError::Conflict(
            "Proposal Attempt metadata disagrees with selected admission".into(),
        ));
    }
    let rows = tx
        .prepare(
            "SELECT payload FROM events
             WHERE run_id = ?1 AND causation_id = ?2 AND node_id = ?3 AND attempt_id = ?4
               AND type = 'AttemptAdmitted@1' ORDER BY sequence",
        )?
        .query_map(params![run_id, round_event_id, node, attempt], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let [raw] = rows.as_slice() else {
        return Err(StoreError::Conflict(
            "Proposal has no unique selected Attempt admission".into(),
        ));
    };
    let admitted: review_core::event::AttemptAdmittedPayloadV1 = serde_json::from_str(raw)?;
    if admitted.selection != "selected" {
        return Err(StoreError::Conflict(
            "Proposal belongs to an unselected Attempt".into(),
        ));
    }
    admitted.result_artifact.ok_or_else(|| {
        StoreError::Conflict("Proposal's selected Attempt has no result artifact".into())
    })
}

pub(crate) fn proposal_candidate(
    cas: &Cas,
    prepared: &PreparedArtifacts,
    artifact_id: &str,
) -> Result<review_core::ProposalCandidateV1, StoreError> {
    let value = prepared
        .json
        .get(artifact_id)
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| {
            cas.get_json(artifact_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))
        })?;
    let candidate: review_core::ProposalCandidateV1 = serde_json::from_value(value)?;
    candidate
        .validate()
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    Ok(candidate)
}

pub(crate) fn prepared_proposal_authority(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    batch: &std::collections::BTreeMap<String, (String, String, String)>,
    candidate_id: &str,
) -> Result<(String, String, String), StoreError> {
    if let Some(authority) = batch.get(candidate_id) {
        return Ok(authority.clone());
    }
    let rows = tx
        .prepare(
            "SELECT node_id, attempt_id, payload FROM events
             WHERE run_id = ?1 AND causation_id = ?2 AND type = 'ProposalPrepared@1'
               AND json_extract(payload, '$.candidate_artifact_id') = ?3
             ORDER BY sequence",
        )?
        .query_map(params![run_id, round_event_id, candidate_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let [(node, attempt, raw)] = rows.as_slice() else {
        return Err(StoreError::Conflict(
            "accepted Proposal has no unique durable preparation".into(),
        ));
    };
    let payload: review_core::ProposalPreparedPayloadV1 = serde_json::from_str(raw)?;
    Ok((node.clone(), attempt.clone(), payload.result_artifact_id))
}

pub(crate) fn manifest_entries(
    value: &Value,
) -> Result<std::collections::BTreeMap<String, Value>, StoreError> {
    let object = value
        .as_object()
        .ok_or_else(|| StoreError::Conflict("Snapshot Manifest is not an object".into()))?;
    if object
        .keys()
        .any(|key| key != "entries" && key != "path_encoding")
        || object
            .get("path_encoding")
            .is_some_and(|encoding| !matches!(encoding.as_str(), Some("legacy_v1" | "percent_v2")))
    {
        return Err(StoreError::Conflict(
            "Snapshot Manifest has an unsupported shape or path encoding".into(),
        ));
    }
    let entries = object
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| StoreError::Conflict("Snapshot Manifest has no entries".into()))?;
    let mut mapped = std::collections::BTreeMap::new();
    let mut prior: Option<&str> = None;
    for entry in entries {
        let entry_object = entry.as_object().ok_or_else(|| {
            StoreError::Conflict("Snapshot Manifest entry is not an object".into())
        })?;
        if entry_object.len() != 4
            || entry_object
                .keys()
                .any(|key| !matches!(key.as_str(), "path" | "kind" | "content" | "size"))
        {
            return Err(StoreError::Conflict(
                "Snapshot Manifest entry has an unsupported shape".into(),
            ));
        }
        let path = entry_object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| StoreError::Conflict("Snapshot Manifest entry has no path".into()))?;
        if prior.is_some_and(|prior| prior.as_bytes() >= path.as_bytes())
            || !matches!(
                entry_object.get("kind").and_then(Value::as_str),
                Some("file" | "executable" | "symlink")
            )
            || !entry_object
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(is_digest)
            || entry_object.get("size").and_then(Value::as_u64).is_none()
        {
            return Err(StoreError::Conflict(
                "Snapshot Manifest entry is invalid or not canonically ordered".into(),
            ));
        }
        prior = Some(path);
        mapped.insert(path.to_string(), entry.clone());
    }
    Ok(mapped)
}

pub(crate) fn manifest_value(
    path_encoding: Option<Value>,
    entries: std::collections::BTreeMap<String, Value>,
) -> Value {
    let mut object = serde_json::Map::new();
    if let Some(path_encoding) = path_encoding {
        object.insert("path_encoding".into(), path_encoding);
    }
    object.insert(
        "entries".into(),
        Value::Array(entries.into_values().collect()),
    );
    Value::Object(object)
}

pub(crate) fn validate_candidate_manifest(
    cas: &Cas,
    subject: &review_core::SubjectV1,
    candidate: &review_core::ProposalCandidateV1,
) -> Result<(), StoreError> {
    if candidate.base_snapshot_id != subject.head_snapshot_id {
        return Err(StoreError::Conflict(
            "Proposal candidate is stale for the active Subject".into(),
        ));
    }
    let base_snapshot: review_core::SourceSnapshot = serde_json::from_value(
        cas.get_json(&candidate.base_snapshot_id)
            .map_err(|error| StoreError::Conflict(error.to_string()))?,
    )?;
    let base_manifest_id = base_snapshot
        .artifact_manifest
        .as_deref()
        .ok_or_else(|| StoreError::Conflict("Proposal Base Snapshot has no Manifest".into()))?;
    let base = cas
        .get_json(base_manifest_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let derived = cas
        .get_json(&candidate.derived_manifest_artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let base_entries = manifest_entries(&base)?;
    let derived_entries = manifest_entries(&derived)?;
    if base.get("path_encoding") != derived.get("path_encoding") {
        return Err(StoreError::Conflict(
            "Proposal candidate changed the Manifest path encoding".into(),
        ));
    }
    let changed = base_entries
        .keys()
        .chain(derived_entries.keys())
        .filter(|path| base_entries.get(*path) != derived_entries.get(*path))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if changed
        != candidate
            .paths
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    {
        return Err(StoreError::Conflict(
            "Proposal candidate Manifest changes paths outside its declaration".into(),
        ));
    }
    cas.verify(&candidate.patch_artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    Ok(())
}

pub(crate) fn validate_accepted_proposal_claims(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    node: &str,
    candidate: &review_core::ProposalCandidateV1,
    proposal: &review_core::PatchProposal,
) -> Result<(), StoreError> {
    let report_rows = tx
        .prepare(
            "SELECT payload FROM events WHERE run_id = ?1 AND causation_id = ?2
             AND type = 'FindingReported@1' AND node_id = ?3 ORDER BY sequence",
        )?
        .query_map(params![run_id, round_event_id, node], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let report_ids = report_rows
        .iter()
        .filter_map(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter_map(|payload| {
            payload
                .get("report_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut findings = Vec::new();
    let mut reports = Vec::new();
    let mut unique = std::collections::BTreeSet::new();
    for claim in &proposal.finding_refs {
        let kind = match claim.kind {
            review_core::ClaimRefKind::Finding => "finding",
            review_core::ClaimRefKind::Report => "report",
        };
        if !is_digest(&claim.id) || !unique.insert((kind, claim.id.as_str())) {
            return Err(StoreError::Conflict(
                "accepted Proposal repeats or malforms a claim reference".into(),
            ));
        }
        match claim.kind {
            review_core::ClaimRefKind::Finding => findings.push(claim.id.clone()),
            review_core::ClaimRefKind::Report => reports.push(claim.id.clone()),
        }
    }
    findings.sort();
    reports.sort();
    if findings != candidate.finding_ids
        || reports.len() != candidate.report_indexes.len()
        || reports.iter().any(|report| !report_ids.contains(report))
    {
        return Err(StoreError::Conflict(
            "accepted Proposal claim links contradict its selected Attempt reduction".into(),
        ));
    }
    for finding in &findings {
        let known: i64 = tx.query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1
             AND type = 'FindingReported@1' AND correlation_id = ?2",
            params![run_id, finding],
            |row| row.get(0),
        )?;
        if known == 0 {
            return Err(StoreError::Conflict(
                "accepted Proposal names an unknown Finding".into(),
            ));
        }
    }
    Ok(())
}
