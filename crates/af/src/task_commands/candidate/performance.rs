//! Read-only native acceptance and provenance checks shared by dispatch and installed adapters.
use super::*;
use af_scenario::contract::TaskReceipt;
use af_scenario::performance::{Proof, ProofRequest};

fn accepted(
    receipt: &TaskReceipt,
    repo: &Path,
    state: &Path,
    kind: &str,
) -> Result<OpenedTask, String> {
    let opened = open_task(&receipt.task_id, repo, Some(state))?;
    let (result, view) = opened.view()?;
    if opened.task.revision_id != receipt.revision_id
        || opened.task.plan_id.as_deref() != Some(&receipt.plan_id)
        || !matches!(&opened.task.phase, TaskPhaseV1::Finished { result_id } if result_id == &receipt.result_id)
        || result.as_ref() != Some(&receipt.result)
        || receipt.result.execution != TaskExecutionV1::Completed
        || receipt.result.acceptance != TaskAcceptanceV1::Satisfied
        || view.state.outcome != Some(af_contracts::task::state::TaskOutcomeV1::Verified)
        || opened.task.revision.kind != kind
    {
        return Err(format!(
            "Publication requires exact native accepted {kind} Task"
        ));
    }
    if kind != "report" {
        let plan: ExecutionPlanV3 = artifact(&opened.cas, &receipt.plan_id, EXECUTION_PLAN_V4)?;
        super::super::scenario_template::performance_roles(&plan, kind)?;
        let selected = select(&opened, None)?;
        let findings = super::super::findings::document(&opened, None)?;
        let round = findings
            .round
            .as_ref()
            .ok_or("Publication requires a recorded independent Review Round")?;
        if !round.complete
            || !round.missing_reviewers.is_empty()
            || round.snapshot_id != selected.snapshot_id
            || round.conclusion != af_contracts::task::review::ReviewConclusionV1::Pass
            || round.reviewers.len() != 2
            || !["bugs", "correctness"].iter().all(|name| {
                round
                    .reviewers
                    .iter()
                    .any(|r| r.reviewer == *name && r.result_id.is_some())
            })
            || !findings.evaluation.as_ref().is_some_and(|e| {
                e.outcome == af_contracts::task::pipeline::ReceiptOutcomeV1::Passed
            })
        {
            return Err(
                "Publication needs both reviewers and that exact Review Round's passing evaluator"
                    .into(),
            );
        }
    }
    Ok(opened)
}

fn source_tree(opened: &OpenedTask, port: &ArtifactInputV1) -> Result<String, String> {
    let [id] = port.artifact_ids.as_slice() else {
        return Err("Publication needs one exact SourceTree artifact".into());
    };
    let snapshot = input_bindings::source_tree_snapshot(&opened.cas, id)?;
    if port.artifact_type != SOURCE_TREE_V1
        || port.cardinality != PortCardinality::One
        || port.snapshot_id.as_deref() != Some(&snapshot)
    {
        return Err("Publication SourceTree differs from its native artifact".into());
    }
    Ok(snapshot)
}
fn source(opened: &OpenedTask) -> Result<String, String> {
    source_tree(
        opened,
        opened
            .task
            .revision
            .inputs
            .get("source")
            .ok_or("Publication Task lacks source")?,
    )
}

/// Require both the bound Snapshot and exact native predecessor Task/result/artifact receipt.
fn predecessor(
    opened: &OpenedTask,
    receipt: &TaskReceipt,
    expected: &str,
) -> Result<String, String> {
    let id = source(opened)?;
    let bindings = recorded_input_bindings(&opened.cas, &opened.task.revision)?
        .ok_or("Publication source lacks native input bindings")?;
    let bound = bindings
        .bindings
        .get("source")
        .ok_or("Publication source binding absent")?;
    let reference = bound
        .task
        .as_ref()
        .ok_or("Publication source was not bound from a Task")?;
    let port = receipt
        .result
        .outputs
        .get("snapshot")
        .ok_or("Publication predecessor lacks Snapshot")?;
    if reference.task_revision_id != receipt.revision_id
        || reference.acceptance != TaskAcceptanceV1::Satisfied
        || reference.domain_conclusion != receipt.result.domain_conclusion
        || reference.task_id != receipt.task_id
        || reference.result_id != receipt.result_id
        || reference.port != "snapshot"
        || bound.snapshot_id.as_deref() != Some(expected)
        || bound.rerooted_snapshot_id.as_deref().unwrap_or(expected) != id
        || port.artifact_ids.as_slice() != [bound.artifact_id.clone()]
        || input_bindings::source_tree_snapshot(&opened.cas, &bound.artifact_id)? != expected
    {
        return Err("Publication source differs from exact predecessor receipt".into());
    }
    let source_port = &opened.task.revision.inputs["source"];
    let resolved = bound
        .resolved_artifact_id
        .as_ref()
        .unwrap_or(&bound.artifact_id);
    if source_port.artifact_type != SOURCE_TREE_V1
        || source_port.artifact_ids.as_slice() != [resolved.clone()]
        || input_bindings::source_tree_snapshot(&opened.cas, resolved)? != id
        || !bound.also.is_empty()
    {
        return Err("Publication source differs from its native resolved artifact".into());
    }
    super::super::scenario_template::experiment_source_matches(&opened.cas, &id, expected)?;
    Ok(id)
}

pub(crate) fn proof(request: &ProofRequest, repo: &Path, state: &Path) -> Result<Proof, String> {
    let completion = request.validate()?;
    let implementation = accepted(&request.candidate, repo, state, "implement")?;
    let verification = accepted(&request.verification, repo, state, "verification")?;
    let report = accepted(&request.report, repo, state, "report")?;
    let selected = select(&implementation, None)?;
    let candidate_port = request
        .candidate
        .result
        .outputs
        .get("snapshot")
        .ok_or("Publication candidate output absent")?;
    if source_tree(&implementation, candidate_port)? != selected.snapshot_id {
        return Err("Publication candidate artifact differs from its native Snapshot".into());
    }
    let source_id = source(&implementation)?;
    let (baseline, _) = af_source_git::task::read_snapshot(&implementation.cas, &source_id)?;
    let origin = af_source_git::task::read_origin(&implementation.cas, &baseline.origin_id)?;
    if !selected.verified
        || selected.snapshot_id != request.evidence.revision
        || completion["baseline_snapshot_id"] != source_id
        || origin.source_revision() != Some(request.source_commit.as_str())
        || baseline.content_digest != request.source_content_digest
        || baseline.parent_snapshot_id.is_some()
    {
        return Err("Publication measurement/candidate/source/base authority differs".into());
    }
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let repository = af_source_git::Repo::open(repo, scratch.path());
    if repository.repository_id().map_err(|e| e.to_string())? != origin.repository_id() {
        return Err("Publication Task belongs to a different repository".into());
    }
    let (_, old) = af_source_git::task::read_snapshot(&implementation.cas, &source_id)?;
    let (_, new) = af_source_git::task::read_snapshot(&implementation.cas, &selected.snapshot_id)?;
    let old_controls = old
        .entries
        .iter()
        .filter(|e| e.path == ".github" || e.path.starts_with(".github/"));
    let new_controls = new
        .entries
        .iter()
        .filter(|e| e.path == ".github" || e.path.starts_with(".github/"));
    if !af_source_git::task::descends_from(&implementation.cas, &selected.snapshot_id, &source_id)?
        || old.content_digest() == new.content_digest()
        || !old_controls.eq(new_controls)
    {
        return Err("Publication requires a changed candidate with no .github changes".into());
    }
    let verified_source = predecessor(&verification, &request.candidate, &selected.snapshot_id)?;
    let verified = select(&verification, None)?;
    if !verified.verified || verified.snapshot_id != verified_source {
        return Err("Independent verification changed its exact source".into());
    }
    let report_source = predecessor(&report, &request.verification, &verified_source)?;
    // Both independent verification and final reporting must consume this exact measurement.
    for opened in [&verification, &report] {
        let requirements = opened
            .task
            .revision
            .inputs
            .get("requirements")
            .ok_or("Publication Task requirements absent")?;
        let [requirements_id] = requirements.artifact_ids.as_slice() else {
            return Err("Publication Task requirements ambiguous".into());
        };
        let requirements = opened
            .cas
            .get_artifact(requirements_id)
            .map_err(|e| e.to_string())?;
        if requirements.payload["specification"]["scenario_evidence"]
            != serde_json::to_value(&request.evidence).map_err(|e| e.to_string())?
        {
            return Err("Independent verification or final report did not consume the exact native comparison".into());
        }
    }
    use af_contracts::task::document::{DOCUMENT_V1, DocumentV1};
    let port = request
        .report
        .result
        .outputs
        .get("report")
        .ok_or("Accepted final report absent")?;
    let [id] = port.artifact_ids.as_slice() else {
        return Err("Final report is ambiguous".into());
    };
    let envelope = report.cas.get_artifact(id).map_err(|e| e.to_string())?;
    validate_envelope(&envelope)?;
    if envelope.artifact_type != DOCUMENT_V1
        || port.artifact_type != DOCUMENT_V1
        || port.cardinality != PortCardinality::One
        || port.snapshot_id.as_deref() != Some(&report_source)
        || envelope.subject_snapshot_id.as_deref() != Some(&report_source)
    {
        return Err("Final report Document source differs".into());
    }
    let document: DocumentV1 =
        serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
    document.validate()?;
    let tasks = vec![
        request.candidate.task_id.clone(),
        request.verification.task_id.clone(),
        request.report.task_id.clone(),
    ];
    let task_report = super::super::task_report::read(state, &tasks)?;
    let unresolved = super::super::task_report::unresolved(state, &task_report)?;
    let block = super::super::task_report::markdown_pr_body(&task_report, unresolved);
    let measured = serde_json::to_string_pretty(&completion).map_err(|e| e.to_string())?;
    let body = format!(
        "{}\n\n{}\n\n### Native measured comparison\n\n```json\n{}\n```\n",
        document.text, block, measured
    );
    if body.len() > 48000
        || body.matches("<!-- af-task-report:v1 -->").count() != 1
        || body.matches("<!-- /af-task-report -->").count() != 1
        || body.contains("<!-- af-github:")
    {
        return Err("Native publication report exceeds content/marker bounds".into());
    }
    Ok(Proof {
        schema: "af/performance-pr-proof@1".into(),
        request_digest: request.digest()?,
        source_snapshot: source_id,
        candidate_snapshot: selected.snapshot_id,
        body,
    })
}
