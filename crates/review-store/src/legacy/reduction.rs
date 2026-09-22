//! Pure canonical Review reduction, shared by the historical Campaign and Task domain hosts.
//! No Store, Attempt budget or scheduler is created here. The caller admits/persists the result.
use super::*;

#[derive(Debug, Clone)]
pub struct PreparedReviewReduction {
    pub reduction: CanonicalReduction,
    pub ledger: Ledger,
    pub events: Vec<NewEvent>,
}

/// Task reduction keeps the selected flattened Worker address. Logical
/// reviewer names remain finding/demand sources; they never manufacture an Attempt producer.
pub fn prepare_canonical_task_review(
    cas: &Cas,
    run_id: &str,
    ledger: &Ledger,
    stages: &[CanonicalStage<'_>],
) -> Result<PreparedReviewReduction, StoreError> {
    validate_stages(ledger, stages)?;
    let mut prepared = prepare_canonical_inputs(run_id, ledger, stages)?;
    for (stage, item) in stages.iter().zip(&mut prepared) {
        let artifact: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(stage.result_artifact_id)
                .map_err(|e| StoreError::Artifact(e.to_string()))?,
        )?;
        crate::validate_envelope(&artifact).map_err(|e| StoreError::Conflict(e.to_string()))?;
        if stage.result_contract != ReviewerResultContract::V2
            || artifact.artifact_id != stage.result_artifact_id
            || artifact.artifact_type != review_core::contract::REVIEWER_RESULT_V2
            || artifact.subject_snapshot_id.as_deref() != Some(stage.subject_snapshot_id)
            || artifact.input_artifacts != stage.input_artifacts
            || !matches!(&artifact.producer, Producer::Attempt { run_id: run, attempt_id, .. }
                if run == run_id && attempt_id == stage.attempt_id)
        {
            return Err(StoreError::Conflict(
                "Task canonical result lost its exact selected Attempt provenance".into(),
            ));
        }
        if selected_task_stage(&artifact.payload)? != *stage.stage {
            return Err(StoreError::Conflict(
                "Task reduction changed the selected result payload".into(),
            ));
        }
        item.provenance
            .as_mut()
            .expect("canonical stage provenance")
            .producer = artifact.producer;
    }
    prepare_review_outputs(cas, run_id, ledger, &prepared)
}

// Compare the existing typed stage semantics after strict wire validation. Canonical JSON
// stores 1.0 as 1, and the contract permits omitted nullable report fields. Neither changes
// the selected result or authorizes changing its producer, references, or actual field values.
fn selected_task_stage(payload: &serde_json::Value) -> Result<LegacyStageOutput, StoreError> {
    review_core::validate_reviewer_result_v2(payload).map_err(StoreError::Conflict)?;
    let mut decoded = payload.clone();
    let object = decoded
        .as_object_mut()
        .expect("validated result is an object");
    let reports = object.remove("reports").expect("validated result reports");
    object.insert("findings".into(), reports);
    let mut dispositions = object
        .remove("dispositions")
        .expect("validated result dispositions");
    for entry in dispositions.as_array_mut().expect("validated dispositions") {
        let entry = entry.as_object_mut().expect("validated disposition");
        let id = entry
            .remove("finding_id")
            .expect("validated disposition ID");
        entry.insert("fp".into(), id);
    }
    object.insert("disputes".into(), dispositions);
    Ok(serde_json::from_value(decoded)?)
}

fn validate_stages(ledger: &Ledger, stages: &[CanonicalStage<'_>]) -> Result<(), StoreError> {
    if stages.is_empty()
        || stages.len() > 64
        || stages
            .iter()
            .map(|s| s.source)
            .collect::<BTreeSet<_>>()
            .len()
            != stages.len()
        || stages.iter().any(|s| {
            Some(s.subject_id) != ledger.active_subject_id()
                || Some(s.subject_snapshot_id) != ledger.active_head_snapshot_id()
        })
    {
        return Err(StoreError::Conflict(
            "Task review reduction needs unique current-Subject stages".into(),
        ));
    }
    Ok(())
}

pub(super) fn prepare_canonical_inputs(
    run_id: &str,
    ledger: &Ledger,
    stages: &[CanonicalStage<'_>],
) -> Result<Vec<PreparedStage>, StoreError> {
    if ledger.finding_identity_policy() != Some(CANONICAL_FINDING_IDENTITY_POLICY) {
        return Err(StoreError::Conflict(format!(
            "canonical report ingestion disagrees with Campaign policy `{}`",
            ledger.finding_identity_policy().unwrap_or("unavailable")
        )));
    }
    let mut prepared = Vec::with_capacity(stages.len());
    for stage in stages {
        let reports = stage
            .stage
            .findings
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, finding)| {
                finding.into_report(index).map_err(|reason| {
                    StoreError::Conflict(format!(
                        "{} finding {index} violates FindingReport@1: {reason}",
                        stage.source
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut input_artifacts = Vec::with_capacity(stage.input_artifacts.len() + 1);
        input_artifacts.push(stage.result_artifact_id.to_string());
        input_artifacts.extend(stage.input_artifacts.iter().cloned());
        let mut seen = BTreeSet::new();
        input_artifacts.retain(|id| seen.insert(id.clone()));
        prepared.push(PreparedStage {
            source: stage.source.to_string(),
            demand_requirement: stage.demand_requirement,
            reports,
            demands: stage.stage.benchmark_demands.clone(),
            disputes: stage.stage.disputes.clone(),
            provenance: Some(ReportProvenance {
                producer: Producer::Attempt {
                    run_id: run_id.to_owned(),
                    node_id: stage.source.to_string(),
                    attempt_id: stage.attempt_id.to_string(),
                },
                input_artifacts,
                subject_snapshot_id: stage.subject_snapshot_id.to_string(),
                subject_id: stage.subject_id.to_string(),
            }),
            result_contract: stage.result_contract,
        });
    }
    Ok(prepared)
}

pub(super) fn prepare_review_outputs(
    cas: &Cas,
    run_id: &str,
    ledger: &Ledger,
    stages: &[PreparedStage],
) -> Result<PreparedReviewReduction, StoreError> {
    let round = ledger.round;
    let mut projected = (*ledger).clone();
    let mut events = Vec::new();
    let existing_reports: BTreeSet<(&str, &str, u32, &str)> = ledger
        .findings()
        .into_iter()
        .flat_map(|finding| {
            finding.reports.iter().map(|report| {
                (
                    finding.key.as_str(),
                    report.source.as_str(),
                    report.round,
                    report.report_id.as_str(),
                )
            })
        })
        .collect();
    let mut pending_reports: BTreeSet<(String, String, u32, String)> = BTreeSet::new();
    let mut selected_report_ids = Vec::new();
    let mut report_ids_by_source = std::collections::BTreeMap::new();
    let mut relation_ids = Vec::new();
    let mut input_artifact_ids = Vec::new();
    let mut selected_demand_artifact_ids = Vec::new();
    let mut demand_input_artifact_ids = Vec::new();
    let mut pending_demands = BTreeSet::new();
    let mut pending_occurrences: std::collections::BTreeMap<(String, String), String> =
        std::collections::BTreeMap::new();

    for stage in stages {
        let source = stage.source.as_str();
        if let Some(provenance) = &stage.provenance {
            for demand in &stage.demands {
                let demand_id = canonical_demand_id(source, demand);
                if ledger.demand(&demand_id).is_some() || !pending_demands.insert(demand_id.clone())
                {
                    continue;
                }
                let payload = review_core::DemandV1 {
                    demand_id: demand_id.clone(),
                    claim: demand.claim.clone(),
                    why: demand.why.clone(),
                    suggested_method: demand.suggested_method.clone(),
                    source: source.to_string(),
                    requirement: stage.demand_requirement,
                    round,
                    subject_id: provenance.subject_id.clone(),
                };
                payload.validate().map_err(StoreError::Conflict)?;
                let (record_id, envelope) = cas
                    .put_artifact(
                        review_core::contract::DEMAND_V1,
                        provenance.producer.clone(),
                        provenance.input_artifacts.clone(),
                        Some(provenance.subject_snapshot_id.clone()),
                        serde_json::to_value(payload)?,
                    )
                    .map_err(|error| StoreError::Conflict(error.to_string()))?;
                let event = NewEvent::new(
                    crate::ledger::EVENT_DEMAND_RECORDED,
                    serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
                        artifact_id: record_id.clone(),
                    })?,
                )
                .correlating(demand_id)
                .referencing(vec![record_id.clone()]);
                apply_candidate(&mut projected, &event, cas)?;
                events.push(event);
                selected_demand_artifact_ids.push(envelope.artifact_id);
                demand_input_artifact_ids.push(record_id);
            }
        }
        let mut reports = stage.reports.clone();
        if let Some(provenance) = &stage.provenance {
            for dispute in &stage.disputes {
                let corroborates = match stage.result_contract {
                    ReviewerResultContract::V1 => dispute.position.trim() == "confirm",
                    ReviewerResultContract::V2 => dispute.position.trim() == "corroborate",
                };
                if !corroborates {
                    continue;
                }
                let key = dispute.fp.trim();
                let Some(finding) = ledger.get(key) else {
                    // A model may mistype a long canonical ID. Like an unresolvable refute,
                    // it carries no safe authority and must not discard the other selected
                    // reviewers' evidence.
                    continue;
                };
                let mut replayed = false;
                for existing in finding.reports.iter().filter(|report| {
                    report.source == source
                        && report.round == round
                        && report.relations.iter().any(|relation| {
                            relation.kind == review_core::RelationKind::Corroborates
                                && relation.target.kind
                                    == review_core::finding::ClaimTargetKind::Finding
                                && relation.target.id == key
                        })
                }) {
                    let value = cas.get_json(&existing.report_id).map_err(|error| {
                        StoreError::Artifact(format!(
                            "corroborating Report {} is unreadable during replay: {error}",
                            existing.report_id
                        ))
                    })?;
                    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value)
                        .map_err(|error| {
                            StoreError::Artifact(format!(
                                "corroborating Report {} is not an ArtifactEnvelope: {error}",
                                existing.report_id
                            ))
                        })?;
                    crate::canonical::validate_envelope(&envelope).map_err(|error| {
                        StoreError::Artifact(format!(
                            "corroborating Report {}: {error}",
                            existing.report_id
                        ))
                    })?;
                    if envelope.artifact_type != review_core::contract::FINDING_REPORT_V1 {
                        return Err(StoreError::Artifact(format!(
                            "corroborating Report {} has type {}",
                            existing.report_id, envelope.artifact_type
                        )));
                    }
                    if envelope.producer != provenance.producer
                        || envelope.input_artifacts != provenance.input_artifacts
                        || envelope.subject_snapshot_id.as_deref()
                            != Some(provenance.subject_snapshot_id.as_str())
                    {
                        continue;
                    }
                    reports.push(serde_json::from_value(envelope.payload).map_err(|error| {
                        StoreError::Artifact(format!(
                            "corroborating Report {} is not FindingReport@1: {error}",
                            existing.report_id
                        ))
                    })?);
                    replayed = true;
                    break;
                }
                if replayed {
                    continue;
                }
                let Some(confidence) = finding.confidence else {
                    continue;
                };
                let fix = finding.fix.clone();
                let locations =
                    if finding.identity_file == review_core::legacy::CHANGE_WIDE_SENTINEL {
                        Vec::new()
                    } else if review_core::is_valid_repo_path(&finding.identity_file) {
                        let line = match finding.identity_line.map(u32::try_from).transpose() {
                            Ok(line) => line,
                            Err(_) => continue,
                        };
                        vec![review_core::Location {
                            path: finding.identity_file.clone(),
                            line,
                            end_line: None,
                        }]
                    } else {
                        continue;
                    };
                reports.push(FindingReport {
                    title: finding.title.clone(),
                    severity: finding.severity,
                    locations,
                    body: finding.body.clone(),
                    fix,
                    confidence,
                    failure_trace: None,
                    rule_id: None,
                    occurrence_key: None,
                    relations: vec![review_core::Relation {
                        kind: review_core::RelationKind::Corroborates,
                        target: review_core::finding::RelationTarget {
                            kind: review_core::finding::ClaimTargetKind::Finding,
                            id: key.to_string(),
                        },
                        reason: Some(dispute.reason.clone()),
                    }],
                });
            }
        }
        let mut published = Vec::with_capacity(reports.len());
        for report in &reports {
            let (report_id, semantic_id) = match &stage.provenance {
                Some(provenance) => {
                    let (record_id, envelope) = cas
                        .put_artifact(
                            review_core::contract::FINDING_REPORT_V1,
                            provenance.producer.clone(),
                            provenance.input_artifacts.clone(),
                            Some(provenance.subject_snapshot_id.clone()),
                            serde_json::to_value(report)?,
                        )
                        .map_err(|error| StoreError::Conflict(error.to_string()))?;
                    (record_id, envelope.artifact_id)
                }
                None => {
                    let value = serde_json::to_value(report)?;
                    let record_id = cas
                        .put_json(&value)
                        .map_err(|error| StoreError::Conflict(error.to_string()))?;
                    (record_id.clone(), record_id)
                }
            };
            published.push((report, report_id, semantic_id));
        }
        let keys = match &stage.provenance {
            Some(_) => canonical_stage_keys(&published, ledger, &pending_occurrences)?,
            None => published
                .iter()
                .map(|(report, _, _)| {
                    legacy_fingerprint(report_identity_path(report), &report.title)
                })
                .collect(),
        };

        for ((report, report_id, semantic_id), key) in published.into_iter().zip(keys.into_iter()) {
            if let (Some(rule_id), Some(occurrence_key)) = (&report.rule_id, &report.occurrence_key)
            {
                pending_occurrences.insert((rule_id.clone(), occurrence_key.clone()), key.clone());
            }

            // The report is an immutable artifact; the event references it. A duplicate from
            // another reviewer is stored too, so every reviewer's evidence stays attached.
            let report_identity = (key.clone(), source.to_string(), round, report_id.clone());

            if stage.provenance.is_some() {
                selected_report_ids.push(semantic_id.clone());
                report_ids_by_source
                    .entry(source.to_string())
                    .or_insert_with(Vec::new)
                    .push(semantic_id.clone());
                input_artifact_ids.push(report_id.clone());
            }

            if existing_reports.contains(&(key.as_str(), source, round, report_id.as_str()))
                || pending_reports.contains(&report_identity)
            {
                continue;
            }
            pending_reports.insert(report_identity);

            let payload = json!({
                "key": key,
                "round": round,
                "source": source,
                "report_id": report_id,
            });
            let pending_challenge = projected
                .resolution_challenge_for_report(&key, report.severity)
                .and_then(|kind| {
                    projected
                        .resolution(&key)
                        .cloned()
                        .map(|resolution| (kind, resolution))
                });
            let event = NewEvent::new(EVENT_FINDING_REPORTED, payload)
                .node(source)
                .correlating(key.clone())
                .referencing(vec![report_id.clone()]);
            apply_candidate(&mut projected, &event, cas)?;
            events.push(event);

            if let Some((kind, resolution)) = pending_challenge
                && projected
                    .get(&key)
                    .and_then(|finding| finding.reports.last())
                    .is_some_and(|attached| attached.scope != Some(ReportScope::Out))
            {
                let subject_id = projected
                    .active_subject_id()
                    .ok_or_else(|| StoreError::Conflict("Campaign has no active Subject".into()))?
                    .to_string();
                let root = projected
                    .finding_view(&key)
                    .expect("reported Finding has a visible view")
                    .key;
                let challenge = review_core::ResolutionChallengeV1 {
                    finding_id: root.clone(),
                    resolution_id: resolution.artifact_id.clone(),
                    subject_id,
                    kind,
                    actor: "review.kernel/resolution-policy@1".into(),
                    reason: format!(
                        "in-scope Report {} challenged the scoped {:?} Resolution",
                        semantic_id, resolution.resolution.outcome
                    ),
                    evidence_ids: vec![semantic_id],
                };
                challenge.validate().map_err(StoreError::Conflict)?;
                let (record_id, _) = cas
                    .put_artifact(
                        review_core::contract::RESOLUTION_CHALLENGE_V1,
                        Producer::KernelOperation {
                            run_id: run_id.to_owned(),
                            node_id: None,
                            operation_id: format!("automatic-resolution-challenge:{root}:{kind:?}"),
                        },
                        vec![resolution.record_id, report_id],
                        projected.active_head_snapshot_id().map(str::to_string),
                        serde_json::to_value(challenge)?,
                    )
                    .map_err(|error| StoreError::Conflict(error.to_string()))?;
                let challenge_event = NewEvent::new(
                    EventType::FindingResolutionChallengedV1,
                    serde_json::to_value(review_core::RecordedArtifactPayloadV1 {
                        artifact_id: record_id.clone(),
                    })?,
                )
                .correlating(root)
                .referencing(vec![record_id]);
                apply_candidate(&mut projected, &challenge_event, cas)?;
                events.push(challenge_event);
            }
        }

        if stage.result_contract == ReviewerResultContract::V2 {
            let provenance = stage.provenance.as_ref().ok_or_else(|| {
                StoreError::Conflict(
                    "ReviewerResult@2 dispositions require canonical Attempt provenance".into(),
                )
            })?;
            for disposition in &stage.disputes {
                let finding_id = disposition.fp.trim();
                if ledger.get(finding_id).is_none() {
                    return Err(StoreError::Conflict(format!(
                        "{source} disposition names Finding `{finding_id}` outside its assigned prior Finding Set"
                    )));
                }
                let position = match disposition.position.trim() {
                    "corroborate" => FindingDispositionPosition::Corroborate,
                    "not_reproduced" => FindingDispositionPosition::NotReproduced,
                    "dispute" => FindingDispositionPosition::Dispute,
                    _ => {
                        return Err(StoreError::Conflict(format!(
                            "{source} disposition has an invalid position"
                        )));
                    }
                };
                let payload = FindingDispositionV1 {
                    finding_id: finding_id.to_string(),
                    source: source.to_string(),
                    position,
                    reason: disposition.reason.clone(),
                    round,
                    subject_id: provenance.subject_id.clone(),
                };
                payload.validate().map_err(StoreError::Conflict)?;
                let (record_id, envelope) = cas
                    .put_artifact(
                        review_core::contract::FINDING_DISPOSITION_V1,
                        provenance.producer.clone(),
                        provenance.input_artifacts.clone(),
                        Some(provenance.subject_snapshot_id.clone()),
                        serde_json::to_value(payload)?,
                    )
                    .map_err(|error| StoreError::Conflict(error.to_string()))?;
                relation_ids.push(envelope.artifact_id);
                input_artifact_ids.push(record_id.clone());

                if position != FindingDispositionPosition::Dispute {
                    continue;
                }
                let contestable = matches!(
                    projected.get(finding_id).map(|finding| finding.status),
                    Some(Status::Open | Status::Fixed)
                );
                if !contestable {
                    continue;
                }
                let payload = json!({
                    "key": finding_id,
                    "status": Status::Contested.as_str(),
                    "note": format!("contested by {source}: {}", disposition.reason),
                    "round": round,
                });
                let event = NewEvent::new(EVENT_FINDING_RESOLVED, payload)
                    .correlating(finding_id.to_string())
                    .referencing(vec![record_id]);
                apply_candidate(&mut projected, &event, cas)?;
                events.push(event);
            }
        }

        // Reviewer disputes are part of the legacy contract the model is asked to answer. A
        // `confirm` above becomes a current, provenance-carrying Report with an explicit
        // corroborates relation. A `refute` on a prior claim's `claim_id` says "I think this
        // is wrong". Fold it:
        // an active claim a reviewer refutes becomes `contested`, which blocks convergence
        // and flags the claim for human adjudication rather than leaving the dispute inert in
        // raw CAS output.
        for dispute in &stage.disputes {
            if stage.result_contract != ReviewerResultContract::V1 {
                continue;
            }
            if dispute.position.trim() != "refute" {
                continue;
            }
            let key = dispute.fp.trim();
            let contestable = matches!(
                projected.get(key).map(|f| f.status),
                Some(Status::Open | Status::Fixed)
            );
            if !contestable {
                continue;
            }
            let payload = json!({
                "key": key,
                "status": Status::Contested.as_str(),
                "note": format!("contested by {source}: {}", dispute.reason),
                "round": round,
            });
            let event = NewEvent::new(EVENT_FINDING_RESOLVED, payload).correlating(key.to_string());
            apply_candidate(&mut projected, &event, cas)?;
            events.push(event);
        }
    }

    let reduction = CanonicalReduction {
        selected_report_ids,
        report_ids_by_source,
        relation_ids,
        selected_demand_artifact_ids,
        input_artifact_ids,
        demand_input_artifact_ids,
        reducer_version: if stages
            .iter()
            .any(|stage| stage.result_contract == ReviewerResultContract::V2)
        {
            review_core::FINDING_REDUCER_VERSION_V2
        } else {
            review_core::FINDING_REDUCER_VERSION
        },
    };
    Ok(PreparedReviewReduction {
        reduction,
        ledger: projected,
        events,
    })
}

#[cfg(test)]
mod task_selected_stage_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selected_stage_preserves_canonical_numbers_optional_fields_and_exact_payload() {
        let mut payload = json!({"verdict":"request-changes","summary":"Review","reports":[{"severity":"major","file":"lib.rs","title":"Missing behavior","body":"Required behavior is absent","fix":"Implement it","confidence":1.0}],"benchmark_demands":[],"dispositions":[]});
        let original = selected_task_stage(&payload).unwrap();
        let stored: serde_json::Value =
            serde_json::from_slice(&crate::canonical::canonicalize(&payload).unwrap()).unwrap();
        assert_eq!(stored["reports"][0]["confidence"], json!(1));
        assert_eq!(selected_task_stage(&stored).unwrap(), original);
        payload["reports"][0]
            .as_object_mut()
            .unwrap()
            .remove("confidence");
        let omitted = selected_task_stage(&payload).unwrap();
        assert_eq!(omitted.findings[0].confidence, None);
        assert_eq!(omitted.findings[0].line, None);
        payload["reports"][0]["confidence"] = json!(null);
        payload["reports"][0]["line"] = json!(null);
        assert_eq!(selected_task_stage(&payload).unwrap(), omitted);
        payload["reports"][0]["title"] = json!("A changed selected claim");
        assert_ne!(selected_task_stage(&payload).unwrap(), omitted);
        payload["reports"][0]["unknown"] = json!(true);
        assert!(selected_task_stage(&payload).is_err());
    }
}
