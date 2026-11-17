//! Native implementation publication proof. Adapter JSON never establishes acceptance.
use super::*;
use af_scenario::{adapter::Request, contract::*, implementation::Scope, state::Run};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Proof {
    pub schema: String,
    pub request_digest: String,
    pub scope_id: String,
    pub execution_id: String,
    pub pin_id: String,
    pub scope: Scope,
    pub source_snapshot: String,
    pub candidate_snapshot: String,
    pub task_report: String,
}
fn template<'a>(
    run: &'a Run,
    receipt: &TaskReceipt,
) -> Result<&'a af_scenario::template::Template> {
    for frame in run.frames.values() {
        let definition = &run.pin.bundle.definitions[&frame.scenario];
        for progress in &frame.steps {
            if progress.intents.iter().any(|i| {
                run.fresh_tasks.get(&i.id).is_some_and(|t| {
                    t.task_id == receipt.task_id
                        && t.revision_id == receipt.revision_id
                        && t.plan_id == receipt.plan_id
                })
            }) {
                let step = definition
                    .steps
                    .iter()
                    .find(|s| s.id == progress.step)
                    .ok_or("Missing native Task step")?;
                if let Kind::FreshTask { template, .. } = &step.operation {
                    return Ok(template);
                }
            }
        }
    }
    Err("Publication receipt lacks admitted native template provenance".into())
}
fn requirements(opened: &OpenedTask) -> Result<serde_json::Value> {
    let port = opened
        .task
        .revision
        .inputs
        .get("requirements")
        .ok_or("Missing whole-goal requirements")?;
    let [id] = port.artifact_ids.as_slice() else {
        return Err("Ambiguous requirements".into());
    };
    let envelope = opened.cas.get_artifact(id).map_err(|e| e.to_string())?;
    validate_envelope(&envelope)?;
    if port.artifact_type != "af/Requirements@1" || envelope.artifact_type != "af/Requirements@1" {
        return Err("Invalid whole-goal requirements".into());
    }
    let mut specification = envelope.payload["specification"].clone();
    if let Some(map) = specification.as_object_mut() {
        // Native repair evidence supplements, never replaces, the original business contract.
        map.remove("scenario_native_outcome");
        map.remove("scenario_evidence");
    }
    Ok(specification)
}
/// Follow native predecessor/continuation records back to the admitted committed source.
/// A same-repository foreign Task is still outside this Run's template pool.
fn source_lineage(
    opened: &OpenedTask,
    run: &Run,
    scope: &Scope,
    seen: &mut BTreeSet<String>,
    goal: &str,
    specification: &serde_json::Value,
) -> Result<()> {
    if !seen.insert(opened.task.revision.task_id.clone()) || seen.len() > run.fresh_tasks.len() {
        return Err("Publication source lineage cycles or exceeds its admitted pool".into());
    }
    if opened.task.revision.goal != goal || &requirements(opened)? != specification {
        return Err("Publication source lineage changes the original whole goal".into());
    }
    let source = performance::source(opened)?;
    let bindings = recorded_input_bindings(&opened.cas, &opened.task.revision)?;
    let predecessor = bindings
        .as_ref()
        .and_then(|b| b.bindings.get("source"))
        .and_then(|b| b.task.as_ref());
    let continuation = super::super::continuation::recorded(&opened.cas, &opened.task.revision)?;
    let (task, revision, result, snapshot) = if let Some(reference) = predecessor {
        let bound = &bindings.as_ref().ok_or("Source bindings missing")?.bindings["source"];
        if reference.port != "snapshot" || reference.acceptance != TaskAcceptanceV1::Satisfied {
            return Err("Publication source predecessor is not accepted".into());
        }
        (
            reference.task_id.as_str(),
            reference.task_revision_id.as_str(),
            reference.result_id.as_str(),
            bound
                .snapshot_id
                .as_deref()
                .ok_or("Source binding Snapshot missing")?,
        )
    } else if let Some((_, findings)) = &continuation {
        super::super::continuation::verify_findings(&opened.cas, &opened.store, findings)?;
        (
            &*findings.continues.task_id,
            &*findings.continues.task_revision_id,
            &*findings.continues.result_id,
            &*findings.continues.snapshot_id,
        )
    } else {
        let (snapshot, _) = af_source_git::task::read_snapshot(&opened.cas, &source)?;
        let origin = af_source_git::task::read_origin(&opened.cas, &snapshot.origin_id)?;
        if snapshot.parent_snapshot_id.is_some()
            || snapshot.content_digest != scope.source_content_digest
            || origin.repository_id() != scope.repository_id
            || origin.source_revision() != Some(scope.source_commit.as_str())
        {
            return Err("Publication source is not the admitted original source".into());
        }
        return Ok(());
    };
    let target = run
        .fresh_tasks
        .values()
        .find(|t| t.task_id == task && t.revision_id == revision)
        .ok_or("Publication source predecessor belongs to another Run")?;
    let prior = open_task(task, &scope.repo, Some(&scope.task_state))?;
    if prior.task.revision_id != revision
        || prior.task.plan_id.as_deref() != Some(&target.plan_id)
        || !matches!(&prior.task.phase, TaskPhaseV1::Finished { result_id } if result_id == result)
    {
        return Err("Publication source predecessor revision/result changed".into());
    }
    super::super::scenario_template::experiment_source_matches(&opened.cas, &source, snapshot)?;
    if predecessor.is_some() {
        let (result, _) = prior.view()?;
        let result = result.ok_or("Publication source predecessor has no native result")?;
        let port = result
            .outputs
            .get("snapshot")
            .ok_or("Publication source predecessor lacks candidate")?;
        let selected = select(&prior, None)?;
        if result.execution != TaskExecutionV1::Completed
            || result.acceptance != TaskAcceptanceV1::Satisfied
            || !selected.verified
            || selected.snapshot_id != snapshot
            || performance::source_tree(&prior, port)? != snapshot
        {
            return Err(
                "Publication source predecessor differs from its accepted native candidate".into(),
            );
        }
    }
    source_lineage(&prior, run, scope, seen, goal, specification)
}

fn prior_tasks(run: &Run, intent: &str) -> Result<Vec<String>> {
    fn walk(run: &Run, path: &str, intent: &str, tasks: &mut Vec<String>) -> Result<bool> {
        let frame = run
            .frames
            .get(path)
            .ok_or("Publication report frame missing")?;
        for progress in &frame.steps {
            if progress.intents.iter().any(|i| i.id == intent) {
                return Ok(true);
            }
            for invocation in &progress.intents {
                if let Some(target) = run.fresh_tasks.get(&invocation.id) {
                    let opened = open_task(&target.task_id, &target.repo, Some(&target.state))?;
                    if opened.task.revision_id != target.revision_id
                        || opened.task.plan_id.as_deref() != Some(&target.plan_id)
                    {
                        return Err("Publication report Task plan changed".into());
                    }
                    if !tasks.contains(&target.task_id) {
                        tasks.push(target.task_id.clone());
                    }
                }
            }
            if let Some(child) = &progress.child {
                if walk(run, child, intent, tasks)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    let mut tasks = Vec::new();
    if !walk(run, "root", intent, &mut tasks)? {
        return Err("Publication report intent absent".into());
    }
    Ok(tasks)
}

pub(crate) fn proof(request: &Request, run: &Run, scope: &Scope) -> Result<Proof> {
    let receipt = |key: &str| match request.inputs.get(key) {
        Some(Datum::Task(t)) => Ok(t),
        _ => Err("Missing native publication receipt".to_string()),
    };
    let candidate = receipt("candidate")?;
    let verifier = receipt("verification")?;
    let implementation =
        performance::accepted(candidate, &scope.repo, &scope.task_state, "implement")?;
    let verification =
        performance::accepted(verifier, &scope.repo, &scope.task_state, "verification")?;
    let original = template(run, candidate)?;
    let independent = template(run, verifier)?;
    if original.document["goal"] != independent.document["goal"]
        || original.document["requirements"] != independent.document["requirements"]
        || independent.document["base"]["commit"] != scope.source_commit
        || implementation.task.revision.goal
            != original.document["goal"].as_str().unwrap_or_default()
        || verification.task.revision.goal
            != independent.document["goal"].as_str().unwrap_or_default()
        || implementation.task.revision.authority.policy_id != original.authority_id
        || verification.task.revision.authority.policy_id != independent.authority_id
        || requirements(&implementation)? != original.document["requirements"]
        || requirements(&verification)? != original.document["requirements"]
    {
        return Err("Publication verifier must cover the original whole goal and requirements on admitted authority".into());
    }
    source_lineage(
        &implementation,
        run,
        scope,
        &mut BTreeSet::new(),
        original.document["goal"]
            .as_str()
            .ok_or("Missing original goal")?,
        &original.document["requirements"],
    )?;
    let selected = select(&implementation, None)?;
    let port = candidate
        .result
        .outputs
        .get("snapshot")
        .ok_or("Missing candidate artifact")?;
    let verified_source =
        performance::predecessor(&verification, candidate, &selected.snapshot_id)?;
    let verified = select(&verification, None)?;
    if !selected.verified
        || !verified.verified
        || verified.snapshot_id != verified_source
        || performance::source_tree(&implementation, port)? != selected.snapshot_id
    {
        return Err("Publication requires the exact independently accepted candidate".into());
    }
    // Verification's declared original baseline is separate from both its candidate source
    // and the adapter's later live destination branch observation.
    use af_contracts::task::findings::{TASK_BASE_V1, TaskBaseV1};
    let mut bases = Vec::new();
    for id in &verification.task.revision.provenance.input_artifact_ids {
        let envelope = verification
            .cas
            .get_artifact(id)
            .map_err(|e| e.to_string())?;
        if envelope.artifact_type == TASK_BASE_V1 {
            let base: TaskBaseV1 = artifact(&verification.cas, id, TASK_BASE_V1)?;
            base.validate()?;
            bases.push(base);
        }
    }
    let [base] = bases.as_slice() else {
        return Err("Verification needs exactly one pinned original baseline".into());
    };
    let (snapshot, old) = af_source_git::task::read_snapshot(&verification.cas, &base.snapshot_id)?;
    let origin = af_source_git::task::read_origin(&verification.cas, &snapshot.origin_id)?;
    if base.commit != scope.source_commit
        || origin.source_revision() != Some(scope.source_commit.as_str())
        || origin.repository_id() != scope.repository_id
        || snapshot.content_digest != scope.source_content_digest
        || snapshot.parent_snapshot_id.is_some()
        || base.snapshot_id == verified_source
    {
        return Err("Verification original baseline differs from startup scope".into());
    }
    let (_, new) = af_source_git::task::read_snapshot(&implementation.cas, &selected.snapshot_id)?;
    let controls = |m: &af_source_git::Manifest| {
        m.entries
            .iter()
            .filter(|e| e.path == ".github" || e.path.starts_with(".github/"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let source = performance::source(&implementation)?;
    let (source_snapshot, _) = af_source_git::task::read_snapshot(&implementation.cas, &source)?;
    let source_origin =
        af_source_git::task::read_origin(&implementation.cas, &source_snapshot.origin_id)?;
    if source_origin.repository_id() != scope.repository_id
        || !af_source_git::task::descends_from(&implementation.cas, &selected.snapshot_id, &source)?
        || old.content_digest() == new.content_digest()
        || controls(&old) != controls(&new)
    {
        return Err("Publication source lineage, changed candidate or control paths differ".into());
    }
    let Some(Datum::Action(prepared)) = request.inputs.get("prepared") else {
        return Err("Missing prepared candidate".into());
    };
    let prepared = af_scenario::workspace::Prepared::parse(prepared)?;
    let payload: serde_json::Value =
        serde_json::from_str(request.inputs["body"].as_text()?).map_err(|e| e.to_string())?;
    if prepared.task_id != candidate.task_id
        || prepared.result_id != candidate.result_id
        || prepared.snapshot_id != selected.snapshot_id
        || prepared.repository != scope.repository
        || prepared.repository_id != scope.destination_repository_id
        || prepared.base != scope.base
        || !prepared.branch.starts_with(&scope.branch_prefix)
        || prepared.publication() != payload
        || prepared.subject()? != request.inputs["subject"].as_text()?
        || prepared.commit != request.inputs["revision"].as_text()?
    {
        return Err("Publication preparation differs from scoped native proof".into());
    }
    let report =
        super::super::task_report::read(&scope.task_state, &prior_tasks(run, &request.intent_id)?)?;
    let unresolved = super::super::task_report::unresolved(&scope.task_state, &report)?;
    let task_report = super::super::task_report::markdown_pr_body(&report, unresolved);
    if task_report.len() > 48000 {
        return Err("Native scoped Task report exceeds publication proof bounds".into());
    }
    Ok(Proof {
        schema: "af/implementation-pr-proof@1".into(),
        request_digest: hash(request)?,
        scope_id: hash(scope)?,
        execution_id: run.execution_id.clone(),
        pin_id: run.pin.revision.clone(),
        scope: scope.clone(),
        source_snapshot: base.snapshot_id.clone(),
        candidate_snapshot: selected.snapshot_id,
        task_report,
    })
}
