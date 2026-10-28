//! The findings handoff of `af review report` (ADR-0248): each Campaign finding with the prompt
//! that hands it to the agent that will fix it and, only when a positive `FixVerification@1`
//! backs its fixed Resolution, the verified fix; and what the last recorded run covered, per
//! reviewer node of the pinned plan and per path of its Round's Subject.

use std::collections::{BTreeMap, HashMap};

use review_core::reviewer_result::CHANGE_WIDE_SENTINEL;
use review_core::task::handoff::{
    self, CoverageV1, FindingPromptV1, PromptSource, VerifiedBy, VerifiedFixV1,
};
use review_core::{EventType, RunEvent, RunNodeOutcomeV2, RunReportPayloadV6};
use review_store::{Cas, Ledger, ResolvedSubjectScope, Status};

/// One finding of `af/review-report@5`: the Ledger's view, its agent prompt, and its verified
/// fix, null unless a verification record proves it.
#[derive(serde::Serialize)]
pub(crate) struct ReportFindingView {
    #[serde(flatten)]
    pub(crate) finding: review_store::Finding,
    pub(crate) agent_prompt: FindingPromptV1,
    pub(crate) verified_fix: Option<VerifiedFixV1>,
}

/// What the last recorded run covered: its place among the runs, its Round, and the coverage.
/// The report's `coverage` is null when no run is recorded, or its Round's Subject is
/// unreadable.
#[derive(serde::Serialize)]
pub(crate) struct ReportCoverageView {
    pub(crate) run: usize,
    pub(crate) round: u32,
    /// The head Snapshot of the Round's Subject.
    pub(crate) snapshot_id: String,
    #[serde(flatten)]
    pub(crate) coverage: CoverageV1,
}

/// The Campaign's Rounds by their start event, and each Round's Subject at its latest epoch.
struct Rounds {
    by_event: HashMap<String, (u32, String)>,
    by_round: BTreeMap<u32, (u32, String)>,
    subjects: HashMap<String, ResolvedSubjectScope>,
}

impl Rounds {
    fn read(cas: &Cas, events: &[RunEvent]) -> Result<Self, String> {
        let mut rounds = Rounds {
            by_event: HashMap::new(),
            by_round: BTreeMap::new(),
            subjects: HashMap::new(),
        };
        for event in events {
            if event.event_type != EventType::RoundStartedV1 {
                continue;
            }
            let started: review_core::RoundStartedPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            rounds.by_event.insert(
                event.event_id.clone(),
                (started.round, started.subject_id.clone()),
            );
            let latest = rounds
                .by_round
                .entry(started.round)
                .or_insert((started.epoch, started.subject_id.clone()));
            if started.epoch >= latest.0 {
                *latest = (started.epoch, started.subject_id.clone());
            }
            // A Subject the CAS cannot resolve leaves its Round without a Snapshot here: its
            // findings fall back to the active head, and a run on it reports no coverage.
            if let std::collections::hash_map::Entry::Vacant(entry) =
                rounds.subjects.entry(started.subject_id.clone())
                && let Ok(resolved) = review_store::resolve_subject_scope(cas, &started.subject_id)
            {
                entry.insert(resolved);
            }
        }
        Ok(rounds)
    }

    /// The head Snapshot of the Round a finding was last seen in.
    fn snapshot(&self, round: u32) -> Option<&str> {
        let (_, subject) = self.by_round.get(&round)?;
        Some(
            self.subjects
                .get(subject)?
                .subject
                .head_snapshot_id
                .as_str(),
        )
    }
}

/// A finding's file as a prompt names it: none for a change-wide or unrecorded location.
fn place(file: &str) -> Option<&str> {
    let file = file.trim();
    (!file.is_empty() && file != CHANGE_WIDE_SENTINEL).then_some(file)
}

/// The fix of a finding a verification record proves: its fixed Resolution names a positive
/// `FixVerification@1` for the same finding, on a Subject whose head Snapshot is readable.
fn verified_fix(
    cas: &Cas,
    ledger: &Ledger,
    finding: &review_store::Finding,
) -> Option<VerifiedFixV1> {
    if finding.status != Status::Fixed {
        return None;
    }
    let resolution = ledger.resolution(&finding.key)?;
    let verification = ledger.verification(resolution.resolution.verification_id.as_deref()?)?;
    if !verification.verification.positive
        || verification.verification.finding_id != resolution.resolution.finding_id
    {
        return None;
    }
    let subject =
        review_store::resolve_subject_scope(cas, &verification.verification.subject_id).ok()?;
    let fix = VerifiedFixV1 {
        finding_id: finding.key.clone(),
        record_id: verification.artifact_id.clone(),
        snapshot_id: subject.subject.head_snapshot_id,
    };
    fix.validate().ok().map(|()| fix)
}

/// The reviewer nodes of the Campaign's pinned plan: its `reviewer` and `scatter` nodes.
fn reviewer_nodes(cas: &Cas, events: &[RunEvent]) -> Result<Vec<String>, String> {
    let Some(opened) = events
        .iter()
        .find(|event| event.event_type == EventType::CampaignOpenedV1)
    else {
        return Ok(Vec::new());
    };
    let opened: review_core::CampaignOpenedPayloadV1 =
        serde_json::from_value(opened.payload.clone()).map_err(|e| e.to_string())?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&opened.campaign_manifest_id)
            .map_err(|e| format!("the Campaign's pinned manifest is unreadable: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let bytes = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|e| format!("the Campaign's pinned pipeline is unreadable: {e}"))?;
    let definition = review_config::Definition::from_toml(
        std::str::from_utf8(&bytes).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("the Campaign's pinned pipeline is invalid: {e}"))?;
    Ok(definition
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                review_config::NodeKindSpec::Reviewer | review_config::NodeKindSpec::Scatter
            )
        })
        .map(|node| node.id.clone())
        .collect())
}

/// Why a reviewer node of the plan has no result in a run.
fn not_run(outcome: Option<&RunNodeOutcomeV2>) -> Option<String> {
    match outcome {
        Some(RunNodeOutcomeV2::Completed { .. }) => None,
        Some(RunNodeOutcomeV2::Failed { error }) => Some(format!("it failed: {error}")),
        Some(RunNodeOutcomeV2::Suppressed { reason }) => Some(format!(
            "it was suppressed ({}): a node it reads produced nothing",
            serde_json::to_value(reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        )),
        None => Some("the run recorded no outcome for it".into()),
    }
}

/// The report's findings with their handoff, and what its last run covered.
pub(crate) fn read(
    cas: &Cas,
    events: &[RunEvent],
    ledger: &Ledger,
    findings: Vec<review_store::Finding>,
) -> Result<(Vec<ReportFindingView>, Option<ReportCoverageView>), String> {
    let rounds = Rounds::read(cas, events)?;
    let active = ledger.active_head_snapshot_id();
    let mut views = Vec::with_capacity(findings.len());
    for finding in findings {
        let snapshot = rounds
            .snapshot(finding.last_seen_round)
            .or(active)
            .ok_or_else(|| {
                format!(
                    "finding {} names no reviewed Snapshot: its Round {} has no readable Subject",
                    finding.key, finding.last_seen_round
                )
            })?
            .to_owned();
        let agent_prompt = handoff::prompt(
            &PromptSource {
                reviewer: &finding.source,
                severity: finding.severity,
                title: &finding.title,
                file: place(&finding.file),
                line: finding.line,
                snapshot_id: &snapshot,
                evidence: &finding.body,
                fix: Some(finding.fix.as_str()).filter(|fix| !fix.trim().is_empty()),
            },
            VerifiedBy::Campaign,
        );
        let verified_fix = verified_fix(cas, ledger, &finding);
        views.push(ReportFindingView {
            finding,
            agent_prompt,
            verified_fix,
        });
    }
    let reports: Vec<&RunEvent> = events
        .iter()
        .filter(|event| event.event_type.is_run_report())
        .collect();
    let Some(last) = reports.last() else {
        return Ok((views, None));
    };
    let Some((round, subject)) = last
        .causation_id
        .as_deref()
        .and_then(|cause| rounds.by_event.get(cause))
    else {
        return Ok((views, None));
    };
    let report: RunReportPayloadV6 =
        serde_json::from_value(last.payload.clone()).map_err(|e| e.to_string())?;
    let outcomes: HashMap<&str, &RunNodeOutcomeV2> = report
        .outcomes
        .iter()
        .map(|outcome| (outcome.node.as_str(), &outcome.outcome))
        .collect();
    let reviewers = reviewer_nodes(cas, events)?
        .into_iter()
        .map(|node| {
            let reason = not_run(outcomes.get(node.as_str()).copied());
            (node, reason)
        })
        .collect();
    let Some(resolved) = rounds.subjects.get(subject) else {
        return Ok((views, None));
    };
    let changed = resolved.changed_paths.clone();
    let coverage = handoff::coverage(
        changed.as_deref(),
        reviewers,
        views
            .iter()
            .filter(|view| view.finding.last_seen_round == *round)
            .filter_map(|view| place(&view.finding.file)),
    );
    coverage.validate()?;
    Ok((
        views,
        Some(ReportCoverageView {
            run: reports.len(),
            round: *round,
            snapshot_id: resolved.subject.head_snapshot_id.clone(),
            coverage,
        }),
    ))
}

/// The coverage as text: a heading line, then one line per reviewer and one of the paths.
pub(crate) fn coverage_lines(coverage: Option<&ReportCoverageView>) -> Vec<String> {
    use handoff::{PathCoverageStateV1 as Path, ReviewerCoverageStateV1 as Reviewer};
    let Some(view) = coverage else {
        return vec![
            "Coverage:".into(),
            "  not reviewed: no recorded run of a Round with a readable Subject".into(),
        ];
    };
    let mut lines = vec![format!(
        "Coverage (run {}, round {}):",
        view.run, view.round
    )];
    if view.coverage.reviewers.is_empty() {
        lines.push("  not reviewed: the pinned plan has no reviewer".into());
    }
    for reviewer in &view.coverage.reviewers {
        let name = crate::terminal_text(&reviewer.reviewer);
        lines.push(match (reviewer.state, &reviewer.reason) {
            (Reviewer::Reviewed, _) => format!("  reviewer {name}: reviewed"),
            (Reviewer::NotRun, reason) => format!(
                "  reviewer {name}: not reviewed, it did not run: {}",
                crate::terminal_text(reason.as_deref().unwrap_or("no result recorded"))
            ),
        });
    }
    let parts: Vec<String> = [
        (Path::Reviewed, "reviewed"),
        (Path::PartiallyReviewed, "partially reviewed"),
        (Path::NotReviewed, "not reviewed"),
        (Path::OutsideRound, "outside the round"),
    ]
    .into_iter()
    .filter_map(|(state, word)| {
        let count = view
            .coverage
            .paths
            .iter()
            .filter(|path| path.state == state)
            .count();
        (count > 0).then(|| format!("{count} {word}"))
    })
    .collect();
    if !parts.is_empty() {
        lines.push(format!("  paths: {}", parts.join(", ")));
    }
    lines
}
