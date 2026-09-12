//! Read-only presentation of already selected Review facts. This reader never selects an
//! Attempt, changes its charge, or derives authority from a bare CAS artifact.

use super::*;
use review_core::RunEvent;
use review_core::task::review_compat::*;

impl ReviewDomainState<'_> {
    pub(crate) fn selected_attempt_evidence(&self) -> Result<Vec<AttemptEvidence>, String> {
        let events = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?;
        selected_attempt_evidence(self.cas, &events, &self.authority.round_event_id)
    }
}

/// Events are the verified durable Campaign prefix; exact Round causation excludes prior
/// epochs. Historical and common selections retain one ordering and one public field shape.
fn selected_attempt_evidence(
    cas: &Cas,
    events: &[RunEvent],
    round_event_id: &str,
) -> Result<Vec<AttemptEvidence>, String> {
    let mut evidence = Vec::new();
    for event in events.iter().filter(|event| {
        matches!(
            event.event_type,
            EventType::AttemptAdmittedV1 | EventType::TaskReviewResultSelectedV1
        ) && event.causation_id.as_deref() == Some(round_event_id)
    }) {
        let (provenance_id, result_artifact, legacy_cost, selection) = if event.event_type
            == EventType::AttemptAdmittedV1
        {
            let payload: AttemptAdmittedPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
            if payload.selection != "selected" {
                continue;
            }
            (
                payload
                    .provenance_artifact
                    .ok_or("selected Attempt has no provenance artifact")?,
                payload
                    .result_artifact
                    .ok_or("selected Attempt has no result artifact")?,
                Some(payload.cost_tokens),
                None,
            )
        } else {
            let payload: TaskReviewResultSelectedV1 =
                serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
            payload.validate()?;
            (
                payload.provenance_artifact_id.clone(),
                payload.result_artifact_id.clone(),
                None,
                Some(payload),
            )
        };
        let node = event
            .node_id
            .as_ref()
            .ok_or("selected Attempt has no node ID")?;
        let attempt_id = event
            .attempt_id
            .as_ref()
            .ok_or("selected Attempt has no Attempt ID")?;
        let provenance = cas
            .get_json(&provenance_id)
            .map_err(|error| error.to_string())?;
        let item = if let Some(selection) = selection
            .as_ref()
            .filter(|_| provenance.get("type").is_some())
        {
            task_evidence(cas, event, selection, &provenance_id)?
        } else {
            let cost_tokens = legacy_cost
                .or_else(|| provenance["cost_tokens"].as_u64())
                .ok_or("selected Attempt provenance has no cost")?;
            if provenance["node"].as_str() != Some(node.as_str())
                || provenance["attempt"].as_str() != Some(attempt_id.as_str())
                || provenance["cost_tokens"].as_u64() != Some(cost_tokens)
            {
                return Err("selected Attempt provenance contradicts its admission event".into());
            }
            AttemptEvidence {
                node: node.clone(),
                attempt_id: attempt_id.clone(),
                cost_tokens,
                usage: serde_json::from_value(provenance["usage"].clone())
                    .map_err(|error| error.to_string())?,
                context_manifest: serde_json::from_value(provenance["context_manifest"].clone())
                    .map_err(|error| error.to_string())?,
                raw_artifact: provenance["raw"]
                    .as_str()
                    .ok_or("selected Attempt provenance has no raw artifact")?
                    .to_string(),
                result_artifact,
            }
        };
        evidence.push(item);
    }
    evidence.sort_by(|left, right| {
        (&left.node, &left.attempt_id).cmp(&(&right.node, &right.attempt_id))
    });
    Ok(evidence)
}

fn task_evidence(
    cas: &Cas,
    event: &RunEvent,
    selected: &TaskReviewResultSelectedV1,
    provenance_id: &str,
) -> Result<AttemptEvidence, String> {
    let frame = cas
        .get_artifact(provenance_id)
        .map_err(|error| error.to_string())?;
    if frame.artifact_type != TASK_REVIEW_ATTEMPT_PROVENANCE_V1 {
        return Err("selected Task Attempt has another provenance type".into());
    }
    let provenance: TaskReviewAttemptProvenanceV1 =
        serde_json::from_value(frame.payload).map_err(|error| error.to_string())?;
    provenance.validate()?;
    let frame = cas
        .get_artifact(&selected.context_id)
        .map_err(|error| error.to_string())?;
    if frame.artifact_type != TASK_REVIEW_CONTEXT_V1 {
        return Err("selected Task Attempt has another context type".into());
    }
    let context: TaskReviewContextV1 =
        serde_json::from_value(frame.payload).map_err(|error| error.to_string())?;
    context.validate()?;
    if event.node_id.as_deref() != Some(&provenance.review_node)
        || event.attempt_id.as_deref() != Some(&provenance.attempt_id)
        || event.causation_id.as_deref() != Some(&context.round_event_id)
        || event.run_id != context.campaign_id
        || provenance.context_id != selected.context_id
        || provenance.task_invocation_id != selected.invocation_id
        || provenance.result_artifact_id != selected.result_artifact_id
        || provenance.review_node != context.review_node
        || provenance.attempt_id != context.attempt_id
        || provenance.task_invocation_id != context.task_invocation_id
    {
        return Err("selected Task Attempt provenance contradicts its selection or context".into());
    }
    let cost_tokens = provenance.charged_tokens.get();
    let usage = provenance
        .usage_id
        .as_deref()
        .map(|id| review_runner::task::usage::read_task_usage(cas, id))
        .transpose()?
        .unwrap_or_else(|| TokenUsage::charge_only(cost_tokens));
    if usage.chargeable_tokens != cost_tokens {
        return Err("selected Task Attempt provenance contradicts its usage".into());
    }
    Ok(AttemptEvidence {
        node: provenance.review_node,
        attempt_id: provenance.attempt_id,
        cost_tokens,
        usage,
        context_manifest: serde_json::from_value(
            cas.get_json(&context.context_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?,
        raw_artifact: provenance.raw_artifact_id,
        result_artifact: provenance.result_artifact_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_selection_fields_round_filter_and_sort_order_are_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let mut context_manifest = ContextManifest::default();
        context_manifest.record("prompt", "fixture", None, None, 17);
        context_manifest.finish(17);
        let usage = TokenUsage {
            input_tokens: Some(12),
            output_tokens: Some(5),
            cache_read_tokens: Some(3),
            cache_write_tokens: Some(2),
            reasoning_tokens: Some(1),
            chargeable_tokens: 17,
        };
        let expected: Vec<_> = ["alpha", "zeta"]
            .into_iter()
            .map(|node| AttemptEvidence {
                node: node.into(),
                attempt_id: format!("attempt-{node}"),
                cost_tokens: 17,
                usage: usage.clone(),
                context_manifest: context_manifest.clone(),
                raw_artifact: cas.put(node.as_bytes()).unwrap(),
                result_artifact: cas.put(b"result").unwrap(),
            })
            .collect();
        let mut events: Vec<_> = expected.iter().rev().map(|item| {
            let provenance = cas.put_json(&serde_json::json!({
                "node":item.node,"attempt":item.attempt_id,"cost_tokens":item.cost_tokens,
                "usage":item.usage,"context_manifest":item.context_manifest,"raw":item.raw_artifact,
            })).unwrap();
            RunEvent {
                event_id:"event".into(),run_id:"review".into(),sequence:1,
                event_type:EventType::AttemptAdmittedV1,occurred_at:"time".into(),
                node_id:Some(item.node.clone()),attempt_id:Some(item.attempt_id.clone()),
                causation_id:Some("round".into()),correlation_id:None,artifact_refs:vec![provenance.clone()],
                payload:serde_json::json!({"selection":"selected","cost_tokens":item.cost_tokens,"result_artifact":item.result_artifact,"provenance_artifact":provenance}),
            }
        }).collect();
        let mut prior = events[0].clone();
        prior.causation_id = Some("prior-round".into());
        prior.payload = serde_json::Value::Null;
        events.push(prior);
        let mut quarantined = events[0].clone();
        quarantined.payload["selection"] = serde_json::json!("quarantined");
        events.push(quarantined);
        assert_eq!(
            selected_attempt_evidence(&cas, &events, "round").unwrap(),
            expected
        );
        events[0].payload["cost_tokens"] = serde_json::json!(18);
        assert!(
            selected_attempt_evidence(&cas, &events, "round")
                .unwrap_err()
                .contains("contradicts its admission")
        );
    }
}
