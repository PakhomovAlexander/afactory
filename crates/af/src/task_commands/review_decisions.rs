//! Capture an exact human decision batch; the native reducer owns its admission and replay.
use super::*;
use af_contracts::task::review::{ReviewHistoryV1, TASK_REVIEW_ROUND_V1};
use af_contracts::task::review_decisions::*;

pub(crate) struct Options {
    pub task_id: String,
    pub from: String,
    pub file: PathBuf,
    pub pipeline: String,
    pub authority: String,
    pub repo: PathBuf,
    pub state: Option<PathBuf>,
    pub json: bool,
    pub execute: bool,
}

pub(crate) fn context(task_id: &str, repo: &Path, state: Option<&Path>) -> Result<i32, String> {
    let (_, state) = state_path(repo, state)?;
    require_store(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let task = store
        .task_projection(&cas, task_id)
        .map_err(|e| e.to_string())?
        .ok_or("No recorded Review Task")?;
    let TaskPhaseV1::Finished { result_id } = task.phase else {
        return Err("Decision context requires a finished native Review Task".into());
    };
    let result: TaskResultV1 = artifact(&cas, &result_id, TASK_RESULT_V1)?;
    result.validate()?;
    if result.execution != TaskExecutionV1::Completed
        || result.acceptance != TaskAcceptanceV1::Satisfied
    {
        return Err("Decision context requires a complete native Review Task; required failed work must be resumed first".into());
    }
    let port = result
        .outputs
        .get("review")
        .filter(|p| p.artifact_type == TASK_REVIEW_ROUND_V1)
        .ok_or("Task has no selected standalone Review output")?;
    let review_id = port.artifact_ids[0].clone();
    let review: TaskReviewRoundV1 = artifact(&cas, &review_id, TASK_REVIEW_ROUND_V1)?;
    let ledger = ReviewTaskDomain::recorded_ledger(&cas, &review_id)?;
    let finding_set = artifact(
        &cas,
        review
            .finding_set_id
            .as_deref()
            .ok_or("Incomplete Review has no human decision authority")?,
        af_contracts::contract::FINDING_SET_V1,
    )?;
    let demand_set = artifact(
        &cas,
        review
            .demand_set_id
            .as_deref()
            .ok_or("Incomplete Review has no human decision authority")?,
        af_contracts::contract::DEMAND_SET_V1,
    )?;
    let mut finding_view_ids = BTreeMap::new();
    let mut resolutions = BTreeMap::new();
    for finding in ledger.findings() {
        let key = finding.key.clone();
        finding_view_ids.insert(
            key.clone(),
            ledger.finding_view_id(&key).ok_or("Missing Finding view")?,
        );
        if let Some(record) = ledger.resolution(&key) {
            resolutions.insert(
                key,
                DecisionResolutionV1 {
                    artifact_id: record.artifact_id.clone(),
                    resolution: record.resolution.clone(),
                },
            );
        }
    }
    let value = ReviewDecisionContextV1 {
        schema: "af/review-decision-context@1".into(),
        review_id,
        subject: af_store::resolve_subject_scope(&cas, &review.subject_id)
            .map_err(|e| e.to_string())?
            .subject,
        review,
        finding_set,
        demand_set,
        finding_view_ids,
        policy_time: ledger.policy_time(),
        resolutions,
    };
    value.validate()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(0)
}

pub(crate) fn run(options: Options) -> Result<i32, String> {
    let (repo, state) = state_path(&options.repo, options.state.as_deref())?;
    require_store(&state)?;
    let bytes = input_file::read(&options.file, 256 * 1024, "Review decisions")?;
    let batch: ReviewDecisionsV1 = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    batch.validate()?;
    let inputs = {
        let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
        let store =
            EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
        if store
            .task_projection(&cas, &options.task_id)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("Human decisions require a new Task ID; resume the already captured exact plan instead".into());
        }
        let task = store
            .task_projection(&cas, &options.from)
            .map_err(|e| e.to_string())?
            .ok_or("Review decisions: source Task is not recorded in this Store")?;
        let TaskPhaseV1::Finished { result_id } = &task.phase else {
            return Err("Review decisions require a finished native Review Task".into());
        };
        let result: TaskResultV1 = artifact(&cas, result_id, TASK_RESULT_V1)?;
        result.validate()?;
        if result.execution != TaskExecutionV1::Completed
            || result.acceptance != TaskAcceptanceV1::Satisfied
        {
            return Err("Human decisions require a complete native Review Task; resume incomplete or failed required work first".into());
        }
        let conclusions: Vec<_> = result
            .evidence
            .iter()
            .filter(|id| {
                cas.get_artifact(id).is_ok_and(|a| {
                    a.artifact_type == af_contracts::task::review::TASK_REVIEW_CONCLUSION_V1
                })
            })
            .cloned()
            .collect();
        let [conclusion] = conclusions.as_slice() else {
            return Err(
                "Human decisions require the exact original Task accounting conclusion".into(),
            );
        };
        let review = result.outputs.get("review").filter(|p| p.artifact_type == TASK_REVIEW_ROUND_V1 && p.artifact_ids == [batch.review_id.clone()]).ok_or("Review decisions must name the selected original Review output, never a discarded or stale Round")?;
        let round: TaskReviewRoundV1 =
            artifact(&cas, &review.artifact_ids[0], TASK_REVIEW_ROUND_V1)?;
        // Also checks the entire original plan, immutable results and canonical reduction.
        ReviewTaskDomain::recorded_ledger(&cas, &batch.review_id)?;
        if round.policy_id != batch.policy_id
            || round.subject_id != batch.subject_id
            || round.snapshot_id != batch.snapshot_id
        {
            return Err(
                "Review decisions changed their original policy, Subject or Snapshot".into(),
            );
        }
        let history = result
            .outputs
            .get("history")
            .filter(|p| p.artifact_type == REVIEW_HISTORY_V1)
            .ok_or("Review decisions require complete native history")?
            .clone();
        let value: ReviewHistoryV1 = artifact(&cas, &history.artifact_ids[0], REVIEW_HISTORY_V1)?;
        value.validate()?;
        if !matches!(value, ReviewHistoryV1::Recorded { round_report_id, .. } if round_report_id == batch.review_id)
        {
            return Err("Review decisions changed selected history".into());
        }
        let subject = af_store::resolve_subject_scope(&cas, &batch.subject_id)
            .map_err(|e| e.to_string())?
            .subject;
        if subject.base_snapshot_id != batch.base_snapshot_id {
            return Err("Review decisions changed the original Base".into());
        }
        let source = source_tree(
            &cas,
            producer(),
            &batch.snapshot_id,
            vec![batch.review_id.clone()],
        )?;
        let id = cas
            .put_artifact(
                REVIEW_DECISIONS_V1,
                producer(),
                vec![
                    batch.review_id.clone(),
                    batch.policy_id.clone(),
                    batch.subject_id.clone(),
                    batch.snapshot_id.clone(),
                ],
                None,
                serde_json::to_value(&batch).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        let mut inputs = BTreeMap::from([
            (
                "original_result".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![result_id.clone()],
                    artifact_type: TASK_RESULT_V1.into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: None,
                },
            ),
            (
                "original_conclusion".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![conclusion.clone()],
                    artifact_type: af_contracts::task::review::TASK_REVIEW_CONCLUSION_V1.into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: None,
                },
            ),
            ("source".into(), source),
            ("history".into(), history),
            (
                "decisions".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![id],
                    artifact_type: REVIEW_DECISIONS_V1.into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: None,
                },
            ),
        ]);
        if let Some(base) = batch.base_snapshot_id.as_deref() {
            inputs.insert(
                "base".into(),
                source_tree(&cas, producer(), base, vec![batch.review_id.clone()])?,
            );
        }
        inputs
    };
    // The ordinary adapter, compiler, exact-plan confirmation, lease and publication remain
    // the sole execution authority. No Task limit or original charge is amended.
    let bytes = serde_json::to_vec(&json!({
        "schema":"af.task-file/1", "task_id":options.task_id, "kind":"review",
        "goal":format!("Apply exact human Review decisions: {}", batch.reason), "strategy":"light",
        "pipeline":{"name":options.pipeline,"fallback":"refuse"},
        "limits":{"tokens":0,"max_attempts":1,"wall_ms":60000,"verification":{"tokens":0,"attempts":0,"wall_ms":0}}
    })).map_err(|e| e.to_string())?;
    if !options.execute {
        crate::model_check::enable_plan_time();
    }
    start_kind_bytes(
        StartOptions {
            review_inputs: Some(inputs),
            review_selectors: None,
            input_json: vec![],
            file: options.file,
            bindings: None,
            source_bindings: None,
            repo,
            state: Some(state),
            authority: options.authority,
            uncommitted: false,
            json: options.json,
            plan_only: !options.execute,
            timeout_secs: None,
            optimization_history: None,
            fit: false,
            replace: false,
        },
        bytes,
        Some("review"),
        Presentation::Print,
    )
}
