//! Read selected evidence only through native Task records, never owner-supplied JSON captures.
use super::*;
use review_core::task::findings::{TASK_REVIEW_FINDINGS_V1, TaskReviewFindingsV1};
use review_core::task::optimization::{OPTIMIZATION_HISTORY_V1, OptimizationHistoryV1};
use review_core::task::optimization_light::*;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceSource {
    schema: String,
    task_id: String,
    revision_id: String,
    artifact: ArtifactEnvelope,
}

pub(crate) fn read(
    repo: &Path,
    state: &Path,
    task: &str,
    id: &str,
) -> Result<EvidenceSource, String> {
    if !review_core::is_digest(id) {
        return Err("Evidence requires a native artifact ID".into());
    }
    let opened = open_task(task, repo, Some(state))?;
    let envelope = opened.cas.get_artifact(id).map_err(|e| e.to_string())?;
    validate_envelope(&envelope)?;
    match envelope.artifact_type.as_str() {
        TASK_REVIEW_FINDINGS_V1 => {
            if !opened
                .task
                .revision
                .provenance
                .input_artifact_ids
                .iter()
                .any(|i| i == id)
            {
                return Err("Findings are not recorded in this Task's provenance".into());
            }
            let record: TaskReviewFindingsV1 =
                serde_json::from_value(envelope.payload.clone()).map_err(|e| e.to_string())?;
            record.validate()?;
            continuation::verify_findings(&opened.cas, &opened.store, &record)?;
        }
        OPTIMIZATION_HISTORY_V1 => {
            if !opened.task.revision.inputs.values().any(|p| {
                p.artifact_type == OPTIMIZATION_HISTORY_V1 && p.artifact_ids.iter().any(|i| i == id)
            }) {
                return Err("History Capture is not a native Task input".into());
            }
            let record: OptimizationHistoryV1 =
                serde_json::from_value(envelope.payload.clone()).map_err(|e| e.to_string())?;
            record.validate()?;
            // Receipt validation preserves exact captured prefixes, gaps and cutoff; a capture
            // attests observations, never causal or timing authority.
        }
        OPTIMIZATION_ADOPTION_OBSERVATION_V1 => {
            let observation = opened
                .task
                .adoption_observations
                .iter()
                .find(|(key, _)| key == id)
                .map(|(_, value)| value)
                .ok_or("Adoption is not recorded by this Task")?;
            if serde_json::to_value(observation).map_err(|e| e.to_string())? != envelope.payload {
                return Err("Adoption differs from its authoritative observation".into());
            }
            observation.validate()?;
            let receipt: OptimizationAdoptionReceiptV1 = artifact(
                &opened.cas,
                &observation.adoption_receipt_id,
                OPTIMIZATION_ADOPTION_RECEIPT_V1,
            )?;
            receipt.validate()?;
            use review_core::task::delivery::TaskDeliveryStatusV1;
            let prepared = opened.task.deliveries.iter().any(|(id, d)| {
                id == &receipt.delivery_record_id
                    && d.status == TaskDeliveryStatusV1::Prepared
                    && d.source_snapshot_id == receipt.source_snapshot_id
                    && d.derived_snapshot_id == receipt.delivered_snapshot_id
            });
            let mut delivered = false;
            for (id, record) in &opened.task.deliveries {
                if record.status == TaskDeliveryStatusV1::Delivered {
                    let delivery = opened.cas.get_artifact(id).map_err(|e| e.to_string())?;
                    validate_envelope(&delivery)?;
                    delivered |= delivery
                        .input_artifacts
                        .contains(&observation.adoption_receipt_id)
                        && record.source_snapshot_id == receipt.source_snapshot_id
                        && record.derived_snapshot_id == receipt.delivered_snapshot_id;
                }
            }
            if receipt.task_id != task || !prepared || !delivered {
                return Err(
                    "Adoption lacks its authoritative prepared and completed delivery".into(),
                );
            }
            let commit = opened
                .cas
                .get_json(&observation.commit_snapshot_id)
                .map_err(|e| e.to_string())?;
            if commit["schema"] != "af.optimization-adoption-commit/1"
                || commit["content_digest"] != observation.commit_tree_id
            {
                return Err("Adoption commit evidence differs".into());
            }
        }
        _ => return Err("Unsupported native research artifact".into()),
    }
    let source = EvidenceSource {
        schema: "af/scenario-evidence-source@1".into(),
        task_id: task.into(),
        revision_id: opened.task.revision_id,
        artifact: envelope,
    };
    if serde_json::to_vec(&source)
        .map_err(|e| e.to_string())?
        .len()
        > 4 * 1024 * 1024
    {
        return Err("Selected native evidence exceeds 4 MiB; narrow the capture".into());
    }
    Ok(source)
}
