//! `af review report`: the Rounds, spend, and Findings of one Campaign, as text, Markdown, or
//! JSON.

use std::collections::BTreeMap;

use review_core::{
    EventType, RunFailureReasonV2, RunFailureReasonV3, RunReportPayloadV2, RunReportPayloadV3,
    RunReportPayloadV4, RunReportPayloadV5, RunVerdictV2, RunVerdictV3, Severity,
};
use review_store::{Cas, Ledger, LedgerProjection, Status};

use crate::review::evidence::{
    LatestRoundEvidence, latest_round_evidence, pinned_pipeline_definition,
};
use crate::{ReportFormat, ReportOptions, campaign_run_id, campaign_state, open_campaign_store};

#[derive(serde::Serialize)]
struct ReviewReportView {
    schema: &'static str,
    campaign: String,
    runs_recorded: usize,
    ledger_round: u32,
    final_verdict: Option<String>,
    rounds: Vec<ReportRoundView>,
    spend: Vec<RoundSpendView>,
    demands: Vec<review_core::DemandSetEntryV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded_not_gathered: Option<LatestRoundEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wall_ms: Option<u64>,
    /// The scheduler's bound on simultaneously running nodes, from the pinned pipeline of the
    /// latest Round. Absent when no Round has started.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_parallel: Option<usize>,
    findings_summary: FindingsSummaryView,
    findings: Vec<review_store::Finding>,
}

#[derive(serde::Serialize)]
pub(crate) struct ReportRoundView {
    pub(crate) run: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) round: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) epoch: Option<u32>,
    pub(crate) verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reported_tokens: Option<u64>,
}

#[derive(serde::Serialize)]
pub(crate) struct RoundSpendView {
    pub(crate) round: u32,
    pub(crate) epoch: u32,
    pub(crate) spent_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) wall_ms: Option<u64>,
    pub(crate) reviewers: Vec<ReviewerSpendView>,
}

#[derive(serde::Serialize)]
pub(crate) struct ReviewerSpendView {
    pub(crate) reviewer: String,
    pub(crate) spent_tokens: u64,
    pub(crate) attempt_tokens: u64,
    pub(crate) provider_tokens: u64,
    pub(crate) attempts: Vec<AttemptSpendView>,
    pub(crate) provider_operations: Vec<ProviderSpendView>,
}

#[derive(serde::Serialize)]
pub(crate) struct AttemptSpendView {
    pub(crate) attempt_id: String,
    pub(crate) outcome: String,
    pub(crate) spent_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    /// The reservation that bounded this Attempt: the node's own cap, or the pipeline's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reserved: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) wall: Option<AttemptWallView>,
}

/// Wall-clock and provider usage from the store's sidecar; absent when the Attempt predates it.
#[derive(serde::Serialize, Clone)]
pub(crate) struct AttemptWallView {
    started_unix_ms: u64,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<review_store::AttemptUsage>,
}

/// Findings by disposition, so precision is a number rather than a feeling.
#[derive(serde::Serialize, Clone, Copy, Default)]
pub(crate) struct FindingsSummaryView {
    pub(crate) open: usize,
    pub(crate) pending_verification: usize,
    pub(crate) fixed: usize,
    pub(crate) rejected: usize,
    pub(crate) wontfix: usize,
    pub(crate) contested: usize,
}

pub(crate) fn findings_summary(findings: &[review_store::Finding]) -> FindingsSummaryView {
    findings
        .iter()
        .fold(FindingsSummaryView::default(), |mut summary, finding| {
            match finding.status {
                Status::Open => summary.open += 1,
                Status::PendingVerification => summary.pending_verification += 1,
                Status::Fixed => summary.fixed += 1,
                Status::Rejected => summary.rejected += 1,
                Status::Wontfix => summary.wontfix += 1,
                Status::Contested => summary.contested += 1,
            }
            summary
        })
}

pub(crate) fn findings_summary_line(summary: &FindingsSummaryView) -> String {
    format!(
        "{} open, {} pending, {} fixed, {} rejected, {} wontfix, {} contested",
        summary.open,
        summary.pending_verification,
        summary.fixed,
        summary.rejected,
        summary.wontfix,
        summary.contested
    )
}

/// Wall-clock a set of Rounds took: per (round, epoch), first Attempt start to last Attempt end,
/// summed across Rounds. `None` when nothing was recorded.
pub(crate) fn wall_span_ms(rows: &[review_store::AttemptWall]) -> Option<u64> {
    let mut spans: BTreeMap<(u32, u32), (u64, u64)> = BTreeMap::new();
    for row in rows {
        let end = row.started_unix_ms.saturating_add(row.elapsed_ms);
        spans
            .entry((row.round, row.epoch))
            .and_modify(|(start, finish)| {
                *start = (*start).min(row.started_unix_ms);
                *finish = (*finish).max(end);
            })
            .or_insert((row.started_unix_ms, end));
    }
    if spans.is_empty() {
        return None;
    }
    Some(spans.values().fold(0_u64, |sum, (start, finish)| {
        sum.saturating_add(finish.saturating_sub(*start))
    }))
}

/// Attaches sidecar rows to the spend view and returns the campaign's wall-clock total.
fn attach_attempt_wall(
    spend: &mut [RoundSpendView],
    rows: &[review_store::AttemptWall],
) -> Option<u64> {
    let by_attempt: BTreeMap<&str, &review_store::AttemptWall> = rows
        .iter()
        .map(|row| (row.attempt_id.as_str(), row))
        .collect();
    for round in spend.iter_mut() {
        let mut in_round = Vec::new();
        for reviewer in &mut round.reviewers {
            for attempt in &mut reviewer.attempts {
                if let Some(row) = by_attempt.get(attempt.attempt_id.as_str()) {
                    attempt.wall = Some(AttemptWallView {
                        started_unix_ms: row.started_unix_ms,
                        elapsed_ms: row.elapsed_ms,
                        usage: row.usage.clone(),
                    });
                    in_round.push((*row).clone());
                }
            }
        }
        round.wall_ms = wall_span_ms(&in_round);
    }
    wall_span_ms(rows)
}

pub(crate) fn human_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if ms < 1000 {
        format!("{ms}ms")
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn usage_summary(usage: &review_store::AttemptUsage) -> String {
    let mut parts = Vec::new();
    for (label, value) in [
        ("in", usage.input_tokens),
        ("out", usage.output_tokens),
        ("cache-read", usage.cache_read_tokens),
        ("cache-write", usage.cache_write_tokens),
        ("reasoning", usage.reasoning_tokens),
    ] {
        if let Some(value) = value {
            parts.push(format!("{label} {value}"));
        }
    }
    if parts.is_empty() {
        format!("chargeable {}", usage.chargeable_tokens)
    } else {
        parts.join(", ")
    }
}

fn attempt_wall_suffix(wall: Option<&AttemptWallView>) -> String {
    wall.map(|wall| {
        format!(
            ", {}{}",
            human_duration(wall.elapsed_ms),
            wall.usage
                .as_ref()
                .map(|usage| format!(" ({})", usage_summary(usage)))
                .unwrap_or_default()
        )
    })
    .unwrap_or_default()
}

#[derive(serde::Serialize)]
pub(crate) struct ProviderSpendView {
    operation_id: String,
    provider_id: String,
    capability_id: String,
    state: String,
    spent_tokens: u64,
}

struct RoundSpendAccumulator {
    round: u32,
    epoch: u32,
    reviewers: BTreeMap<String, ReviewerSpendAccumulator>,
}

#[derive(Default)]
struct ReviewerSpendAccumulator {
    attempts: BTreeMap<String, AttemptSpendAccumulator>,
    providers: BTreeMap<String, ProviderSpendAccumulator>,
}

struct AttemptSpendAccumulator {
    outcome: String,
    spent_tokens: u64,
    /// The reservation that bounded this Attempt — its node cap, or the pipeline's.
    reserved: Option<u64>,
    broker_observed_tokens: u64,
    detail: Option<String>,
    terminal: bool,
}

struct ProviderSpendAccumulator {
    provider_id: String,
    capability_id: String,
    state: review_core::ProviderOperationStateV1,
    charged_tokens: u64,
    reserved_tokens: u64,
    failure: bool,
}

pub(crate) fn print_report(options: &ReportOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign)?;
    let store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let ledger = LedgerProjection::rebuild(&store, &cas, &run_id)
        .map_err(|e| e.to_string())?
        .into_ledger();
    print_scope_authority_warnings(&ledger);
    let events = store.replay(&run_id).map_err(|e| e.to_string())?;
    let reports: Vec<_> = events
        .iter()
        .filter(|event| event.event_type.is_run_report())
        .collect();
    let round_authority = report_round_authority(&events)?;
    let rounds = report_rounds(&reports, &round_authority)?;
    let recorded_not_gathered = latest_round_evidence(&events, &cas)?.filter(|evidence| {
        evidence.ledger_was_not_produced() && !evidence.available_node_results.is_empty()
    });
    let mut spend = report_spend(&events, &round_authority)?;
    let wall_rows = store.attempt_wall(&run_id).map_err(|e| e.to_string())?;
    let wall_ms = attach_attempt_wall(&mut spend, &wall_rows);
    let max_parallel = events
        .iter()
        .rev()
        .find(|event| event.event_type == EventType::RoundStartedV1)
        .map(|round| pinned_pipeline_definition(round, &cas))
        .transpose()?
        .map(|definition| {
            definition
                .max_parallel
                .map_or(review_graph::DEFAULT_MAX_PARALLEL, |bound| {
                    usize::try_from(bound).unwrap_or(usize::MAX)
                })
        });
    let findings = ledger.finding_views();
    let view = ReviewReportView {
        schema: "af/review-report@1",
        campaign: options.campaign.clone(),
        runs_recorded: reports.len(),
        ledger_round: ledger.round,
        final_verdict: reports
            .last()
            .map(|event| report_verdict(event))
            .transpose()?,
        rounds,
        spend,
        demands: ledger.demand_views(),
        recorded_not_gathered,
        wall_ms,
        max_parallel,
        findings_summary: findings_summary(&findings),
        findings,
    };

    match options.format {
        ReportFormat::Markdown => print_report_markdown(&view),
        ReportFormat::Text => print_report_text(&view),
        ReportFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&view).map_err(|error| error.to_string())?
        ),
    }
    Ok(())
}

pub(crate) fn report_rounds(
    reports: &[&review_core::RunEvent],
    round_authority: &BTreeMap<String, (u32, u32)>,
) -> Result<Vec<ReportRoundView>, String> {
    reports
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let authority = event
                .causation_id
                .as_deref()
                .and_then(|causation| round_authority.get(causation));
            Ok(ReportRoundView {
                run: index + 1,
                round: authority.map(|(round, _)| *round),
                epoch: authority.map(|(_, epoch)| *epoch),
                verdict: report_verdict(event)?,
                reported_tokens: event.payload.get("spent_tokens").and_then(|v| v.as_u64()),
            })
        })
        .collect()
}

pub(crate) fn report_round_authority(
    events: &[review_core::RunEvent],
) -> Result<BTreeMap<String, (u32, u32)>, String> {
    events
        .iter()
        .filter(|event| event.event_type == EventType::RoundStartedV1)
        .map(|event| {
            let payload: review_core::RoundStartedPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
            Ok((event.event_id.clone(), (payload.round, payload.epoch)))
        })
        .collect()
}

pub(crate) fn report_spend(
    events: &[review_core::RunEvent],
    round_authority: &BTreeMap<String, (u32, u32)>,
) -> Result<Vec<RoundSpendView>, String> {
    let mut rounds: BTreeMap<String, RoundSpendAccumulator> = round_authority
        .iter()
        .map(|(event_id, (round, epoch))| {
            (
                event_id.clone(),
                RoundSpendAccumulator {
                    round: *round,
                    epoch: *epoch,
                    reviewers: BTreeMap::new(),
                },
            )
        })
        .collect();

    for event in events {
        let Some(round_id) = event.causation_id.as_deref() else {
            continue;
        };
        let Some(round) = rounds.get_mut(round_id) else {
            continue;
        };
        match event.event_type {
            EventType::AttemptDispatchedV1 => {
                let payload: review_core::event::AttemptDispatchedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let (node, attempt) = event_attempt_identity(event)?;
                round
                    .reviewers
                    .entry(node.to_string())
                    .or_default()
                    .attempts
                    .insert(
                        attempt.to_string(),
                        AttemptSpendAccumulator {
                            outcome: "running".to_string(),
                            spent_tokens: payload.reserved.unwrap_or(0),
                            reserved: payload.reserved,
                            broker_observed_tokens: 0,
                            detail: None,
                            terminal: false,
                        },
                    );
            }
            EventType::ReviewerExecutionBoundV1 => {
                let binding: review_core::ReviewerExecutionBindingV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let authority = review_core::broker_authority_usage(&binding.operations)?;
                let (node, attempt_id) = event_attempt_identity(event)?;
                let attempt = round
                    .reviewers
                    .get_mut(node)
                    .and_then(|reviewer| reviewer.attempts.get_mut(attempt_id))
                    .ok_or("Reviewer Execution Binding has no dispatched Attempt")?;
                if !attempt.terminal {
                    attempt.spent_tokens = attempt.spent_tokens.max(authority);
                }
            }
            EventType::BrokerOperationCompletedV1 => {
                let receipt: review_core::BrokerOperationReceiptV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let (node, attempt_id) = event_attempt_identity(event)?;
                let attempt = round
                    .reviewers
                    .get_mut(node)
                    .and_then(|reviewer| reviewer.attempts.get_mut(attempt_id))
                    .ok_or("Broker operation receipt has no dispatched Attempt")?;
                attempt.broker_observed_tokens = attempt
                    .broker_observed_tokens
                    .checked_add(receipt.charged_usage)
                    .ok_or("reported Broker spend overflow")?;
                attempt.spent_tokens = attempt.spent_tokens.max(attempt.broker_observed_tokens);
            }
            EventType::AttemptAdmittedV1 => {
                let payload: review_core::event::AttemptAdmittedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                settle_attempt(round, event, payload.selection, payload.cost_tokens, None)?;
            }
            EventType::AttemptFailedV1 => {
                let payload: review_core::event::AttemptFailedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                settle_attempt(
                    round,
                    event,
                    "failed".to_string(),
                    payload.charged.unwrap_or(0),
                    Some(payload.error),
                )?;
            }
            EventType::AttemptFencedV1 => {
                let payload: review_core::event::AttemptFencedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                settle_attempt(
                    round,
                    event,
                    "fenced".to_string(),
                    payload.charged.unwrap_or(0),
                    Some(payload.reason),
                )?;
            }
            EventType::AttemptReleasedV1 => {
                let payload: review_core::event::AttemptReleasedPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                settle_attempt(round, event, "released".to_string(), 0, Some(payload.error))?;
            }
            EventType::ProviderOperationTransitionV1 => {
                let payload: review_core::ProviderOperationTransitionPayloadV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let reviewer = round.reviewers.entry(payload.node_id.clone()).or_default();
                let operation = reviewer
                    .providers
                    .entry(payload.operation_id.clone())
                    .or_insert_with(|| ProviderSpendAccumulator {
                        provider_id: payload.provider_id.clone(),
                        capability_id: payload.capability_id.clone(),
                        state: payload.state,
                        charged_tokens: 0,
                        reserved_tokens: 0,
                        failure: false,
                    });
                operation.charged_tokens = operation
                    .charged_tokens
                    .checked_add(payload.charged_tokens)
                    .ok_or("reported Provider spend overflow")?;
                operation.state = payload.state;
                operation.reserved_tokens = payload.reserved_tokens;
                operation.failure = payload.failure_class.is_some();
            }
            _ => {}
        }
    }

    let mut views = rounds
        .into_values()
        .map(round_spend_view)
        .collect::<Result<Vec<_>, String>>()?;
    views.sort_by_key(|view| (view.round, view.epoch));
    Ok(views)
}

fn event_attempt_identity(event: &review_core::RunEvent) -> Result<(&str, &str), String> {
    Ok((
        event
            .node_id
            .as_deref()
            .ok_or_else(|| format!("{} has no reviewer node", event.event_type))?,
        event
            .attempt_id
            .as_deref()
            .ok_or_else(|| format!("{} has no Attempt ID", event.event_type))?,
    ))
}

fn settle_attempt(
    round: &mut RoundSpendAccumulator,
    event: &review_core::RunEvent,
    outcome: String,
    spent_tokens: u64,
    detail: Option<String>,
) -> Result<(), String> {
    let (node, attempt_id) = event_attempt_identity(event)?;
    let attempt = round
        .reviewers
        .entry(node.to_string())
        .or_default()
        .attempts
        .entry(attempt_id.to_string())
        .or_insert_with(|| AttemptSpendAccumulator {
            outcome: "running".to_string(),
            spent_tokens: 0,
            reserved: None,
            broker_observed_tokens: 0,
            detail: None,
            terminal: false,
        });
    if attempt.terminal {
        // A late response to an already-fenced Attempt is durably quarantined but must not be
        // charged twice. The first terminal lifecycle event owns its operator-visible outcome.
        return Ok(());
    }
    attempt.outcome = outcome;
    attempt.spent_tokens = spent_tokens.max(attempt.broker_observed_tokens);
    attempt.detail = detail;
    attempt.terminal = true;
    Ok(())
}

fn round_spend_view(round: RoundSpendAccumulator) -> Result<RoundSpendView, String> {
    let mut spent_tokens = 0_u64;
    let mut reviewers = Vec::new();
    for (reviewer, accumulator) in round.reviewers {
        let attempts = accumulator
            .attempts
            .into_iter()
            .map(|(attempt_id, attempt)| AttemptSpendView {
                attempt_id,
                outcome: attempt.outcome,
                spent_tokens: attempt.spent_tokens,
                detail: attempt.detail,
                reserved: attempt.reserved,
                wall: None,
            })
            .collect::<Vec<_>>();
        let attempt_tokens = attempts.iter().try_fold(0_u64, |sum, attempt| {
            sum.checked_add(attempt.spent_tokens)
                .ok_or("reported Attempt spend overflow")
        })?;
        let provider_operations = accumulator
            .providers
            .into_iter()
            .map(|(operation_id, provider)| {
                let outstanding = if provider.state
                    == review_core::ProviderOperationStateV1::Running
                    && !provider.failure
                {
                    provider.reserved_tokens
                } else {
                    0
                };
                Ok(ProviderSpendView {
                    operation_id,
                    provider_id: provider.provider_id,
                    capability_id: provider.capability_id,
                    state: provider_state_label(provider.state).to_string(),
                    spent_tokens: provider
                        .charged_tokens
                        .checked_add(outstanding)
                        .ok_or("reported Provider spend overflow")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let provider_tokens = provider_operations
            .iter()
            .try_fold(0_u64, |sum, operation| {
                sum.checked_add(operation.spent_tokens)
                    .ok_or("reported Provider spend overflow")
            })?;
        let reviewer_tokens = attempt_tokens
            .checked_add(provider_tokens)
            .ok_or("reported reviewer spend overflow")?;
        spent_tokens = spent_tokens
            .checked_add(reviewer_tokens)
            .ok_or("reported Round spend overflow")?;
        reviewers.push(ReviewerSpendView {
            reviewer,
            spent_tokens: reviewer_tokens,
            attempt_tokens,
            provider_tokens,
            attempts,
            provider_operations,
        });
    }
    Ok(RoundSpendView {
        round: round.round,
        epoch: round.epoch,
        spent_tokens,
        wall_ms: None,
        reviewers,
    })
}

fn print_report_text(report: &ReviewReportView) {
    println!("Review campaign: {}", report.campaign);
    println!("Runs recorded: {}", report.runs_recorded);
    println!("Ledger round: {}", report.ledger_round);
    println!(
        "Final verdict: {}",
        report.final_verdict.as_deref().unwrap_or("not recorded")
    );
    if let Some(wall) = report.wall_ms {
        println!("Wall-clock: {}", human_duration(wall));
    }
    if let Some(max_parallel) = report.max_parallel {
        println!("Parallel: {max_parallel} Workers at once (pinned pipeline max_parallel)");
    }
    println!(
        "Findings: {}",
        findings_summary_line(&report.findings_summary)
    );
    println!("Rounds:");
    if report.rounds.is_empty() {
        println!("  none");
    }
    for round in &report.rounds {
        println!(
            "  run {} (round {} epoch {}): {}; reported tokens {}",
            round.run,
            optional_number(round.round),
            optional_number(round.epoch),
            round.verdict,
            optional_tokens(round.reported_tokens)
        );
    }
    println!("Spend:");
    for round in &report.spend {
        println!(
            "  round {} epoch {}: {} tokens{}",
            round.round,
            round.epoch,
            round.spent_tokens,
            round
                .wall_ms
                .map(|ms| format!(", {}", human_duration(ms)))
                .unwrap_or_default()
        );
        for reviewer in &round.reviewers {
            println!(
                "    {}: {} tokens (attempts {}, providers {})",
                reviewer.reviewer,
                reviewer.spent_tokens,
                reviewer.attempt_tokens,
                reviewer.provider_tokens
            );
            for attempt in &reviewer.attempts {
                println!(
                    "      attempt {}: {}, {} tokens{}{}{}",
                    attempt.attempt_id,
                    attempt.outcome,
                    attempt.spent_tokens,
                    attempt
                        .reserved
                        .map(|cap| format!(" (cap {cap})"))
                        .unwrap_or_default(),
                    attempt_wall_suffix(attempt.wall.as_ref()),
                    attempt
                        .detail
                        .as_deref()
                        .map(|detail| format!(" - {}", one_line(detail)))
                        .unwrap_or_default()
                );
            }
        }
    }
    println!("Demands:");
    if report.demands.is_empty() {
        println!("  none");
    }
    for demand in &report.demands {
        println!(
            "  [{}; {}] {} ({})",
            demand_requirement_label(demand.requirement),
            demand_status_label(demand.status),
            one_line(&demand.claim),
            demand.demand_id
        );
        println!("    why: {}", one_line(&demand.why));
        println!(
            "    suggested method: {}",
            one_line(&demand.suggested_method)
        );
        println!("    source: {}", demand.source);
    }
    print_recorded_not_gathered_text(report.recorded_not_gathered.as_ref());
    println!("Findings:");
    if report.findings.is_empty() {
        println!("  none");
    }
    for finding in &report.findings {
        println!(
            "  [{}; scope={}; severity={}; effective={}] {} ({}) at {}:{}",
            finding.status.as_str(),
            finding.convergence_scope_label(),
            severity_label(finding.severity),
            finding
                .convergence_severity
                .map(severity_label)
                .unwrap_or("-"),
            one_line(&finding.title),
            finding.key,
            finding.file,
            finding
                .line
                .map_or("-".to_string(), |line| line.to_string())
        );
        println!("    body: {}", one_line(&finding.body));
        println!(
            "    fix: {}",
            one_line(
                finding
                    .fix
                    .as_deref()
                    .unwrap_or("unavailable: artifact-less legacy import")
            )
        );
        for transition in &finding.history {
            println!(
                "    history round {}: {} - {}",
                transition.round,
                transition_label(transition.kind),
                one_line(transition.note.as_deref().unwrap_or("(no note)"))
            );
        }
    }
}

fn print_report_markdown(report: &ReviewReportView) {
    println!("# Review campaign `{}`", report.campaign);
    println!();
    println!("- Runs recorded: {}", report.runs_recorded);
    println!("- Ledger round: {}", report.ledger_round);
    println!(
        "- Final verdict: {}",
        report.final_verdict.as_deref().unwrap_or("not recorded")
    );
    if let Some(wall) = report.wall_ms {
        println!("- Wall-clock: {}", human_duration(wall));
    }
    if let Some(max_parallel) = report.max_parallel {
        println!("- Parallel: {max_parallel} Workers at once (pinned pipeline max_parallel)");
    }
    println!(
        "- Findings: {}",
        findings_summary_line(&report.findings_summary)
    );
    println!();
    println!("## Runs");
    println!();
    println!("| Run | Round | Epoch | Verdict | Tokens |");
    println!("| ---: | ---: | ---: | --- | ---: |");
    for round in &report.rounds {
        println!(
            "| {} | {} | {} | {} | {} |",
            round.run,
            optional_number(round.round),
            optional_number(round.epoch),
            round.verdict,
            optional_tokens(round.reported_tokens)
        );
    }
    println!();
    println!("## Spend");
    println!();
    println!(
        "| Round | Epoch | Reviewer | Attempt tokens | Provider tokens | Total tokens | Round wall |"
    );
    println!("| ---: | ---: | --- | ---: | ---: | ---: | ---: |");
    for round in &report.spend {
        let wall = round
            .wall_ms
            .map(human_duration)
            .unwrap_or_else(|| "-".to_string());
        if round.reviewers.is_empty() {
            println!(
                "| {} | {} | - | 0 | 0 | 0 | {wall} |",
                round.round, round.epoch
            );
        }
        for reviewer in &round.reviewers {
            println!(
                "| {} | {} | {} | {} | {} | {} | {wall} |",
                round.round,
                round.epoch,
                reviewer.reviewer,
                reviewer.attempt_tokens,
                reviewer.provider_tokens,
                reviewer.spent_tokens
            );
        }
    }
    println!();
    println!("### Attempts");
    for round in &report.spend {
        for reviewer in &round.reviewers {
            for attempt in &reviewer.attempts {
                println!();
                println!(
                    "- Round {}, **{}**, Attempt `{}`: {}, {} tokens{}{}{}",
                    round.round,
                    reviewer.reviewer,
                    attempt.attempt_id,
                    attempt.outcome,
                    attempt.spent_tokens,
                    attempt
                        .reserved
                        .map(|cap| format!(" (cap {cap})"))
                        .unwrap_or_default(),
                    attempt_wall_suffix(attempt.wall.as_ref()),
                    attempt
                        .detail
                        .as_deref()
                        .map(|detail| format!(" — {}", markdown_line(detail)))
                        .unwrap_or_default()
                );
            }
        }
    }
    println!();
    println!("## Demands");
    if report.demands.is_empty() {
        println!();
        println!("None.");
    }
    for demand in &report.demands {
        println!();
        println!(
            "- **[{}, {}] {}** (`{}`)",
            demand_requirement_label(demand.requirement),
            demand_status_label(demand.status),
            demand.claim,
            demand.demand_id
        );
        println!("  - Why: {}", markdown_line(&demand.why));
        println!(
            "  - Suggested method: {}",
            markdown_line(&demand.suggested_method)
        );
        println!("  - Source: {}", demand.source);
    }
    print_recorded_not_gathered_markdown(report.recorded_not_gathered.as_ref());
    println!();
    println!("## Findings");
    for effective_severity in [
        Some(Severity::Blocker),
        Some(Severity::Major),
        Some(Severity::Minor),
        None,
    ] {
        println!();
        let heading =
            effective_severity.map_or("Recorded, not blocking this Subject", |severity| {
                match severity {
                    Severity::Blocker => "Blocker",
                    Severity::Major => "Major",
                    Severity::Minor => "Minor",
                }
            });
        println!("### {heading}");
        let matching = report
            .findings
            .iter()
            .filter(|finding| finding.convergence_severity == effective_severity)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            println!();
            println!("None.");
            continue;
        }
        for finding in matching {
            println!();
            println!(
                "- **[{}, scope={}, severity={}, effective={}] {}** (`{}`) at `{}:{}`",
                finding.status.as_str(),
                finding.convergence_scope_label(),
                severity_label(finding.severity),
                finding
                    .convergence_severity
                    .map(severity_label)
                    .unwrap_or("-"),
                finding.title,
                finding.key,
                finding.file,
                finding
                    .line
                    .map_or("-".to_string(), |line| line.to_string())
            );
            println!("  - Body: {}", markdown_line(&finding.body));
            println!(
                "  - Fix: {}",
                markdown_line(
                    finding
                        .fix
                        .as_deref()
                        .unwrap_or("unavailable: artifact-less legacy import")
                )
            );
            let evidence = finding
                .reports
                .iter()
                .map(|attached| {
                    if attached.report_id.is_empty() {
                        format!(
                            "{} round {} scope={} at {}:{} (legacy import)",
                            attached.source,
                            attached.round,
                            attached.scope_label(),
                            attached.file,
                            attached
                                .line
                                .map_or("-".to_string(), |line| line.to_string())
                        )
                    } else {
                        format!(
                            "{} round {} scope={} at {}:{} `{}`",
                            attached.source,
                            attached.round,
                            attached.scope_label(),
                            attached.file,
                            attached
                                .line
                                .map_or("-".to_string(), |line| line.to_string()),
                            attached.report_id
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            println!("  - Reports: {evidence}");
            for transition in &finding.history {
                println!(
                    "  - Resolution/history, round {}: {} - {}",
                    transition.round,
                    transition_label(transition.kind),
                    markdown_line(transition.note.as_deref().unwrap_or("(no note)"))
                );
            }
        }
    }
}

fn print_recorded_not_gathered_text(evidence: Option<&LatestRoundEvidence>) {
    let Some(evidence) = evidence else { return };
    println!("Recorded, not gathered:");
    println!("  reason: {}", evidence.absence_reason());
    for result in &evidence.available_node_results {
        println!(
            "  {} attempt {}: result {}, {} tokens, severities {}",
            result.node,
            result.attempt_id,
            result.result_artifact_id,
            result.spend_tokens,
            if result.severities.is_empty() {
                "none recorded".to_string()
            } else {
                result.severities.join(", ")
            }
        );
    }
}

fn print_recorded_not_gathered_markdown(evidence: Option<&LatestRoundEvidence>) {
    let Some(evidence) = evidence else { return };
    println!();
    println!("## Recorded, not gathered");
    println!();
    println!(
        "The latest Round did not produce a Ledger because {}. These admitted results remain evidence only; they are not Findings, a clean Ledger, or convergence input.",
        evidence.absence_reason()
    );
    for result in &evidence.available_node_results {
        println!();
        println!(
            "- **{}**, Attempt `{}`, result `{}`, spend {} tokens, severities: {}",
            result.node,
            result.attempt_id,
            result.result_artifact_id,
            result.spend_tokens,
            if result.severities.is_empty() {
                "none recorded".to_string()
            } else {
                result.severities.join(", ")
            }
        );
        for finding in &result.findings {
            println!(
                "  - [{}] {} — {}",
                finding["severity"].as_str().unwrap_or("unknown"),
                markdown_line(finding["title"].as_str().unwrap_or("untitled finding")),
                markdown_line(finding["body"].as_str().unwrap_or("(no body)"))
            );
        }
    }
}

fn provider_state_label(state: review_core::ProviderOperationStateV1) -> &'static str {
    match state {
        review_core::ProviderOperationStateV1::Running => "running",
        review_core::ProviderOperationStateV1::WaitingForHuman => "waiting_for_human",
        review_core::ProviderOperationStateV1::Resumed => "resumed",
        review_core::ProviderOperationStateV1::Done => "done",
        review_core::ProviderOperationStateV1::Failed => "failed",
    }
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Minor => "minor",
        Severity::Major => "major",
        Severity::Blocker => "blocker",
    }
}

fn demand_requirement_label(requirement: review_core::DemandRequirement) -> &'static str {
    match requirement {
        review_core::DemandRequirement::Required => "required",
        review_core::DemandRequirement::Advisory => "advisory",
    }
}

fn demand_status_label(status: review_core::DemandStatus) -> &'static str {
    match status {
        review_core::DemandStatus::Open => "open",
        review_core::DemandStatus::Satisfied => "satisfied",
        review_core::DemandStatus::Stale => "stale",
        review_core::DemandStatus::Waived => "waived",
    }
}

fn transition_label(kind: review_store::ledger::TransitionKind) -> String {
    match kind {
        review_store::ledger::TransitionKind::Reported => "reported".to_string(),
        review_store::ledger::TransitionKind::Duplicate => "duplicate".to_string(),
        review_store::ledger::TransitionKind::Escalated => "escalated".to_string(),
        review_store::ledger::TransitionKind::Reopened => "reopened".to_string(),
        review_store::ledger::TransitionKind::AdoptedWhileDeclined => {
            "adopted_while_declined".to_string()
        }
        review_store::ledger::TransitionKind::AuthorityRecovered => {
            "authority_recovered".to_string()
        }
        review_store::ledger::TransitionKind::Attested => "attested".to_string(),
        review_store::ledger::TransitionKind::Challenged => "challenged".to_string(),
        review_store::ledger::TransitionKind::Resolved(status) => {
            format!("resolved:{}", status.as_str())
        }
    }
}

pub(crate) fn optional_tokens(tokens: Option<u64>) -> String {
    tokens.map_or_else(|| "-".to_string(), |tokens| tokens.to_string())
}

pub(crate) fn optional_number(number: Option<u32>) -> String {
    number.map_or_else(|| "-".to_string(), |number| number.to_string())
}

pub(crate) fn last_closed_summary(
    round: Option<u32>,
    epoch: Option<u32>,
    verdict: Option<&str>,
) -> Option<String> {
    verdict.map(|verdict| {
        format!(
            "round {} epoch {}; {verdict}",
            optional_number(round),
            optional_number(epoch)
        )
    })
}

fn one_line(value: &str) -> String {
    value.lines().collect::<Vec<_>>().join(" ")
}

fn report_verdict(event: &review_core::RunEvent) -> Result<String, String> {
    match event.event_type {
        EventType::RunReportV1 => event.payload["verdict"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "RunReport@1 has no string verdict".to_string()),
        EventType::RunReportV2 => {
            let report: RunReportPayloadV2 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(match report.verdict {
                RunVerdictV2::Pass => "pass".to_string(),
                RunVerdictV2::Fail {
                    reason: RunFailureReasonV2::NotConverged,
                } => "fail (not_converged)".to_string(),
                RunVerdictV2::Fail {
                    reason: RunFailureReasonV2::Exhausted,
                } => "fail (exhausted)".to_string(),
                RunVerdictV2::Incomplete { missing_nodes } => {
                    format!("incomplete ({} missing nodes)", missing_nodes.len())
                }
            })
        }
        EventType::RunReportV3 => {
            let report: RunReportPayloadV3 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(render_verdict_v3(report.verdict))
        }
        EventType::RunReportV4 => {
            let report: RunReportPayloadV4 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(render_verdict_v3(report.verdict))
        }
        EventType::RunReportV5 => {
            let report: RunReportPayloadV5 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(render_verdict_v3(report.verdict))
        }
        _ => Err(format!("{} is not a run report", event.event_type)),
    }
}

fn render_verdict_v3(verdict: RunVerdictV3) -> String {
    match verdict {
        RunVerdictV3::Pass => "pass".to_string(),
        RunVerdictV3::Fail {
            reason: RunFailureReasonV3::NotConverged,
        } => "fail (not_converged)".to_string(),
        RunVerdictV3::Fail {
            reason: RunFailureReasonV3::AuthorityUnavailable,
        } => "fail (authority_unavailable)".to_string(),
        RunVerdictV3::Fail {
            reason: RunFailureReasonV3::Exhausted,
        } => "fail (exhausted)".to_string(),
        RunVerdictV3::Incomplete { missing_nodes } => {
            format!("incomplete ({} missing nodes)", missing_nodes.len())
        }
    }
}

fn markdown_line(value: &str) -> String {
    value.lines().collect::<Vec<_>>().join(" ")
}

pub(crate) fn print_scope_authority_warnings(ledger: &Ledger) {
    for failure in ledger.scope_authority_failures() {
        match failure.authority {
            review_store::ScopeAuthorityKind::RoundBinding => eprintln!(
                "warning: round {} Report Scope is unknown: round binding disagrees for Subject {}: {}",
                failure.round, failure.authority_id, failure.reason
            ),
            authority => eprintln!(
                "warning: round {} Report Scope is unknown: {:?} authority {} is unavailable: {}",
                failure.round, authority, failure.authority_id, failure.reason
            ),
        }
    }
}
