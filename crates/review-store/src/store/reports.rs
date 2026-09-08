//! RunReport admission: outcomes against the plan, Gate bindings, cache snapshots, and the
//! receipts a closing report must agree with.

use review_core::definition::CacheKindSpec;
use review_core::{EventType, RunEvent};
use rusqlite::params;
use serde_json::Value;

use crate::cas::Cas;
use crate::store::authority::{
    AuthorityPlan, dynamic_node_authority, load_authority_plan, pinned_cache_kind,
};
use crate::store::{PreparedArtifacts, StoreError};

fn report_outcomes(
    event_type: EventType,
    payload: &Value,
) -> Result<Vec<review_core::RunNodeReportV2>, StoreError> {
    match event_type {
        EventType::RunReportV2 => Ok(serde_json::from_value::<review_core::RunReportPayloadV2>(
            payload.clone(),
        )?
        .outcomes),
        EventType::RunReportV3 => Ok(serde_json::from_value::<review_core::RunReportPayloadV3>(
            payload.clone(),
        )?
        .outcomes),
        EventType::RunReportV4 => Ok(serde_json::from_value::<review_core::RunReportPayloadV4>(
            payload.clone(),
        )?
        .outcomes),
        EventType::RunReportV5 => Ok(serde_json::from_value::<review_core::RunReportPayloadV5>(
            payload.clone(),
        )?
        .outcomes),
        _ => Err(StoreError::Conflict(format!(
            "{event_type} has no structural run-report outcomes"
        ))),
    }
}

pub(crate) fn validate_report_plan(
    plan: &AuthorityPlan,
    event_type: EventType,
    payload: &Value,
) -> Result<(), StoreError> {
    let outcomes = report_outcomes(event_type, payload)?;
    let expected: std::collections::BTreeSet<&str> =
        plan.nodes.keys().map(String::as_str).collect();
    let actual: std::collections::BTreeSet<&str> = outcomes
        .iter()
        .map(|outcome| outcome.node.as_str())
        .collect();
    if expected != actual || actual.len() != outcomes.len() {
        return Err(StoreError::Conflict(format!(
            "{event_type} does not cover exactly the pinned Campaign plan"
        )));
    }
    let reports_bindings = matches!(event_type, EventType::RunReportV4 | EventType::RunReportV5);
    if reports_bindings != plan.gate_bound {
        return Err(StoreError::Conflict(format!(
            "{event_type} does not match the pinned pipeline's Gate Execution Binding version"
        )));
    }
    let reports_caches = event_type == EventType::RunReportV5;
    if reports_caches == plan.cache_kinds.is_empty() {
        return Err(StoreError::Conflict(format!(
            "{event_type} does not match the pinned pipeline's Cache Snapshot authority"
        )));
    }
    if reports_bindings {
        let bindings = match event_type {
            EventType::RunReportV4 => {
                serde_json::from_value::<review_core::RunReportPayloadV4>(payload.clone())?
                    .execution_bindings
            }
            EventType::RunReportV5 => {
                serde_json::from_value::<review_core::RunReportPayloadV5>(payload.clone())?
                    .execution_bindings
            }
            _ => unreachable!("reports_bindings accepted only RunReport@4/@5"),
        };
        let binding_nodes: std::collections::BTreeSet<String> =
            bindings.into_iter().map(|binding| binding.node).collect();
        if binding_nodes != plan.gate_nodes {
            return Err(StoreError::Conflict(
                "RunReport@4 does not cover exactly the pinned Gate nodes".into(),
            ));
        }
    }
    if event_type == EventType::RunReportV5 {
        let report: review_core::RunReportPayloadV5 = serde_json::from_value(payload.clone())?;
        let actual: std::collections::BTreeSet<(String, CacheKindSpec)> = report
            .cache_snapshots
            .into_iter()
            .map(|snapshot| (snapshot.node, pinned_cache_kind(snapshot.kind)))
            .chain(
                report
                    .cache_failures
                    .into_iter()
                    .map(|failure| (failure.node, pinned_cache_kind(failure.kind))),
            )
            .collect();
        let expected: std::collections::BTreeSet<(String, CacheKindSpec)> = plan
            .gate_nodes
            .iter()
            .flat_map(|node| {
                plan.cache_kinds
                    .iter()
                    .map(move |kind| (node.clone(), *kind))
            })
            .collect();
        if actual != expected {
            return Err(StoreError::Conflict(
                "RunReport@5 does not cover exactly the pinned Gate cache requests".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_report_gate_bindings(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    event_type: EventType,
    payload: &Value,
) -> Result<(), StoreError> {
    let bindings = match event_type {
        EventType::RunReportV4 => {
            serde_json::from_value::<review_core::RunReportPayloadV4>(payload.clone())?
                .execution_bindings
        }
        EventType::RunReportV5 => {
            serde_json::from_value::<review_core::RunReportPayloadV5>(payload.clone())?
                .execution_bindings
        }
        _ => {
            return Err(StoreError::Conflict(format!(
                "{event_type} has no Gate Execution Bindings"
            )));
        }
    };
    let reported: std::collections::BTreeMap<_, _> = bindings
        .into_iter()
        .map(|binding| (binding.node.clone(), binding))
        .collect();
    let mut statement = tx.prepare(
        "SELECT node_id, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type = 'GateExecutionBound@1'
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut durable = std::collections::BTreeMap::new();
    for row in rows {
        let (node, payload) = row?;
        let binding: review_core::RunExecutionBindingV4 = serde_json::from_str(&payload)?;
        if binding.node != node {
            return Err(StoreError::Conflict(
                "durable Gate Execution Binding metadata disagrees with its payload".into(),
            ));
        }
        durable.insert(node, binding);
    }
    if durable != reported {
        return Err(StoreError::Conflict(format!(
            "{event_type} bindings differ from the durable Gate execution facts"
        )));
    }
    Ok(())
}

pub(crate) fn validate_report_cache_snapshots(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    round_event_id: &str,
    payload: &Value,
    artifact_refs: &[String],
    prepared: &PreparedArtifacts,
) -> Result<(), StoreError> {
    let report: review_core::RunReportPayloadV5 = serde_json::from_value(payload.clone())?;
    let expected_refs: std::collections::BTreeSet<_> = report
        .cache_snapshots
        .iter()
        .map(|snapshot| snapshot.source_digest.clone())
        .collect();
    let reported_refs: std::collections::BTreeSet<_> = artifact_refs.iter().cloned().collect();
    if artifact_refs.len() != reported_refs.len() || !expected_refs.is_subset(&reported_refs) {
        return Err(StoreError::Conflict(
            "RunReport@5 must reference each successful Cache Snapshot manifest exactly once"
                .into(),
        ));
    }
    for snapshot in &report.cache_snapshots {
        let manifest = prepared.json.get(&snapshot.source_digest).ok_or_else(|| {
            StoreError::Conflict("RunReport@5 Cache Snapshot manifest was not prepared".into())
        })?;
        let manifest: review_core::CacheManifestV1 = serde_json::from_value(manifest.clone())?;
        manifest.validate().map_err(StoreError::Conflict)?;
        if manifest.kind != snapshot.kind
            || u64::try_from(manifest.entries.len()).ok() != Some(snapshot.files)
            || manifest.bytes() != snapshot.bytes
        {
            return Err(StoreError::Conflict(
                "RunReport@5 Cache Snapshot receipt contradicts CacheManifest@1".into(),
            ));
        }
    }
    let failed: std::collections::BTreeSet<_> = report
        .cache_failures
        .iter()
        .map(|failure| (failure.node.clone(), failure.kind))
        .collect();
    let reported: std::collections::BTreeMap<_, _> = report
        .cache_snapshots
        .into_iter()
        .map(|snapshot| ((snapshot.node.clone(), snapshot.kind), snapshot))
        .collect();
    let mut statement = tx.prepare(
        "SELECT node_id, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type = 'CacheSnapshotMaterialized@1'
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut durable = std::collections::BTreeMap::new();
    for row in rows {
        let (node, payload) = row?;
        let snapshot: review_core::RunCacheSnapshotV5 = serde_json::from_str(&payload)?;
        if snapshot.node != node {
            return Err(StoreError::Conflict(
                "durable Cache Snapshot metadata disagrees with its payload".into(),
            ));
        }
        if !failed.contains(&(node.clone(), snapshot.kind)) {
            durable.insert((node, snapshot.kind), snapshot);
        }
    }
    if durable != reported {
        return Err(StoreError::Conflict(
            "RunReport@5 Cache Snapshots differ from the durable materialization facts".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_report_receipts(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    round_event_id: &str,
    event_type: EventType,
    payload: &Value,
) -> Result<(), StoreError> {
    let outcomes = report_outcomes(event_type, payload)?;
    let reported_outputs: std::collections::BTreeMap<String, Vec<String>> = outcomes
        .iter()
        .filter_map(|outcome| match &outcome.outcome {
            review_core::RunNodeOutcomeV2::Completed { output_artifacts } => {
                Some((outcome.node.clone(), output_artifacts.clone()))
            }
            _ => None,
        })
        .collect();
    let mut receipts = std::collections::BTreeMap::new();
    let mut statement = tx.prepare(
        "SELECT node_id, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type = 'NodeOutputReceipt@1'
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (node, raw) = row?;
        let receipt: review_core::NodeOutputReceiptPayloadV1 = serde_json::from_str(&raw)?;
        let node =
            node.ok_or_else(|| StoreError::Conflict("NodeOutputReceipt@1 has no node ID".into()))?;
        if node != receipt.node || receipts.insert(node, receipt).is_some() {
            return Err(StoreError::Conflict(format!(
                "{event_type} has ambiguous durable output receipts"
            )));
        }
    }
    for outcome in outcomes {
        if let review_core::RunNodeOutcomeV2::Completed {
            mut output_artifacts,
        } = outcome.outcome
        {
            output_artifacts.sort();
            let receipt = receipts.remove(&outcome.node).ok_or_else(|| {
                StoreError::Conflict(format!(
                    "{event_type} completed node '{}' without a durable receipt",
                    outcome.node,
                ))
            })?;
            let mut durable: Vec<String> = receipt
                .outputs
                .into_iter()
                .flat_map(|port| port.artifact_ids)
                .collect();
            durable.sort();
            if durable != output_artifacts {
                return Err(StoreError::Conflict(format!(
                    "{event_type} contradicts the receipt for node '{}'",
                    outcome.node,
                )));
            }
        } else if receipts.contains_key(&outcome.node) {
            return Err(StoreError::Conflict(format!(
                "{event_type} suppresses or fails node '{}' after it published a receipt",
                outcome.node,
            )));
        }
    }
    if !receipts.is_empty() {
        // Dynamic shard nodes are owned by one static Scatter and therefore are not top-level
        // plan outcomes. They are nevertheless complete only when every exact receipt is
        // represented in that Scatter's reported ShardSet; this is transitive receipt coverage,
        // not an exception to it.
        let plan = load_authority_plan(tx, cas, run_id)?;
        for (node, receipt) in receipts {
            let authority = dynamic_node_authority(tx, cas, run_id, round_event_id, &plan, &node)?
                .ok_or_else(|| {
                    StoreError::Conflict(format!(
                        "{event_type} omits static node `{node}` with a durable output receipt"
                    ))
                })?;
            let owner = node
                .split_once("#slice:")
                .map(|(owner, _)| owner)
                .ok_or_else(|| {
                    StoreError::Conflict("dynamic receipt has no tagged Scatter owner".into())
                })?;
            let shard_set_record = reported_outputs
                .get(owner)
                .into_iter()
                .flatten()
                .find_map(|artifact| {
                    let value = cas.get_json(artifact).ok()?;
                    let envelope =
                        serde_json::from_value::<review_core::ArtifactEnvelope>(value).ok()?;
                    (envelope.artifact_type == review_core::contract::SHARD_SET_V1)
                        .then_some(envelope)
                })
                .ok_or_else(|| {
                    StoreError::Conflict(format!(
                        "dynamic receipt `{node}` has no reported owner ShardSet"
                    ))
                })?;
            crate::validate_envelope(&shard_set_record).map_err(StoreError::Conflict)?;
            let shard_set: review_core::ShardSetV1 =
                serde_json::from_value(shard_set_record.payload)?;
            shard_set.validate_shape().map_err(StoreError::Conflict)?;
            let shard = shard_set
                .shards
                .iter()
                .find(|shard| {
                    shard.runtime_node_id == node && shard.slice_id == authority.slice.slice_id
                })
                .ok_or_else(|| {
                    StoreError::Conflict(format!(
                        "dynamic receipt `{node}` is absent from its owner ShardSet"
                    ))
                })?;
            let review_core::ShardOutcomeV1::Completed {
                result_artifact_ids,
            } = &shard.outcome
            else {
                return Err(StoreError::Conflict(format!(
                    "dynamic receipt `{node}` contradicts a non-completed Shard outcome"
                )));
            };
            let mut durable: Vec<String> = receipt
                .outputs
                .into_iter()
                .flat_map(|port| port.artifact_ids)
                .collect();
            durable.sort();
            let mut represented = result_artifact_ids.clone();
            represented.sort();
            if durable != represented {
                return Err(StoreError::Conflict(format!(
                    "dynamic receipt `{node}` contradicts its exact Shard outcome"
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn report_closes(event_type: EventType, payload: &Value) -> Result<bool, StoreError> {
    let event = RunEvent {
        event_id: String::new(),
        run_id: String::new(),
        sequence: 0,
        event_type,
        occurred_at: "1970-01-01T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs: Vec::new(),
        payload: payload.clone(),
    };
    review_core::run_report_closes_round(&event)
        .map_err(StoreError::Json)
        .map(Option::unwrap_or_default)
}
