//! `af task findings` (ADR-0211): the review findings of a Task's last (or Nth) review round,
//! per reviewer — each with its severity, confidence, title, location, the quoted evidence and
//! the proposed fix — then the evaluator's verdict and its reason. Text is grouped by severity;
//! `--json` prints one `af/task-findings@2` document, whose findings each carry the prompt an
//! agent that will fix them needs and whose round says what it covered (ADR-0248); `--format
//! sarif` prints the same findings as SARIF 2.1.0. Read-only: it reads what `af task report`
//! reads, the Task's recorded rounds, their Subjects, Finding Sets, reviewer results and repair
//! assessments, and the verdict the Task log recorded after the round, from the Store.

use super::*;
use review_core::Severity;
use review_core::finding_set::FindingSetV1;
use review_core::reviewer_result::{CHANGE_WIDE_SENTINEL, ReviewerStageOutput};
use review_core::task::agent_loop::*;
use review_core::task::handoff::{self, PromptSource, VerifiedBy, VerifiedFixV1};
use review_core::task::repair::{TASK_REVIEW_CONTINUATION_V1, TaskReviewContinuationV1};
use review_core::task::review::VerificationOutcomeV1;
use review_core::task::review_context::{TASK_REVIEW_SUBJECT_V2, TaskReviewSubjectV2};
use review_core::task::verification::{TASK_EVALUATION_V1, TaskEvaluationV1};

/// Bound a reviewer's text to what a document carries.
fn bounded(text: &str) -> String {
    let clean = text.trim();
    if clean.chars().count() <= MAX_TEXT {
        clean.to_owned()
    } else {
        let mut cut: String = clean.chars().take(MAX_TEXT - 1).collect();
        cut.push('…');
        cut
    }
}

/// The findings of one complete round: the Finding Set entries that round saw itself, by the
/// reviewer that reported them.
fn gathered(
    set: &FindingSetV1,
    snapshot_id: &str,
) -> Result<BTreeMap<String, Vec<TaskFindingV2>>, String> {
    set.validate()?;
    let mut by_reviewer: BTreeMap<String, Vec<TaskFindingV2>> = BTreeMap::new();
    for entry in &set.findings {
        if entry.last_seen_round != set.round {
            continue;
        }
        by_reviewer
            .entry(entry.source.clone())
            .or_default()
            .push(prompted(
                &entry.source,
                snapshot_id,
                TaskFindingV2 {
                    finding_id: Some(entry.finding_id.clone()),
                    severity: entry.severity,
                    status: Some(entry.status.clone()),
                    confidence: entry.confidence,
                    title: bounded(&entry.title),
                    file: entry.file.as_deref().and_then(place),
                    line: entry.line.filter(|line| *line > 0),
                    evidence: bounded(&entry.body),
                    fix: entry.fix.as_deref().map(bounded),
                    agent_prompt: placeholder(),
                },
            ));
    }
    Ok(by_reviewer)
}

/// An empty prompt [`prompted`] replaces before anything reads it.
fn placeholder() -> handoff::FindingPromptV1 {
    handoff::FindingPromptV1 {
        location: handoff::FindingLocationV1 {
            file: None,
            line: None,
            snapshot_id: String::new(),
        },
        evidence: String::new(),
        requested_change: String::new(),
        verification: String::new(),
        text: String::new(),
    }
}

/// The finding with its agent prompt (ADR-0248): its own location on the round's Snapshot, its
/// evidence and fix, and how a later round of the Task judges a fix.
fn prompted(reviewer: &str, snapshot_id: &str, mut finding: TaskFindingV2) -> TaskFindingV2 {
    finding.agent_prompt = handoff::prompt(
        &PromptSource {
            reviewer,
            severity: finding.severity,
            title: &finding.title,
            file: finding.file.as_deref(),
            line: finding.line,
            snapshot_id,
            evidence: &finding.evidence,
            fix: finding.fix.as_deref(),
        },
        VerifiedBy::Task,
    );
    finding
}

/// The reports of one reviewer's recorded result, for a round whose reduce step did not run:
/// shown as recorded, never as gathered evidence (ADR-0034). A report without a title or a body
/// keeps its place under a placeholder, so its position, the ID a continuation names it by,
/// never shifts.
fn recorded(
    cas: &Cas,
    name: &str,
    result_id: &str,
    snapshot_id: &str,
) -> Result<Vec<TaskFindingV2>, String> {
    let envelope = cas
        .get_artifact(result_id)
        .map_err(|error| format!("the round's {name} result {result_id} is unreadable: {error}"))?;
    validate_envelope(&envelope)?;
    let stage: ReviewerStageOutput = serde_json::from_value(envelope.payload)
        .map_err(|error| format!("the round's {name} result is not a reviewer result: {error}"))?;
    let text = |value: &str, empty: &str| match bounded(value) {
        cut if cut.is_empty() => empty.to_owned(),
        cut => cut,
    };
    Ok(stage
        .reports
        .into_iter()
        .map(|report| {
            prompted(
                name,
                snapshot_id,
                TaskFindingV2 {
                    finding_id: None,
                    severity: report.severity,
                    status: None,
                    confidence: report.confidence.filter(|c| (0.0..=1.0).contains(c)),
                    title: text(&report.title, "(untitled)"),
                    file: place(&report.file),
                    line: report.line.filter(|line| *line > 0),
                    evidence: text(&report.body, "(no body)"),
                    fix: report
                        .fix
                        .as_deref()
                        .map(bounded)
                        .filter(|fix| !fix.is_empty()),
                    agent_prompt: placeholder(),
                },
            )
        })
        .collect())
}

/// A report's file as a finding names it: none for a change-wide report.
fn place(file: &str) -> Option<String> {
    let file = file.trim();
    (!file.is_empty() && file != CHANGE_WIDE_SENTINEL).then(|| bounded(file))
}

/// The findings of one recorded review round, per reviewer in name order, a reviewer that
/// reported none or whose result is missing included: a complete round's are the Finding Set
/// entries that round saw itself, an incomplete round's each reviewer's recorded reports. The
/// one builder of a round's findings: `af task findings` prints them, and a continued Task is
/// handed exactly them (ADR-0212).
pub(super) fn round_findings(
    cas: &Cas,
    round: &TaskReviewRoundV1,
) -> Result<Vec<TaskReviewerFindingsV2>, String> {
    let gathered = match &round.finding_set_id {
        Some(set) => Some(gathered(
            &artifact(cas, set, review_core::contract::FINDING_SET_V1)?,
            &round.snapshot_id,
        )?),
        None => None,
    };
    let mut names: BTreeSet<String> = round.selected_results.keys().cloned().collect();
    names.extend(round.missing_reviewers.iter().cloned());
    if let Some(gathered) = &gathered {
        names.extend(gathered.keys().filter(|name| is_name(name)).cloned());
    }
    let mut reviewers = Vec::new();
    for name in names {
        let result_id = round.selected_results.get(&name).cloned();
        let findings = match (&gathered, &result_id) {
            (Some(gathered), _) => gathered.get(&name).cloned().unwrap_or_default(),
            (None, Some(result)) => recorded(cas, &name, result, &round.snapshot_id)?,
            (None, None) => Vec::new(),
        };
        reviewers.push(TaskReviewerFindingsV2 {
            reviewer: name,
            result_id,
            findings,
        });
    }
    Ok(reviewers)
}

/// Why a reviewer of the round is not covered: the round recorded no result from it.
fn not_run_reason(task_id: &str) -> String {
    format!(
        "the round recorded no result from this required reviewer: it did not run, or none of \
         its Attempts produced a result before the round closed; `af task show {task_id}` \
         shows its Attempts"
    )
}

/// What the round covered (ADR-0248): each reviewer reviewed when the round selected its
/// result, not run otherwise; the paths of the round's Subject, and any path a finding names
/// outside it.
fn round_coverage(
    task_id: &str,
    subject: &TaskReviewSubjectV2,
    reviewers: &[TaskReviewerFindingsV2],
) -> handoff::CoverageV1 {
    let changed = subject
        .change_scope
        .as_ref()
        .map(|scope| scope.changed_paths.as_slice());
    handoff::coverage(
        changed,
        reviewers
            .iter()
            .map(|reviewer| {
                let reason = reviewer
                    .result_id
                    .is_none()
                    .then(|| not_run_reason(task_id));
                (reviewer.reviewer.clone(), reason)
            })
            .collect(),
        reviewers
            .iter()
            .flat_map(|reviewer| &reviewer.findings)
            .filter_map(|finding| finding.file.as_deref()),
    )
}

/// The round's Subject, as its reduce step read it.
fn round_subject(cas: &Cas, round: &TaskReviewRoundV1) -> Result<TaskReviewSubjectV2, String> {
    let id = round
        .invocation
        .inputs
        .get("subject")
        .and_then(|input| input.artifact_ids.first())
        .ok_or("the review round names no Subject")?;
    let subject: TaskReviewSubjectV2 = artifact(cas, id, TASK_REVIEW_SUBJECT_V2)?;
    subject.validate()?;
    Ok(subject)
}

/// The findings this round's Snapshot verified fixed: an earlier finding the round's Finding Set
/// marks `fixed` and the repair assessment the round continued names with a positive
/// `af/TaskFixReceipt@1` on that Snapshot. Nothing else is verified fixed; an incomplete round,
/// which recorded no Finding Set, verified nothing.
fn verified_fixed(
    cas: &Cas,
    round: &TaskReviewRoundV1,
    subject: &TaskReviewSubjectV2,
) -> Result<Vec<VerifiedFixV1>, String> {
    let (Some(set_id), Some(continuation_id)) = (&round.finding_set_id, &subject.continuation_id)
    else {
        return Ok(Vec::new());
    };
    let set: FindingSetV1 = artifact(cas, set_id, review_core::contract::FINDING_SET_V1)?;
    let continuation: TaskReviewContinuationV1 =
        artifact(cas, continuation_id, TASK_REVIEW_CONTINUATION_V1)?;
    let assessment = &continuation.assessment;
    if assessment.current_snapshot_id != round.snapshot_id {
        return Ok(Vec::new());
    }
    let fixed: BTreeSet<&str> = set
        .findings
        .iter()
        .filter(|entry| entry.status == "fixed")
        .map(|entry| entry.finding_id.as_str())
        .collect();
    Ok(assessment
        .claims
        .iter()
        .filter(|(id, claim)| {
            claim.outcome == VerificationOutcomeV1::Positive && fixed.contains(id.as_str())
        })
        .map(|(id, claim)| VerifiedFixV1 {
            finding_id: id.clone(),
            record_id: claim.receipt_id.clone(),
            snapshot_id: assessment.current_snapshot_id.clone(),
        })
        .collect())
}

/// The verdict as the document carries it: the node that published it, af's bounded reason.
fn verdict(cas: &Cas, node: &str, id: &str) -> Result<TaskFindingsEvaluationV1, String> {
    let verdict: TaskEvaluationV1 = artifact(cas, id, TASK_EVALUATION_V1)?;
    Ok(TaskFindingsEvaluationV1 {
        node: runs::node_name(node),
        artifact_id: id.to_owned(),
        outcome: verdict.outcome,
        reason: bounded(&verdict.reason),
    })
}

/// The evaluator's verdict of a Task that recorded no review round: the `af/TaskEvaluation@1`
/// the last node in plan order published. A round's own verdict is the one the Task log
/// recorded after it ([`task_report::round_evaluations`]).
fn evaluation(
    cas: &Cas,
    task: &TaskProjection,
) -> Result<Option<TaskFindingsEvaluationV1>, String> {
    let Some(execution) = &task.execution else {
        return Ok(None);
    };
    let order = &execution.graph.order;
    let mut found: Option<(usize, String, String)> = None;
    for (node, (_, output)) in &execution.outputs {
        for port in output.outputs.values() {
            if port.artifact_type != TASK_EVALUATION_V1 {
                continue;
            }
            let Some(id) = port.artifact_ids.first() else {
                continue;
            };
            let at = order.iter().position(|n| n == node).unwrap_or(0);
            if found.as_ref().is_none_or(|(before, ..)| at >= *before) {
                found = Some((at, node.clone(), id.clone()));
            }
        }
    }
    let Some((_, node, id)) = found else {
        return Ok(None);
    };
    verdict(cas, &node, &id).map(Some)
}

pub(super) fn document(opened: &OpenedTask, wanted: Option<u64>) -> Result<TaskFindingsV2, String> {
    let task = &opened.task;
    let id = &task.task_id;
    let (_, view) = opened.view()?;
    let run_id = review_store::store::task::task_run_id(id).map_err(|e| e.to_string())?;
    let rounds = task_report::recorded_rounds(&opened.cas, &opened.store, &run_id)?;
    let count = rounds.len() as u64;
    let index = match wanted {
        Some(n) if n > count => {
            return Err(match count {
                0 => format!(
                    "Task {id} recorded no review round, so --round {n} names none; it is {}; \
                     next: {}",
                    view.state.label(),
                    view.state.next_action.as_deref().unwrap_or("none")
                ),
                _ => format!(
                    "Task {id} recorded {count} review round{}, so --round {n} names none of \
                     them; name 1 to {count}, or leave --round out for the last: `af task \
                     findings {id}`",
                    if count == 1 { "" } else { "s" }
                ),
            });
        }
        Some(n) => Some(n),
        None => (count > 0).then_some(count),
    };
    let round = match index {
        None => None,
        Some(index) => {
            let round = &rounds[usize::try_from(index - 1).map_err(|e| e.to_string())?];
            let reviewers = round_findings(&opened.cas, round)?;
            let mut counts = TaskFindingCountsV1::default();
            for finding in reviewers.iter().flat_map(|reviewer| &reviewer.findings) {
                match finding.severity {
                    Severity::Blocker => counts.blocker += 1,
                    Severity::Major => counts.major += 1,
                    Severity::Minor => counts.minor += 1,
                }
            }
            let subject = round_subject(&opened.cas, round)?;
            Some(TaskFindingsRoundV2 {
                index,
                round: round.round,
                snapshot_id: round.snapshot_id.clone(),
                conclusion: round.conclusion,
                complete: round.finding_set_id.is_some(),
                missing_reviewers: round.missing_reviewers.iter().cloned().collect(),
                counts,
                coverage: round_coverage(id, &subject, &reviewers),
                verified_fixed: verified_fixed(&opened.cas, round, &subject)?,
                reviewers,
            })
        }
    };
    // The round's own verdict, selected by its place in the log as the round itself is.
    let evaluation = match index {
        None => evaluation(&opened.cas, task)?,
        Some(index) => {
            let followed = task_report::round_evaluations(&opened.cas, &opened.store, &run_id)?;
            match followed
                .get(usize::try_from(index - 1).map_err(|e| e.to_string())?)
                .cloned()
                .flatten()
            {
                Some((node, verdict_id)) => Some(verdict(&opened.cas, &node, &verdict_id)?),
                None => None,
            }
        }
    };
    let document = TaskFindingsV2 {
        schema: TASK_FINDINGS_V2.into(),
        task_id: id.clone(),
        state: view.state,
        rounds: count,
        round,
        evaluation,
    };
    document.validate()?;
    Ok(document)
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Blocker => "blocker",
        Severity::Major => "major",
        Severity::Minor => "minor",
    }
}

fn counted(counts: &TaskFindingCountsV1) -> String {
    let parts: Vec<String> = [
        (counts.blocker, "blocker"),
        (counts.major, "major"),
        (counts.minor, "minor"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, name)| format!("{n} {name}"))
    .collect();
    if parts.is_empty() {
        "no findings".into()
    } else {
        parts.join(", ")
    }
}

/// One line of what a round covered: its reviewers that did not run, then its paths by state.
fn coverage_line(coverage: &handoff::CoverageV1) -> String {
    use handoff::PathCoverageStateV1 as Path;
    let reviewers = coverage.reviewers.len();
    let missing: Vec<&str> = coverage
        .not_run()
        .map(|reviewer| reviewer.reviewer.as_str())
        .collect();
    let mut parts = vec![match missing.len() {
        0 => format!(
            "every reviewer ran ({reviewers} reviewer{})",
            if reviewers == 1 { "" } else { "s" }
        ),
        _ => format!(
            "{} of {reviewers} reviewers did not run ({}), so no findings from them means \
             not reviewed",
            missing.len(),
            preview::text(&missing.join(", "))
        ),
    }];
    for (state, word) in [
        (Path::Reviewed, "reviewed"),
        (Path::PartiallyReviewed, "partially reviewed"),
        (Path::NotReviewed, "not reviewed"),
        (Path::OutsideRound, "outside the round"),
    ] {
        let count = coverage
            .paths
            .iter()
            .filter(|path| path.state == state)
            .count();
        if count > 0 {
            let noun = if count == 1 { "path" } else { "paths" };
            parts.push(format!("{count} {noun} {word}"));
        }
    }
    parts.join("; ")
}

/// The text: the round and its reviewers, the findings grouped by severity (blocker first),
/// the evaluator's verdict, and the next command last.
pub(super) fn lines(document: &TaskFindingsV2) -> Vec<String> {
    let id = &document.task_id;
    let mut lines = Vec::new();
    let conclusion = |conclusion| {
        serde_json::to_value(conclusion)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    match &document.round {
        None => lines.push(format!(
            "Task {id}: {}; it recorded no review round, so there are no review findings",
            document.state.label()
        )),
        Some(round) => {
            lines.push(format!(
                "Task {id}: {}; review round {} of {} ({}) on {}{}",
                document.state.label(),
                round.index,
                document.rounds,
                conclusion(round.conclusion),
                round.snapshot_id,
                if round.complete {
                    String::new()
                } else {
                    "; incomplete: each reviewer's reports as recorded, not gathered".into()
                }
            ));
            for reviewer in &round.reviewers {
                let mut counts = TaskFindingCountsV1::default();
                for finding in &reviewer.findings {
                    match finding.severity {
                        Severity::Blocker => counts.blocker += 1,
                        Severity::Major => counts.major += 1,
                        Severity::Minor => counts.minor += 1,
                    }
                }
                // A reviewer that never ran found nothing only because it never looked.
                let said = if reviewer.result_id.is_none() {
                    "not reviewed: it did not run".to_owned()
                } else {
                    counted(&counts)
                };
                lines.push(format!("  {}: {said}", preview::text(&reviewer.reviewer)));
            }
            lines.push(format!("  coverage: {}", coverage_line(&round.coverage)));
            for fixed in &round.verified_fixed {
                lines.push(format!(
                    "  verified fixed: {} by {} on {}",
                    fixed.finding_id, fixed.record_id, fixed.snapshot_id
                ));
            }
            for severity in [Severity::Blocker, Severity::Major, Severity::Minor] {
                let found: Vec<(&str, &TaskFindingV2)> = round
                    .reviewers
                    .iter()
                    .flat_map(|reviewer| {
                        reviewer
                            .findings
                            .iter()
                            .filter(move |finding| finding.severity == severity)
                            .map(move |finding| (reviewer.reviewer.as_str(), finding))
                    })
                    .collect();
                if found.is_empty() {
                    continue;
                }
                lines.push(String::new());
                lines.push(severity_name(severity).into());
                for (reviewer, finding) in found {
                    let confidence = finding
                        .confidence
                        .map(|c| format!(", confidence {c:.2}"))
                        .unwrap_or_default();
                    let status = finding
                        .status
                        .as_deref()
                        .map(|s| format!(", {}", preview::text(s)))
                        .unwrap_or_default();
                    lines.push(format!(
                        "  [{}] {}{confidence}{status}",
                        preview::text(reviewer),
                        preview::text(&finding.title)
                    ));
                    match (&finding.file, finding.line) {
                        (Some(file), Some(line)) => {
                            lines.push(format!("    at {}:{line}", preview::text(file)));
                        }
                        (Some(file), None) => lines.push(format!("    at {}", preview::text(file))),
                        _ => lines.push("    at an unrecorded location".into()),
                    }
                    for line in finding.evidence.lines() {
                        lines.push(format!("    > {}", preview::text(line)));
                    }
                    if let Some(fix) = &finding.fix {
                        lines.push(format!("    fix: {}", preview::text(fix)));
                    }
                }
            }
        }
    }
    lines.push(String::new());
    match &document.evaluation {
        Some(evaluation) => {
            let outcome = serde_json::to_value(evaluation.outcome)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            lines.push(format!(
                "evaluator ({}): {outcome}",
                preview::text(&evaluation.node)
            ));
            for line in evaluation.reason.lines() {
                lines.push(format!("  {}", preview::text(line)));
            }
        }
        None => lines.push("evaluator: no verdict recorded".into()),
    }
    lines.push(format!(
        "next: {}",
        document.state.next_action.as_deref().unwrap_or("none")
    ));
    lines
}

/// The document as one SARIF 2.1.0 log (ADR-0248).
pub(super) fn sarif(document: &TaskFindingsV2) -> crate::sarif::SarifRun<'_> {
    let round = document.round.as_ref();
    crate::sarif::SarifRun {
        owner: ("task_id", &document.task_id),
        round: round.map(|round| round.index),
        snapshot_id: round.map(|round| round.snapshot_id.as_str()),
        coverage: round.map(|round| &round.coverage),
        verified_fixed: round.map_or(&[], |round| round.verified_fixed.as_slice()),
        findings: round
            .into_iter()
            .flat_map(|round| &round.reviewers)
            .flat_map(|reviewer| {
                reviewer
                    .findings
                    .iter()
                    .map(|finding| crate::sarif::SarifFinding {
                        reviewer: &reviewer.reviewer,
                        severity: finding.severity,
                        title: &finding.title,
                        finding_id: finding.finding_id.as_deref(),
                        status: finding.status.as_deref(),
                        prompt: &finding.agent_prompt,
                        declined: matches!(finding.status.as_deref(), Some("rejected" | "wontfix")),
                    })
            })
            .collect(),
    }
}

/// `af task findings ID [--round N] [--format text|json|sarif]`. Exits 0 once it read the
/// Store: the findings are data.
pub(crate) fn print(
    id: &str,
    round: Option<u64>,
    format: Option<crate::cli::FindingsFormatArg>,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    use crate::cli::FindingsFormatArg as Format;
    let opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let document = document(&opened, round)?;
    match (inspect.json, format) {
        (true, _) | (_, Some(Format::Json)) => println!(
            "{}",
            serde_json::to_string(&document).map_err(|e| e.to_string())?
        ),
        (_, Some(Format::Sarif)) => crate::sarif::print(&sarif(&document))?,
        (_, Some(Format::Text) | None) => {
            for line in lines(&document) {
                println!("{line}");
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::state::*;

    fn id(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn finding(severity: Severity, title: &str) -> TaskFindingV2 {
        prompted(
            "bugs",
            &id('2'),
            TaskFindingV2 {
                finding_id: Some(id('1')),
                severity,
                status: Some("open".into()),
                confidence: Some(0.99),
                title: title.into(),
                file: Some("pagination.py".into()),
                line: Some(1),
                evidence: "A negative offset slices from the end.".into(),
                fix: Some("Raise ValueError.".into()),
                agent_prompt: placeholder(),
            },
        )
    }

    /// A reviewed round: `bugs` reported two findings; `correctness` answered, or, with
    /// `missing`, recorded no result.
    fn document(missing: bool) -> TaskFindingsV2 {
        let reviewers = vec![
            TaskReviewerFindingsV2 {
                reviewer: "bugs".into(),
                result_id: Some(id('3')),
                findings: vec![
                    finding(Severity::Minor, "Name the default"),
                    finding(Severity::Major, "Reject negative offset"),
                ],
            },
            TaskReviewerFindingsV2 {
                reviewer: "correctness".into(),
                result_id: (!missing).then(|| id('4')),
                findings: vec![],
            },
        ];
        let subject = TaskReviewSubjectV2 {
            subject_id: id('6'),
            subject: review_core::SubjectV1::whole_tree(id('2')),
            snapshot_id: id('2'),
            prior_history_id: id('7'),
            continuation_id: None,
            round: 1,
            change_scope: None,
        };
        TaskFindingsV2 {
            schema: TASK_FINDINGS_V2.into(),
            task_id: "t".into(),
            state: TaskStateV1::ended(TaskStatePhaseV1::Finished, TaskOutcomeV1::Verified, None),
            rounds: 1,
            round: Some(TaskFindingsRoundV2 {
                index: 1,
                round: 1,
                snapshot_id: id('2'),
                conclusion: if missing {
                    review_core::task::review::ReviewConclusionV1::Incomplete
                } else {
                    review_core::task::review::ReviewConclusionV1::Pass
                },
                complete: !missing,
                missing_reviewers: if missing {
                    vec!["correctness".into()]
                } else {
                    vec![]
                },
                counts: TaskFindingCountsV1 {
                    blocker: 0,
                    major: 1,
                    minor: 1,
                },
                coverage: round_coverage("t", &subject, &reviewers),
                verified_fixed: vec![],
                reviewers,
            }),
            evaluation: Some(TaskFindingsEvaluationV1 {
                node: "evaluate".into(),
                artifact_id: id('5'),
                outcome: review_core::task::pipeline::ReceiptOutcomeV1::Passed,
                reason: "Verified on the sealed source".into(),
            }),
        }
    }

    #[test]
    fn findings_are_grouped_by_severity_with_their_evidence_and_fix() {
        let document = document(false);
        document.validate().unwrap();
        let text = lines(&document).join("\n");
        let major = text.find("\nmajor\n").expect(&text);
        let minor = text.find("\nminor\n").expect(&text);
        assert!(major < minor, "{text}");
        for needle in [
            "  bugs: 1 major, 1 minor",
            "  correctness: no findings",
            "  coverage: every reviewer ran (2 reviewers); 1 path reviewed",
            "  [bugs] Reject negative offset, confidence 0.99, open",
            "    at pagination.py:1",
            "    > A negative offset slices from the end.",
            "    fix: Raise ValueError.",
            "evaluator (evaluate): passed",
            "  Verified on the sealed source",
        ] {
            assert!(text.contains(needle), "{needle}:\n{text}");
        }
        assert!(text.ends_with("next: none"), "{text}");
        let prompt = &document.round.as_ref().unwrap().reviewers[0].findings[1].agent_prompt;
        assert_eq!(prompt.requested_change, "Raise ValueError.");
        assert_eq!(prompt.verification, handoff::TASK_VERIFICATION);
    }

    /// Acceptance: a reviewer that never ran is reported as not reviewed, not as no findings —
    /// in the text, the document's coverage and the SARIF log.
    #[test]
    fn a_reviewer_that_did_not_run_is_not_reviewed_never_no_findings() {
        let document = document(true);
        document.validate().unwrap();
        let round = document.round.as_ref().unwrap();
        let correctness = &round.coverage.reviewers[1];
        assert_eq!(correctness.reviewer, "correctness");
        assert_eq!(correctness.state, handoff::ReviewerCoverageStateV1::NotRun);
        assert!(
            correctness
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("`af task show t`")),
            "{correctness:?}"
        );
        assert_eq!(
            round.coverage.paths[0].state,
            handoff::PathCoverageStateV1::PartiallyReviewed
        );
        let text = lines(&document).join("\n");
        assert!(
            text.contains("  correctness: not reviewed: it did not run"),
            "{text}"
        );
        assert!(!text.contains("correctness: no findings"), "{text}");
        assert!(
            text.contains(
                "  coverage: 1 of 2 reviewers did not run (correctness), so no findings from \
                 them means not reviewed; 1 path partially reviewed"
            ),
            "{text}"
        );
        let log = crate::sarif::document(&sarif(&document));
        crate::sarif::check(&log).unwrap();
        let invocation = &log["runs"][0]["invocations"][0];
        assert_eq!(invocation["executionSuccessful"], false);
        assert!(
            invocation["toolExecutionNotifications"][0]["message"]["text"]
                .as_str()
                .unwrap()
                .contains("correctness did not run"),
            "{invocation}"
        );
        assert_eq!(log["runs"][0]["results"].as_array().unwrap().len(), 2);
    }
}
