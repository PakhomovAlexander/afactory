//! What the latest Round left behind — the Ledger it produced, or the reviewer results that
//! stayed recorded-not-gathered — shown alike by `run`, `report`, and `ledger` (ADR-0034).

use std::collections::BTreeSet;

use review_core::{
    EventType, RunReportPayloadV2, RunReportPayloadV3, RunReportPayloadV4, RunReportPayloadV5,
};
use review_store::Cas;

#[derive(Debug, serde::Serialize)]
pub(crate) struct AvailableNodeResult {
    pub(crate) node: String,
    pub(crate) attempt_id: String,
    pub(crate) result_artifact_id: String,
    pub(crate) severities: Vec<String>,
    pub(crate) spend_tokens: u64,
    pub(crate) findings: Vec<serde_json::Value>,
}

#[derive(serde::Serialize)]
pub(crate) struct LatestRoundEvidence {
    pub(crate) ledger_production: &'static str,
    pub(crate) available_node_results: Vec<AvailableNodeResult>,
}

impl LatestRoundEvidence {
    pub(crate) fn ledger_was_not_produced(&self) -> bool {
        self.ledger_production.starts_with("not_produced_")
    }

    pub(crate) fn absence_reason(&self) -> &'static str {
        match self.ledger_production {
            "not_produced_upstream_missing" => "required upstream output was missing",
            "not_produced_failed" => "the Ledger node failed",
            "not_produced_gate_blocked" => "the Ledger node was gate-blocked",
            _ => "the Ledger node did not produce an authoritative output",
        }
    }
}

/// The pipeline definition a Round's Campaign Manifest pins, read back from the CAS.
pub(crate) fn pinned_pipeline_definition(
    round_event: &review_core::RunEvent,
    cas: &Cas,
) -> Result<review_config::Definition, String> {
    let round: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(round_event.payload.clone()).map_err(|error| error.to_string())?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&round.campaign_manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    manifest.validate()?;
    let pipeline = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|error| error.to_string())?;
    let pipeline = std::str::from_utf8(&pipeline).map_err(|error| error.to_string())?;
    review_config::Definition::from_toml(pipeline).map_err(|error| error.to_string())
}

fn ledger_node_id(round_event: &review_core::RunEvent, cas: &Cas) -> Result<String, String> {
    let definition = pinned_pipeline_definition(round_event, cas)?;
    let mut ledger_nodes = definition
        .nodes
        .iter()
        .filter(|node| node.kind == review_config::NodeKindSpec::Ledger);
    let node = ledger_nodes
        .next()
        .ok_or("pinned Campaign pipeline has no Ledger node")?;
    if ledger_nodes.next().is_some() {
        return Err("pinned Campaign pipeline has multiple Ledger nodes".into());
    }
    Ok(node.id.clone())
}

fn run_report_outcomes(
    event: &review_core::RunEvent,
) -> Result<Option<Vec<review_core::RunNodeReportV2>>, String> {
    match event.event_type {
        EventType::RunReportV2 => Ok(Some(
            serde_json::from_value::<RunReportPayloadV2>(event.payload.clone())
                .map_err(|error| error.to_string())?
                .outcomes,
        )),
        EventType::RunReportV3 => Ok(Some(
            serde_json::from_value::<RunReportPayloadV3>(event.payload.clone())
                .map_err(|error| error.to_string())?
                .outcomes,
        )),
        EventType::RunReportV4 => Ok(Some(
            serde_json::from_value::<RunReportPayloadV4>(event.payload.clone())
                .map_err(|error| error.to_string())?
                .outcomes,
        )),
        EventType::RunReportV5 => Ok(Some(
            serde_json::from_value::<RunReportPayloadV5>(event.payload.clone())
                .map_err(|error| error.to_string())?
                .outcomes,
        )),
        EventType::RunReportV1 => Ok(None),
        _ => Err(format!("{} is not a Run Report", event.event_type)),
    }
}

pub(crate) fn latest_round_evidence(
    events: &[review_core::RunEvent],
    cas: &Cas,
) -> Result<Option<LatestRoundEvidence>, String> {
    let Some(round_event) = events
        .iter()
        .rev()
        .find(|event| event.event_type == EventType::RoundStartedV1)
    else {
        return Ok(None);
    };
    let ledger_node_id = ledger_node_id(round_event, cas)?;
    let ledger_receipt = events.iter().rev().find(|event| {
        event.event_type == EventType::NodeOutputReceiptV1
            && event.node_id.as_deref() == Some(ledger_node_id.as_str())
            && event.causation_id.as_deref() == Some(round_event.event_id.as_str())
    });
    if let Some(receipt) = ledger_receipt {
        let receipt: review_core::NodeOutputReceiptPayloadV1 =
            serde_json::from_value(receipt.payload.clone()).map_err(|error| error.to_string())?;
        let mut count = None;
        for artifact_id in receipt
            .outputs
            .iter()
            .flat_map(|port| port.artifact_ids.iter())
        {
            let value = cas
                .get_json(artifact_id)
                .map_err(|error| error.to_string())?;
            let Ok(envelope) = serde_json::from_value::<review_core::ArtifactEnvelope>(value)
            else {
                continue;
            };
            if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
                continue;
            }
            review_store::validate_envelope(&envelope)?;
            let set: review_core::FindingSetV1 =
                serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
            set.validate()?;
            count = Some(set.findings.len());
            break;
        }
        return Ok(Some(LatestRoundEvidence {
            ledger_production: if count == Some(0) {
                "produced_clean"
            } else {
                "produced_with_findings"
            },
            available_node_results: Vec::new(),
        }));
    }

    let report = events.iter().rev().find(|event| {
        event.event_type.is_run_report()
            && event.causation_id.as_deref() == Some(round_event.event_id.as_str())
    });
    let Some(report) = report else {
        return Ok(None);
    };
    let Some(outcomes) = run_report_outcomes(report)? else {
        return Ok(None);
    };
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome.node == ledger_node_id)
        .ok_or("Run Report has no outcome for the pinned Ledger node")?;
    let ledger_production = match outcome.outcome {
        review_core::RunNodeOutcomeV2::Suppressed {
            reason: review_core::RunSuppressionReasonV2::UpstreamMissing,
        } => "not_produced_upstream_missing",
        review_core::RunNodeOutcomeV2::Suppressed {
            reason: review_core::RunSuppressionReasonV2::GateBlocked,
        } => "not_produced_gate_blocked",
        review_core::RunNodeOutcomeV2::Failed { .. } => "not_produced_failed",
        review_core::RunNodeOutcomeV2::Completed { .. } => {
            return Err("Ledger completed without a NodeOutputReceipt".into());
        }
    };
    let mut available = Vec::new();
    for event in events.iter().filter(|event| {
        event.event_type == EventType::AttemptAdmittedV1
            && event.causation_id.as_deref() == Some(round_event.event_id.as_str())
    }) {
        let payload: review_core::event::AttemptAdmittedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        if payload.selection != "selected" {
            continue;
        }
        let result_artifact_id = payload
            .result_artifact
            .ok_or("selected Attempt has no result artifact")?;
        let value = cas
            .get_json(&result_artifact_id)
            .map_err(|error| error.to_string())?;
        let reports = value
            .get("reports")
            .or_else(|| value.get("findings"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let findings = reports
            .into_iter()
            .map(|report| {
                serde_json::json!({
                    "severity": report.get("severity").cloned().unwrap_or(serde_json::Value::Null),
                    "title": report.get("title").cloned().unwrap_or(serde_json::Value::Null),
                    "body": report.get("body").cloned().unwrap_or(serde_json::Value::Null),
                    "file": report.get("file").cloned().unwrap_or(serde_json::Value::Null),
                    "line": report.get("line").cloned().unwrap_or(serde_json::Value::Null),
                    "locations": report.get("locations").cloned().unwrap_or(serde_json::Value::Null),
                })
            })
            .collect::<Vec<_>>();
        let severities = findings
            .iter()
            .filter_map(|finding| finding["severity"].as_str().map(str::to_string))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        available.push(AvailableNodeResult {
            node: event
                .node_id
                .clone()
                .ok_or("selected Attempt has no node ID")?,
            attempt_id: event
                .attempt_id
                .clone()
                .ok_or("selected Attempt has no Attempt ID")?,
            result_artifact_id,
            severities,
            spend_tokens: payload.cost_tokens,
            findings,
        });
    }
    available.sort_by(|left, right| {
        (&left.node, &left.attempt_id).cmp(&(&right.node, &right.attempt_id))
    });
    Ok(Some(LatestRoundEvidence {
        ledger_production,
        available_node_results: available,
    }))
}
