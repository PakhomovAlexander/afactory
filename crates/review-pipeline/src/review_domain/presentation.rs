//! Read-only presentation of already selected Review facts. This reader never selects an
//! Attempt, changes its charge, or derives authority from a bare CAS artifact.

use super::*;
use review_core::RunEvent;
use review_core::task::campaign_review::*;

impl ReviewDomainState<'_> {
    pub(crate) fn selected_attempt_evidence(&self) -> Result<Vec<crate::AttemptEvidence>, String> {
        let events = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|e| e.to_string())?;
        selected_attempt_evidence(self.cas, &events, &self.authority.round_event_id)
    }
}

/// Events are the verified durable Campaign prefix; exact Round causation excludes prior
/// epochs. Selections are ordered by node, then Attempt.
fn selected_attempt_evidence(
    cas: &Cas,
    events: &[RunEvent],
    round_event_id: &str,
) -> Result<Vec<crate::AttemptEvidence>, String> {
    let mut evidence = Vec::new();
    for event in events.iter().filter(|event| {
        event.event_type == EventType::TaskReviewResultSelectedV1
            && event.causation_id.as_deref() == Some(round_event_id)
    }) {
        let selection: TaskReviewResultSelectedV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        selection.validate()?;
        evidence.push(task_evidence(cas, event, &selection)?);
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
) -> Result<crate::AttemptEvidence, String> {
    let frame = cas
        .get_artifact(&selected.provenance_artifact_id)
        .map_err(|error| error.to_string())?;
    if frame.artifact_type != TASK_REVIEW_ATTEMPT_PROVENANCE_V2 {
        return Err("selected Task Attempt has another provenance type".into());
    }
    let provenance: TaskReviewAttemptProvenanceV2 =
        serde_json::from_value(frame.payload).map_err(|e| e.to_string())?;
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
        .map(|id| review_runner::task::usage::read_task_usage_exact(cas, id))
        .transpose()?
        .unwrap_or_else(|| review_core::task::usage::TaskTokenUsageV3::charge_only(cost_tokens));
    if usage.chargeable_tokens.get() != cost_tokens {
        return Err("selected Task Attempt provenance contradicts its usage".into());
    }
    Ok(crate::AttemptEvidence {
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

    const ROUND: &str = "rrrrrrrrrrrrrrrrrrrrrrrrr1";
    const PRIOR_ROUND: &str = "rrrrrrrrrrrrrrrrrrrrrrrrr0";

    fn manifest() -> ContextManifest {
        let mut manifest = ContextManifest::default();
        manifest.record("prompt", "fixture", None, None, 17);
        manifest.finish(17);
        manifest
    }

    /// One Task selection of `node` in `round`, with its typed provenance and exact context.
    fn selection(cas: &Cas, node: &str, attempt_id: &str, round: &str, charged: u128) -> RunEvent {
        let blob = |label: &str| cas.put(format!("{node}:{label}").as_bytes()).unwrap();
        let producer = Producer::Attempt {
            run_id: "review".into(),
            node_id: node.into(),
            attempt_id: attempt_id.into(),
        };
        let context = TaskReviewContextV1 {
            campaign_id: "review".into(),
            round_event_id: round.into(),
            invocation_event_id: "i".repeat(26),
            review_node: node.into(),
            subject_id: blob("subject"),
            campaign_manifest_id: blob("manifest"),
            task_invocation_id: blob("invocation"),
            attempt_id: attempt_id.into(),
            reviewer_inputs_id: blob("inputs"),
            rendered_input_id: blob("rendered"),
            context_manifest_id: cas
                .put_json(&serde_json::to_value(manifest()).unwrap())
                .unwrap(),
        };
        let (context_id, _) = cas
            .put_artifact(
                TASK_REVIEW_CONTEXT_V1,
                producer.clone(),
                context
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(&context).unwrap(),
            )
            .unwrap();
        let provenance = TaskReviewAttemptProvenanceV2 {
            context_id: context_id.clone(),
            task_invocation_id: context.task_invocation_id.clone(),
            attempt_id: attempt_id.into(),
            review_node: node.into(),
            result_artifact_id: blob("result"),
            mutations_artifact_id: blob("mutations"),
            raw_artifact_id: blob("raw"),
            charged_tokens: charged.into(),
            usage_id: None,
        };
        let (provenance_id, _) = cas
            .put_artifact(
                TASK_REVIEW_ATTEMPT_PROVENANCE_V2,
                producer,
                provenance
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(&provenance).unwrap(),
            )
            .unwrap();
        let selected = TaskReviewResultSelectedV1 {
            task_id: "review".into(),
            task_revision_id: blob("revision"),
            plan_id: blob("plan"),
            task_node: node.into(),
            invocation_id: context.task_invocation_id,
            output_id: blob("output"),
            context_id,
            result_envelope_id: blob("result-envelope"),
            metadata_envelope_id: blob("metadata-envelope"),
            result_artifact_id: provenance.result_artifact_id,
            provenance_artifact_id: provenance_id,
        };
        RunEvent {
            event_id: "event".into(),
            run_id: "review".into(),
            sequence: 1,
            event_type: EventType::TaskReviewResultSelectedV1,
            occurred_at: "time".into(),
            node_id: Some(node.into()),
            attempt_id: Some(attempt_id.into()),
            causation_id: Some(round.into()),
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(selected).unwrap(),
        }
    }

    #[test]
    fn selections_are_filtered_to_the_round_and_sorted_by_node_then_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let zeta = "z".repeat(26);
        let alpha = "a".repeat(26);
        let events = vec![
            selection(&cas, "zeta", &zeta, ROUND, 17),
            selection(&cas, "alpha", &alpha, ROUND, 5),
            selection(&cas, "alpha", &"p".repeat(26), PRIOR_ROUND, 9),
        ];

        let evidence = selected_attempt_evidence(&cas, &events, ROUND).unwrap();
        let expected = |node: &str, attempt_id: &str, charged: u128| crate::AttemptEvidence {
            node: node.into(),
            attempt_id: attempt_id.into(),
            cost_tokens: charged,
            usage: review_core::task::usage::TaskTokenUsageV3::charge_only(charged),
            context_manifest: manifest(),
            raw_artifact: cas.put(format!("{node}:raw").as_bytes()).unwrap(),
            result_artifact: cas.put(format!("{node}:result").as_bytes()).unwrap(),
        };
        assert_eq!(
            evidence,
            [expected("alpha", &alpha, 5), expected("zeta", &zeta, 17)],
            "a prior Round's selection never counts, and order is node then Attempt"
        );

        let mut contradicted = events;
        contradicted[0].attempt_id = Some("x".repeat(26));
        assert!(
            selected_attempt_evidence(&cas, &contradicted, ROUND)
                .unwrap_err()
                .contains("contradicts its selection or context")
        );
    }
}
