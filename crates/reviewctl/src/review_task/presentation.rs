//! Common Review presentation reads the original Round and the Campaign's one Task budget.
//! Selected transport observations never stand in for retries, Provider or late usage charges.

use review_core::task::usage::{DecimalU64, DecimalU128, TaskTokenUsageV1, TaskTokenUsageV3};
use review_core::task::{
    TaskAcceptanceV1, TaskExecutionV1, TaskLimitsV1, TaskPhaseV1, TaskResultV1,
};
use review_graph::{NodeOutcome, RunReport, SuppressionReason};
use review_pipeline::{RunVerdict, TaskAttemptEvidence};
use review_store::{Cas, EventStore, Ledger, Verdict};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
struct AuthorityView {
    authority_snapshot_id: String,
    campaign_manifest_id: String,
    subject_id: String,
    head_snapshot_id: String,
    round_event_id: String,
    round: u32,
    epoch: u32,
}

#[derive(Serialize)]
struct TaskView {
    task_id: String,
    revision_id: String,
    plan_id: String,
    phase: TaskPhaseV1,
    result: Option<TaskResultV1>,
    limits: TaskLimitsV1,
    committed_tokens: DecimalU128,
    begun_attempts: DecimalU64,
    budget_breached: bool,
}

struct Presentation<'a> {
    mode: crate::CampaignMode,
    candidate: Value,
    run_id: String,
    authority: AuthorityView,
    task: TaskView,
    report: &'a RunReport,
    ledger: &'a Ledger,
    attempts: &'a [TaskAttemptEvidence],
    round_verdict: &'a RunVerdict,
    continuation_required: bool,
    ledger_production: &'static str,
}

pub(super) struct PreparedPresentation {
    value: Value,
    verdict: RunVerdict,
    json: Option<String>,
}

impl PreparedPresentation {
    /// Output may block indefinitely; the caller releases its writer lease first.
    pub(super) fn emit(self, options: &crate::Options) -> RunVerdict {
        if let Some(json) = self.json {
            println!("{json}");
            if let Some(guidance) = light_guidance(&self.value) {
                crate::run_progress(options, format_args!("next     {guidance}"));
            }
        } else {
            human(options, &self.value, &self.verdict);
        }
        self.verdict
    }
}

pub(super) fn prepare(
    options: &crate::Options,
    cas: &Cas,
    store: &EventStore,
    captured: &super::CapturedReviewTask,
    execution: &super::RoundExecution,
) -> Result<PreparedPresentation, String> {
    let state = store
        .task_projection(cas, &captured.revision.task_id)
        .map_err(|e| e.to_string())?
        .ok_or("Review Task is unavailable for presentation")?;
    if state.revision_id != captured.revision_id
        || state.revision != captured.revision
        || state.plan_id.as_deref() != Some(&captured.plan_id)
    {
        return Err("Review Task changed before presentation".into());
    }
    let result = match &state.phase {
        TaskPhaseV1::Finished { result_id } => {
            let frame = cas.get_artifact(result_id).map_err(|e| e.to_string())?;
            if frame.artifact_type != review_core::task::TASK_RESULT_V1 {
                return Err("Finished Review Task has another result type".into());
            }
            let result: TaskResultV1 =
                serde_json::from_value(frame.payload).map_err(|e| e.to_string())?;
            result.validate()?;
            if result.task_revision_id != state.revision_id
                || execution
                    .result
                    .as_ref()
                    .is_some_and(|expected| expected != &result)
            {
                return Err("Review presentation disagrees with its durable Task result".into());
            }
            Some(result)
        }
        _ if execution.result.is_some() => {
            return Err("Review result is not durably finished".into());
        }
        _ => None,
    };
    let budget = &state
        .execution
        .as_ref()
        .ok_or("Review Task has no accounting")?
        .budget;
    let round = captured.compiler.round();
    let authority = round.authority();
    let events = store
        .replay(&round.binding().campaign_id)
        .map_err(|e| e.to_string())?;
    let start = events
        .iter()
        .position(|event| event.event_id == authority.round_event_id())
        .ok_or("Review presentation lost its original Round")?;
    let end = events
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, event)| event.event_type == review_core::EventType::RoundStartedV1)
        .map_or(events.len(), |(index, _)| index);
    let evidence = crate::latest_round_evidence(&events[..end], cas)?
        .ok_or("Review presentation has no durable Round evidence")?;
    let candidate = crate::candidate_identity()?;
    let presentation = Presentation {
        // The recorded-Round loader has already checked this against captured authority.
        mode: options.mode,
        candidate: json!({"version":candidate.version,"executable":candidate.executable,"binary_sha256":candidate.binary_sha256}),
        run_id: round.binding().campaign_id.clone(),
        authority: AuthorityView {
            authority_snapshot_id: authority.authority_snapshot_id().into(),
            campaign_manifest_id: authority.campaign_manifest_id().into(),
            subject_id: authority.subject_id().into(),
            head_snapshot_id: authority.head_snapshot_id().into(),
            round_event_id: authority.round_event_id().into(),
            round: authority.round(),
            epoch: authority.epoch(),
        },
        task: TaskView {
            task_id: state.task_id,
            revision_id: state.revision_id,
            plan_id: captured.plan_id.clone(),
            phase: state.phase,
            result,
            limits: state.revision.limits,
            committed_tokens: budget.committed_tokens().into(),
            begun_attempts: budget.begun_attempts().into(),
            budget_breached: budget.breached(),
        },
        report: &execution.report,
        ledger: &execution.ledger,
        attempts: &execution.attempts,
        round_verdict: &execution.verdict,
        continuation_required: execution.continuation_required,
        ledger_production: evidence.ledger_production,
    };
    let (value, verdict) = presentation.value(cas)?;
    let json = options
        .json
        .then(|| serde_json::to_string(&value).map_err(|e| e.to_string()))
        .transpose()?;
    Ok(PreparedPresentation {
        value,
        verdict,
        json,
    })
}

fn incomplete(round: &RunVerdict, reason: &str) -> RunVerdict {
    match round {
        RunVerdict::Incomplete { .. } => round.clone(),
        _ => RunVerdict::Incomplete {
            missing: vec![("task".into(), reason.into())],
        },
    }
}

fn disposition(
    mode: crate::CampaignMode,
    round: &RunVerdict,
    task: &TaskView,
    continuation: bool,
) -> (RunVerdict, Value) {
    let resources_failed = task.budget_breached
        || task
            .result
            .as_ref()
            .is_some_and(|result| result.execution == TaskExecutionV1::Exhausted);
    if resources_failed {
        return (
            incomplete(round, "The original Task resources are exhausted"),
            json!({
                "kind":"human_decision","start_another_campaign":false,
                "message":"The Task resources are exhausted. Retain this Campaign for a human decision; do not start a replacement Campaign."
            }),
        );
    }
    if continuation {
        let overall = match round {
            RunVerdict::Fail(Verdict::NotConverged) => round.clone(),
            _ => incomplete(
                round,
                "The derived head requires the next full Review Round",
            ),
        };
        return (
            overall,
            json!({"kind":"continue_campaign","start_another_campaign":false}),
        );
    }
    let overall = match task.result.as_ref().map(|result| result.acceptance) {
        Some(TaskAcceptanceV1::Satisfied)
            if matches!(task.phase, TaskPhaseV1::Finished { .. }) && *round == RunVerdict::Pass =>
        {
            RunVerdict::Pass
        }
        Some(TaskAcceptanceV1::Unsatisfied) => match round {
            RunVerdict::Fail(_) => round.clone(),
            _ => RunVerdict::Fail(Verdict::NotConverged),
        },
        _ => incomplete(round, "The Task has no satisfied terminal acceptance"),
    };
    let action = if matches!(task.phase, TaskPhaseV1::Finished { .. })
        && task
            .result
            .as_ref()
            .is_some_and(|r| r.acceptance == TaskAcceptanceV1::Inconclusive)
    {
        json!({"kind":"human_decision","start_another_campaign":false,
            "message":"The Task finished without conclusive acceptance. Retain its evidence for a human decision."})
    } else {
        crate::next_action_value(mode, &overall)
    };
    (overall, action)
}

fn add(sum: u128, value: u128) -> Result<u128, String> {
    sum.checked_add(value)
        .ok_or_else(|| "Review selected observation total exceeds u128".into())
}

fn sum(
    attempts: &[TaskAttemptEvidence],
    select: impl Fn(&TaskAttemptEvidence) -> u128,
) -> Result<DecimalU128, String> {
    attempts
        .iter()
        .try_fold(0_u128, |total, attempt| add(total, select(attempt)))
        .map(Into::into)
}

fn selected_totals(attempts: &[TaskAttemptEvidence]) -> Result<Value, String> {
    let mut usage = BTreeMap::<&str, DecimalU128>::new();
    for (name, select) in [
        (
            "input_tokens",
            (|u: &TaskTokenUsageV3| u.input_tokens) as fn(&TaskTokenUsageV3) -> Option<DecimalU128>,
        ),
        ("output_tokens", |u: &TaskTokenUsageV3| u.output_tokens),
        ("cache_read_tokens", |u: &TaskTokenUsageV3| {
            u.cache_read_tokens
        }),
        ("cache_write_tokens", |u: &TaskTokenUsageV3| {
            u.cache_write_tokens
        }),
        ("reasoning_tokens", |u: &TaskTokenUsageV3| {
            u.reasoning_tokens
        }),
    ] {
        let values: Vec<_> = attempts.iter().filter_map(|a| select(&a.usage)).collect();
        if !values.is_empty() {
            usage.insert(
                name,
                values
                    .into_iter()
                    .try_fold(0_u128, |total, value| add(total, value.get()))?
                    .into(),
            );
        }
    }
    usage.insert(
        "chargeable_tokens",
        sum(attempts, |a| a.usage.chargeable_tokens.get())?,
    );
    Ok(
        json!({"count":u64::try_from(attempts.len()).map_err(|_| "Review Attempt count exceeds u64")?.to_string(),
        "cost_tokens":sum(attempts, |a| a.cost_tokens)?,
        "context":{"rendered_bytes":sum(attempts, |a| u128::from(a.context_manifest.rendered_bytes))?,"estimated_tokens":sum(attempts, |a| u128::from(a.context_manifest.estimated_tokens))?},
        "usage":usage}),
    )
}

fn manifest_value(manifest: &review_runner::ContextManifest) -> Value {
    let entries: Vec<_> = manifest.entries.iter().map(|entry| {
        let mut value = json!({"name":entry.name,"required_by":entry.required_by,
            "rendered_bytes":entry.rendered_bytes.to_string(),"estimated_tokens":entry.estimated_tokens.to_string()});
        if let Some(id) = &entry.artifact_id { value["artifact_id"] = json!(id); }
        if let Some(ty) = &entry.artifact_type { value["artifact_type"] = json!(ty); }
        value
    }).collect();
    json!({"entries":entries,"rendered_bytes":manifest.rendered_bytes.to_string(),"estimated_tokens":manifest.estimated_tokens.to_string()})
}

fn available_results(cas: &Cas, attempts: &[TaskAttemptEvidence]) -> Result<Vec<Value>, String> {
    attempts.iter().map(|attempt| {
        let result = cas.get_json(&attempt.result_artifact).map_err(|e| e.to_string())?;
        let reports = result.get("reports").or_else(|| result.get("findings"))
            .and_then(Value::as_array).ok_or("Selected Review result has no reports")?;
        let findings: Vec<_> = reports.iter().map(|report| json!({
            "severity":report.get("severity"),"title":report.get("title"),"body":report.get("body"),
            "file":report.get("file"),"line":report.get("line"),"locations":report.get("locations"),
        })).collect();
        let severities: BTreeSet<_> = findings.iter().filter_map(|f| f["severity"].as_str()).collect();
        Ok(json!({"node":attempt.node,"attempt_id":attempt.attempt_id,"result_artifact_id":attempt.result_artifact,
            "spend_tokens":attempt.cost_tokens.to_string(),"severities":severities,"findings":findings}))
    }).collect()
}

impl Presentation<'_> {
    fn value(&self, cas: &Cas) -> Result<(Value, RunVerdict), String> {
        let (overall, action) = disposition(
            self.mode,
            self.round_verdict,
            &self.task,
            self.continuation_required,
        );
        let findings: Vec<_> = self
            .ledger
            .finding_views()
            .into_iter()
            .map(|f| {
                json!({
                    "key":f.key,"severity":f.severity,"effective_severity":f.convergence_severity,
                    "status":f.status.as_str(),"scope":f.convergence_scope_label(),"file":f.file,
                    "line":f.line,"title":f.title,"aliases":f.aliases,
                })
            })
            .collect();
        let demands: Vec<_> = self
            .ledger
            .demand_views()
            .into_iter()
            .filter(|d| {
                d.requirement == review_core::DemandRequirement::Required
                    && matches!(
                        d.status,
                        review_core::DemandStatus::Open | review_core::DemandStatus::Stale
                    )
            })
            .map(|d| d.demand_id)
            .collect();
        let nodes: Vec<_> = self.report.outcomes.iter().map(|(node, outcome)| match outcome {
            NodeOutcome::Completed { outputs } => json!({"node":node,"kind":"completed","output_artifacts":outputs.values().flatten().collect::<Vec<_>>()}),
            NodeOutcome::Failed { error, .. } => json!({"node":node,"kind":"failed","error":error}),
            NodeOutcome::Suppressed { reason } => json!({"node":node,"kind":"suppressed","reason":match reason {
                SuppressionReason::BranchNotSelected=>"branch_not_selected",SuppressionReason::GateBlocked=>"gate_blocked",SuppressionReason::UpstreamMissing=>"upstream_missing",
            }}),
        }).collect();
        let attempts: Vec<_> = self
            .attempts
            .iter()
            .map(|a| {
                json!({
                    "node":a.node,"attempt_id":a.attempt_id,"cost_tokens":a.cost_tokens.to_string(),
                    "usage":a.usage,"context_manifest":manifest_value(&a.context_manifest),
                    "raw_artifact":a.raw_artifact,"result_artifact":a.result_artifact,
                })
            })
            .collect();
        let available = if self.ledger_production.starts_with("not_produced_") {
            available_results(cas, self.attempts)?
        } else {
            Vec::new()
        };
        let schema = if self.attempts.iter().any(|a| {
            a.cost_tokens > u128::from(u64::MAX) || TaskTokenUsageV1::try_from(&a.usage).is_err()
        }) {
            "af/review-outcome@3"
        } else {
            "af/review-outcome@2"
        };
        Ok((
            json!({"schema":schema,"campaign_mode":self.mode.as_str(),"candidate":self.candidate,
            "run_id":self.run_id,"authority":self.authority,"task":self.task,
            "node_outcomes":nodes,"blocked_gates":self.report.blocked_gates,"attempts":attempts,
            "totals":{"selected_attempts":selected_totals(self.attempts)?,"open_required_demands":u64::try_from(demands.len()).map_err(|_|"Demand count exceeds u64")?.to_string(),"open_or_stale_demand_ids":demands},
            "findings":findings,"ledger_production":self.ledger_production,"available_node_results":available,
            "round_outcome":crate::verdict_value(self.round_verdict),"outcome":crate::verdict_value(&overall),
            "continuation_required":self.continuation_required,"next_action":action}),
            overall,
        ))
    }
}

fn human(options: &crate::Options, value: &Value, verdict: &RunVerdict) {
    for line in human_lines(value) {
        crate::run_progress(options, format_args!("{line}"));
    }
    crate::run_progress(options, format_args!("verdict  {verdict:?}"));
}

fn light_guidance(value: &Value) -> Option<&'static str> {
    if value["campaign_mode"] != "light" {
        return None;
    }
    match value["next_action"]["kind"].as_str()? {
        "fix_then_gate" => Some(
            "fix the findings, run the deterministic project gate, then stop; do not start another Campaign (use --heavy only by explicit human choice)",
        ),
        "resume_incomplete_round" => Some("resume this exact incomplete light Round"),
        _ => None,
    }
}

fn human_lines(value: &Value) -> Vec<String> {
    let text = |value: &Value| value.as_str().unwrap_or("?").to_string();
    let label = |outcome: &Value| {
        outcome["reason"]
            .as_str()
            .or_else(|| outcome["kind"].as_str())
            .unwrap_or("incomplete")
            .to_string()
    };
    let mut lines = vec![
        format!(
            "Round {} epoch {}: {}",
            value["authority"]["round"],
            value["authority"]["epoch"],
            label(&value["round_outcome"])
        ),
        format!(
            "task     {}: {}",
            text(&value["task"]["task_id"]),
            label(&value["outcome"])
        ),
        format!(
            "spent    {} / {} tokens across {} Task Attempts",
            text(&value["task"]["committed_tokens"]),
            value["task"]["limits"]["tokens"],
            text(&value["task"]["begun_attempts"])
        ),
        format!(
            "selected {} Reviewer Attempts; {} observed tokens",
            text(&value["totals"]["selected_attempts"]["count"]),
            text(&value["totals"]["selected_attempts"]["cost_tokens"])
        ),
    ];
    if let Some(nodes) = value["node_outcomes"].as_array() {
        for node in nodes {
            let label = match node["kind"].as_str() {
                Some("completed") => "done     ",
                Some("failed") => "FAILED   ",
                _ => "never-ran",
            };
            lines.push(format!("  {label} {}", text(&node["node"])));
        }
    }
    for finding in value["findings"].as_array().into_iter().flatten() {
        let line = finding["line"]
            .as_i64()
            .map_or_else(|| "?".into(), |line| line.to_string());
        lines.push(format!(
            "  [{}] {}:{} — {} ({})",
            text(&finding["severity"]),
            text(&finding["file"]),
            line,
            text(&finding["title"]),
            text(&finding["status"])
        ));
    }
    if value["ledger_production"]
        .as_str()
        .is_some_and(|v| v.starts_with("not_produced_"))
    {
        lines.push("recorded, not gathered (Ledger was not produced):".into());
        for result in value["available_node_results"]
            .as_array()
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "  {} attempt {} artifact {}",
                text(&result["node"]),
                text(&result["attempt_id"]),
                text(&result["result_artifact_id"])
            ));
            for finding in result["findings"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "    [{}] {}",
                    text(&finding["severity"]),
                    text(&finding["title"])
                ));
            }
        }
    }
    lines.push(format!(
        "demands  {} open/stale (required)",
        text(&value["totals"]["open_required_demands"])
    ));
    let action = &value["next_action"];
    let guidance = light_guidance(value).unwrap_or_else(|| {
        action["message"]
            .as_str()
            .or_else(|| action["kind"].as_str())
            .unwrap_or("human decision")
    });
    lines.push(format!("next     {guidance}"));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::PortCardinality;
    use review_core::task::{ArtifactInputV1, VerificationReserveV1};
    use review_runner::{ContextManifest, TokenUsage};
    use std::sync::OnceLock;

    fn id() -> String {
        format!("sha256:{}", "a".repeat(64))
    }
    fn task() -> TaskView {
        TaskView {
            task_id: "review_task".into(),
            revision_id: id(),
            plan_id: id(),
            phase: TaskPhaseV1::Running {},
            result: None,
            limits: TaskLimitsV1 {
                tokens: 100,
                max_attempts: 8,
                deadline_unix_ms: 1_900_000_000_000,
                verification: VerificationReserveV1 {
                    tokens: 0,
                    attempts: 0,
                    wall_ms: 0,
                },
            },
            committed_tokens: 7_u128.into(),
            begun_attempts: 2_u64.into(),
            budget_breached: false,
        }
    }
    fn finish(task: &mut TaskView, execution: TaskExecutionV1, acceptance: TaskAcceptanceV1) {
        task.phase = TaskPhaseV1::Finished { result_id: id() };
        task.result = Some(TaskResultV1 {
            task_revision_id: task.revision_id.clone(),
            execution,
            acceptance,
            domain_conclusion: "canonical Review evidence".into(),
            outputs: BTreeMap::from([(
                "findings".into(),
                ArtifactInputV1 {
                    artifact_ids: vec![id()],
                    artifact_type: "review.kernel/FindingSet@1".into(),
                    cardinality: PortCardinality::One,
                    snapshot_id: None,
                },
            )]),
            evidence: BTreeSet::from([id()]),
            missing_obligations: BTreeSet::new(),
        });
        task.result.as_ref().unwrap().validate().unwrap();
    }
    fn validator() -> &'static jsonschema::Validator {
        validator_for(false)
    }
    fn validator_for(wide: bool) -> &'static jsonschema::Validator {
        static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
        static WIDE: OnceLock<jsonschema::Validator> = OnceLock::new();
        (if wide { &WIDE } else { &VALIDATOR }).get_or_init(|| {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
            let mut registry = jsonschema::Registry::new();
            for name in [
                "task-contracts-v1.json",
                "task-phase-v1.json",
                "task-result-v1.json",
                "task-token-usage-v1.json",
                "task-token-usage-v2.json",
                "task-token-usage-v3.json",
            ] {
                let value: Value =
                    serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap();
                let id = value["$id"].as_str().unwrap().to_owned();
                registry = registry
                    .add(id, jsonschema::Resource::from_contents(value))
                    .unwrap();
            }
            let schema: Value = serde_json::from_slice(
                &std::fs::read(dir.join(if wide {
                    "review-outcome-v3.json"
                } else {
                    "review-outcome-v2.json"
                }))
                .unwrap(),
            )
            .unwrap();
            {
                let registry = registry.prepare().unwrap();
                jsonschema::options()
                    .with_registry(&registry)
                    .build(&schema)
                    .unwrap()
            }
        })
    }
    fn valid(value: &Value) {
        let errors = validator()
            .iter_errors(value)
            .map(|e| e.to_string())
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "{errors:?}\n{value:#}");
    }

    #[test]
    fn canonical_round_and_terminal_task_control_different_outcomes() {
        let mut task = task();
        let (outcome, action) =
            disposition(crate::CampaignMode::Heavy, &RunVerdict::Pass, &task, true);
        assert!(matches!(outcome, RunVerdict::Incomplete { .. }));
        assert_eq!(action["kind"], "continue_campaign");
        let failed = RunVerdict::Fail(Verdict::NotConverged);
        let (outcome, action) = disposition(crate::CampaignMode::Heavy, &failed, &task, true);
        assert_eq!(outcome, failed); // Historical failed-Round exit 3 remains distinct from Pass/4.
        assert_eq!(action["kind"], "continue_campaign");
        finish(
            &mut task,
            TaskExecutionV1::Completed,
            TaskAcceptanceV1::Satisfied,
        );
        let (outcome, action) =
            disposition(crate::CampaignMode::Light, &RunVerdict::Pass, &task, false);
        assert_eq!(outcome, RunVerdict::Pass);
        assert_eq!(action["kind"], "done");
        assert!(matches!(
            disposition(crate::CampaignMode::Heavy, &failed, &task, false).0,
            RunVerdict::Incomplete { .. }
        ));
        task.budget_breached = true;
        let (outcome, action) =
            disposition(crate::CampaignMode::Heavy, &RunVerdict::Pass, &task, false);
        assert!(matches!(outcome, RunVerdict::Incomplete { .. }));
        assert_eq!(action["kind"], "human_decision");
        task.budget_breached = false;
        for execution in [TaskExecutionV1::Incomplete, TaskExecutionV1::Exhausted] {
            finish(&mut task, execution, TaskAcceptanceV1::Inconclusive);
            let (outcome, action) =
                disposition(crate::CampaignMode::Heavy, &RunVerdict::Pass, &task, false);
            assert!(matches!(outcome, RunVerdict::Incomplete { .. }));
            assert_eq!(action["kind"], "human_decision");
        }
        finish(
            &mut task,
            TaskExecutionV1::Completed,
            TaskAcceptanceV1::Unsatisfied,
        );
        let (outcome, action) = disposition(crate::CampaignMode::Light, &failed, &task, false);
        assert_eq!(outcome, failed);
        assert_eq!(
            action,
            crate::next_action_value(crate::CampaignMode::Light, &failed)
        );
        let lines = human_lines(&json!({"campaign_mode":"light", "next_action":action}));
        assert_eq!(
            lines.last().unwrap(),
            "next     fix the findings, run the deterministic project gate, then stop; do not start another Campaign (use --heavy only by explicit human choice)"
        );
        let missing = RunVerdict::Incomplete {
            missing: vec![("reviewer_b".into(), "provider refusal".into())],
        };
        let (outcome, _) = disposition(crate::CampaignMode::Light, &missing, &self::task(), false);
        assert_eq!(outcome, missing);
        assert!(add(u128::MAX, 1).is_err());
    }

    #[test]
    fn serializer_keeps_wide_selected_observations_separate_and_partial_evidence_visible() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let result = cas
            .put_json(&json!({"findings":[{
                "severity":"major", "title":"Paid finding survives its missing sibling",
                "body":"exact selected evidence", "file":"src/a.rs", "line":9_007_199_254_740_991_i64
            }]}))
            .unwrap();
        let typed_result = cas.put_json(&json!({"reports":[{
            "severity":"minor", "title":"Typed locations remain available", "body":"modern wire",
            "locations":[{"path":"src/b.rs", "line":u32::MAX, "end_line":u32::MAX}]
        }]})).unwrap();
        let raw = cas.put_json(&json!({"captured":"transport"})).unwrap();
        let attempts: Vec<_> = (1..=2)
            .map(|n| TaskAttemptEvidence {
                node: format!("scatter#{n}"),
                attempt_id: format!("{n:026}"),
                cost_tokens: u128::from(u64::MAX),
                usage: TokenUsage {
                    input_tokens: Some(u64::MAX),
                    chargeable_tokens: u64::MAX,
                    ..TokenUsage::default()
                }
                .into(),
                context_manifest: ContextManifest {
                    entries: vec![review_runner::ContextEntry {
                        name: "worker_input".into(),
                        required_by: "typed ReviewerInputs document".into(),
                        artifact_id: Some(raw.clone()),
                        artifact_type: Some("review.kernel/ReviewerResult@1".into()),
                        rendered_bytes: u64::MAX,
                        estimated_tokens: u64::MAX,
                    }],
                    rendered_bytes: u64::MAX,
                    estimated_tokens: u64::MAX,
                },
                raw_artifact: raw.clone(),
                result_artifact: if n == 1 {
                    result.clone()
                } else {
                    typed_result.clone()
                },
            })
            .collect();
        let report = RunReport {
            outcomes: vec![
                (
                    "reviewer_b".into(),
                    NodeOutcome::Failed {
                        error: "provider refusal".into(),
                        class: None,
                    },
                ),
                (
                    "ledger".into(),
                    NodeOutcome::Suppressed {
                        reason: SuppressionReason::UpstreamMissing,
                    },
                ),
            ],
            blocked_gates: BTreeSet::new(),
        };
        let ledger = Ledger::default();
        let verdict = RunVerdict::Incomplete {
            missing: vec![("reviewer_b".into(), "provider refusal".into())],
        };
        let selected = 2 * u128::from(u64::MAX);
        let mut task = task();
        task.committed_tokens = (selected + 7).into();
        task.budget_breached = true;
        let mut view = Presentation {
            mode: crate::CampaignMode::Light,
            candidate: json!({"version":"test", "executable":"/tools/af", "binary_sha256":id()}),
            run_id: "review-campaign".into(),
            authority: AuthorityView {
                authority_snapshot_id: id(),
                campaign_manifest_id: id(),
                subject_id: id(),
                head_snapshot_id: id(),
                round_event_id: "1".repeat(26),
                round: 1,
                epoch: 1,
            },
            task,
            report: &report,
            ledger: &ledger,
            attempts: &attempts,
            round_verdict: &verdict,
            continuation_required: false,
            ledger_production: "not_produced_upstream_missing",
        };
        let (value, returned) = view.value(&cas).unwrap();
        valid(&value);
        assert_eq!(returned, verdict);
        assert_eq!(
            value["task"]["committed_tokens"],
            (selected + 7).to_string()
        );
        for path in [
            "/totals/selected_attempts/cost_tokens",
            "/totals/selected_attempts/usage/input_tokens",
            "/totals/selected_attempts/usage/chargeable_tokens",
            "/totals/selected_attempts/context/rendered_bytes",
            "/totals/selected_attempts/context/estimated_tokens",
        ] {
            assert_eq!(
                value.pointer(path).unwrap(),
                &json!(selected.to_string()),
                "{path}"
            );
        }
        assert!(
            value["totals"]["selected_attempts"]["usage"]
                .get("output_tokens")
                .is_none()
        );
        assert_eq!(
            value["attempts"][0]["usage"]["chargeable_tokens"],
            u64::MAX.to_string()
        );
        assert_eq!(value["available_node_results"].as_array().unwrap().len(), 2);
        assert_eq!(
            value["available_node_results"][0]["result_artifact_id"],
            result
        );
        assert_eq!(
            value["available_node_results"][0]["findings"][0]["line"],
            9_007_199_254_740_991_i64
        );
        assert_eq!(
            value["available_node_results"][1]["findings"][0]["locations"][0]["line"],
            u32::MAX
        );
        assert_eq!(value["findings"], json!([]));
        let human = human_lines(&value).join("\n");
        assert!(human.contains("recorded, not gathered (Ledger was not produced)"));
        assert!(human.contains("Paid finding survives its missing sibling"));
        assert!(!human.contains("next     done"));
        for (path, replacement) in [
            (
                "/task/committed_tokens",
                json!("340282366920938463463374607431768211456"),
            ),
            ("/task/committed_tokens", json!(7)),
            ("/task/committed_tokens", json!("07")),
            ("/task/committed_tokens", json!("-1")),
            ("/task/begun_attempts", json!("18446744073709551616")),
            (
                "/attempts/0/usage/chargeable_tokens",
                json!("18446744073709551616"),
            ),
            (
                "/totals/selected_attempts/context/rendered_bytes",
                json!("340282366920938463463374607431768211456"),
            ),
            ("/task/limits/tokens", json!(9_007_199_254_740_992_u64)),
            ("/authority/subject_id", json!("ambient-head")),
            ("/schema", json!("af/review-outcome@1")),
            ("/next_action/kind", json!("done")),
        ] {
            let mut invalid = value.clone();
            *invalid.pointer_mut(path).unwrap() = replacement;
            assert!(
                !validator().is_valid(&invalid),
                "accepted {path}: {invalid}"
            );
        }
        for (path, field) in [
            ("", "ambient_authority"),
            ("/task", "replacement_budget"),
            ("/attempts/0", "credential"),
            ("/authority", "live_head"),
        ] {
            let mut invalid = value.clone();
            invalid.pointer_mut(path).unwrap()[field] = json!(true);
            assert!(!validator().is_valid(&invalid), "accepted extra {field}");
        }
        // Terminal accepted serialization requires the actual Task result shape. A canonical
        // passing Round alone is insufficient, including a committed but unreviewed head.
        view.task = self::task();
        finish(
            &mut view.task,
            TaskExecutionV1::Completed,
            TaskAcceptanceV1::Satisfied,
        );
        view.round_verdict = &RunVerdict::Pass;
        view.ledger_production = "produced_clean";
        let (clean, outcome) = view.value(&cas).unwrap();
        valid(&clean);
        assert_eq!(outcome, RunVerdict::Pass);
        view.continuation_required = true;
        let (continuing, outcome) = view.value(&cas).unwrap();
        valid(&continuing);
        assert!(matches!(outcome, RunVerdict::Incomplete { .. }));
        assert_eq!(continuing["round_outcome"]["kind"], "clean");
        assert_eq!(continuing["next_action"]["kind"], "continue_campaign");
        let mut false_clean = continuing;
        false_clean["outcome"] = json!({"kind":"clean"});
        false_clean["next_action"]["kind"] = json!("done");
        assert!(!validator().is_valid(&false_clean));
        let mut inconclusive = clean;
        inconclusive["task"]["result"]["acceptance"] = json!("inconclusive");
        assert!(!validator().is_valid(&inconclusive));
        let mut exact_attempts = attempts.clone();
        for attempt in &mut exact_attempts {
            attempt.cost_tokens += 20;
            attempt.usage.input_tokens = Some((u128::from(u64::MAX) + 20).into());
            attempt.usage.chargeable_tokens = attempt.cost_tokens.into();
        }
        view.attempts = &exact_attempts;
        view.task = self::task();
        view.task.committed_tokens = (selected + 47).into();
        view.round_verdict = &verdict;
        view.continuation_required = false;
        view.ledger_production = "not_produced_upstream_missing";
        let (wide, _) = view.value(&cas).unwrap();
        assert_eq!(wide["schema"], "af/review-outcome@3");
        assert!(
            validator_for(true).is_valid(&wide),
            "{:?}",
            validator_for(true).iter_errors(&wide).collect::<Vec<_>>()
        );
        assert_eq!(
            wide["totals"]["selected_attempts"]["cost_tokens"],
            (selected + 40).to_string()
        );
        assert_eq!(
            wide["attempts"][0]["usage"]["input_tokens"],
            (u128::from(u64::MAX) + 20).to_string()
        );
        let mut invalid = wide.clone();
        invalid["schema"] = json!("af/review-outcome@2");
        assert!(
            !validator().is_valid(&invalid),
            "old per-Attempt range stays frozen"
        );
        invalid = wide;
        invalid["attempts"][0]["usage"]["input_tokens"] =
            json!("340282366920938463463374607431768211456");
        assert!(!validator_for(true).is_valid(&invalid));
    }
}
