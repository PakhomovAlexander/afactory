//! `af task findings` (ADR-0198): the review findings of a Task's last (or Nth) review round,
//! per reviewer — each with its severity, confidence, title, location, the quoted evidence and
//! the proposed fix — then the evaluator's verdict and its reason. Text is grouped by severity;
//! `--json` prints one `af/task-findings@1` document. Read-only: it reads what `af task report`
//! reads, the Task's recorded rounds, their Finding Sets and reviewer results, from the Store.

use super::*;
use review_core::Severity;
use review_core::finding_set::FindingSetV1;
use review_core::task::agent_loop::*;
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
fn gathered(cas: &Cas, set_id: &str) -> Result<BTreeMap<String, Vec<TaskFindingV1>>, String> {
    let set: FindingSetV1 = artifact(cas, set_id, review_core::contract::FINDING_SET_V1)?;
    set.validate()?;
    let mut by_reviewer: BTreeMap<String, Vec<TaskFindingV1>> = BTreeMap::new();
    for entry in set.findings {
        if entry.last_seen_round != set.round {
            continue;
        }
        by_reviewer
            .entry(entry.source.clone())
            .or_default()
            .push(TaskFindingV1 {
                finding_id: Some(entry.finding_id),
                severity: entry.severity,
                status: Some(entry.status),
                confidence: entry.confidence,
                title: bounded(&entry.title),
                file: entry.file.map(|file| bounded(&file)),
                line: entry.line.filter(|line| *line > 0),
                evidence: bounded(&entry.body),
                fix: entry.fix.map(|fix| bounded(&fix)),
            });
    }
    Ok(by_reviewer)
}

/// The reports of one reviewer's recorded result, for a round whose reduce step did not run:
/// shown as recorded, never as gathered evidence (ADR-0034).
fn recorded(cas: &Cas, result_id: &str) -> Result<Vec<TaskFindingV1>, String> {
    let envelope = cas.get_artifact(result_id).map_err(|e| e.to_string())?;
    let reports = envelope.payload["reports"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut findings = Vec::new();
    for report in reports {
        let Ok(severity) = serde_json::from_value::<Severity>(report["severity"].clone()) else {
            continue;
        };
        let text = |key: &str| report[key].as_str().map(bounded).filter(|t| !t.is_empty());
        let (Some(title), Some(evidence)) = (text("title"), text("body")) else {
            continue;
        };
        findings.push(TaskFindingV1 {
            finding_id: None,
            severity,
            status: None,
            confidence: report["confidence"]
                .as_f64()
                .filter(|c| (0.0..=1.0).contains(c)),
            title,
            file: text("file"),
            line: report["line"].as_i64().filter(|line| *line > 0),
            evidence,
            fix: text("fix"),
        });
    }
    Ok(findings)
}

/// The evaluator's verdict: the `af/TaskEvaluation@1` the last node in plan order published.
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
    let verdict: TaskEvaluationV1 = artifact(cas, &id, TASK_EVALUATION_V1)?;
    Ok(Some(TaskFindingsEvaluationV1 {
        node: runs::node_name(&node),
        artifact_id: id,
        outcome: verdict.outcome,
        reason: bounded(&verdict.reason),
    }))
}

pub(super) fn document(opened: &OpenedTask, wanted: Option<u64>) -> Result<TaskFindingsV1, String> {
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
                    "Task {id} recorded no review round, so --round {n} names none; it is {}",
                    view.state.label()
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
            let gathered = match &round.finding_set_id {
                Some(set) => Some(gathered(&opened.cas, set)?),
                None => None,
            };
            let mut reviewers = Vec::new();
            let mut counts = TaskFindingCountsV1::default();
            let mut names: BTreeSet<String> = round.selected_results.keys().cloned().collect();
            names.extend(round.missing_reviewers.iter().cloned());
            if let Some(gathered) = &gathered {
                names.extend(gathered.keys().filter(|name| is_name(name)).cloned());
            }
            for name in names {
                let result_id = round.selected_results.get(&name).cloned();
                let findings = match (&gathered, &result_id) {
                    (Some(gathered), _) => gathered.get(&name).cloned().unwrap_or_default(),
                    (None, Some(result)) => recorded(&opened.cas, result)?,
                    (None, None) => Vec::new(),
                };
                for finding in &findings {
                    match finding.severity {
                        Severity::Blocker => counts.blocker += 1,
                        Severity::Major => counts.major += 1,
                        Severity::Minor => counts.minor += 1,
                    }
                }
                reviewers.push(TaskReviewerFindingsV1 {
                    reviewer: name,
                    result_id,
                    findings,
                });
            }
            Some(TaskFindingsRoundV1 {
                index,
                round: round.round,
                snapshot_id: round.snapshot_id.clone(),
                conclusion: round.conclusion,
                complete: round.finding_set_id.is_some(),
                missing_reviewers: round.missing_reviewers.iter().cloned().collect(),
                counts,
                reviewers,
            })
        }
    };
    let document = TaskFindingsV1 {
        schema: TASK_FINDINGS_V1.into(),
        task_id: id.clone(),
        state: view.state,
        rounds: count,
        round,
        evaluation: evaluation(&opened.cas, task)?,
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

/// The text: the round and its reviewers, the findings grouped by severity (blocker first),
/// the evaluator's verdict, and the next command last.
pub(super) fn lines(document: &TaskFindingsV1) -> Vec<String> {
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
                let missing = if reviewer.result_id.is_none() {
                    " (its result is missing)"
                } else {
                    ""
                };
                lines.push(format!(
                    "  {}: {}{missing}",
                    preview::text(&reviewer.reviewer),
                    counted(&counts)
                ));
            }
            for severity in [Severity::Blocker, Severity::Major, Severity::Minor] {
                let found: Vec<(&str, &TaskFindingV1)> = round
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

/// `af task findings ID [--round N]`. Exits 0 once it read the Store: the findings are data.
pub(crate) fn print(
    id: &str,
    round: Option<u64>,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    let opened = open_task(id, &inspect.repo, inspect.state.as_deref())?;
    let document = document(&opened, round)?;
    if inspect.json {
        println!(
            "{}",
            serde_json::to_string(&document).map_err(|e| e.to_string())?
        );
    } else {
        for line in lines(&document) {
            println!("{line}");
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::state::*;

    #[test]
    fn findings_are_grouped_by_severity_with_their_evidence_and_fix() {
        let id = |c: char| format!("sha256:{}", c.to_string().repeat(64));
        let finding = |severity, title: &str| TaskFindingV1 {
            finding_id: Some(id('1')),
            severity,
            status: Some("open".into()),
            confidence: Some(0.99),
            title: title.into(),
            file: Some("pagination.py".into()),
            line: Some(1),
            evidence: "A negative offset slices from the end.".into(),
            fix: Some("Raise ValueError.".into()),
        };
        let document = TaskFindingsV1 {
            schema: TASK_FINDINGS_V1.into(),
            task_id: "t".into(),
            state: TaskStateV1::ended(TaskStatePhaseV1::Finished, TaskOutcomeV1::Verified, None),
            rounds: 1,
            round: Some(TaskFindingsRoundV1 {
                index: 1,
                round: 1,
                snapshot_id: id('2'),
                conclusion: review_core::task::review::ReviewConclusionV1::Pass,
                complete: true,
                missing_reviewers: vec![],
                counts: TaskFindingCountsV1 {
                    blocker: 0,
                    major: 1,
                    minor: 1,
                },
                reviewers: vec![
                    TaskReviewerFindingsV1 {
                        reviewer: "bugs".into(),
                        result_id: Some(id('3')),
                        findings: vec![
                            finding(Severity::Minor, "Name the default"),
                            finding(Severity::Major, "Reject negative offset"),
                        ],
                    },
                    TaskReviewerFindingsV1 {
                        reviewer: "correctness".into(),
                        result_id: Some(id('4')),
                        findings: vec![],
                    },
                ],
            }),
            evaluation: Some(TaskFindingsEvaluationV1 {
                node: "evaluate".into(),
                artifact_id: id('5'),
                outcome: review_core::task::pipeline::ReceiptOutcomeV1::Passed,
                reason: "Verified on the sealed source".into(),
            }),
        };
        document.validate().unwrap();
        let text = lines(&document).join("\n");
        let major = text.find("\nmajor\n").expect(&text);
        let minor = text.find("\nminor\n").expect(&text);
        assert!(major < minor, "{text}");
        for needle in [
            "  bugs: 1 major, 1 minor",
            "  correctness: no findings",
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
    }
}
