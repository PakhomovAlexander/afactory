//! Continuing a reviewed Task (ADR-0212): `af task run NEW --from TASK [--round N]`, or a Task
//! file's `continue` table.
//!
//! Resolution happens once, at plan time, from the `--state` Store alone, like an `inputs`
//! binding (ADR-0117). The continued round's reviewed candidate becomes the new Task's `source`,
//! re-rooted over the identical Manifest exactly as a bound derived tree is, and the round's
//! reviewer reports and the evaluator's verdict become one `af/TaskReviewFindings@1` artifact on
//! the Pipeline's optional `findings` port. Nothing is pasted into the goal: the candidate is bound
//! as a Snapshot and the findings as an artifact, so neither counts against the 64 KiB
//! Requirements bound.

use super::*;
use review_core::task::findings::*;
use review_core::task::input_bindings::ReferencedTaskV1;
use review_core::task::pipeline::ReceiptOutcomeV1;
use review_core::task::verification::{TASK_EVALUATION_V1, TaskEvaluationV1};

/// A Task file's `continue` table: the recorded Task, and which of its review rounds; absent,
/// the last one it recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContinueRefV1 {
    pub(super) task: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub(super) round: Option<u32>,
}

/// The input port a continuation fills beside `source`.
pub(super) const FINDINGS_PORT: &str = "findings";

/// What a continuation resolved to: the candidate as the new Task's source, the Manifest it
/// holds, and the findings artifact.
pub(super) struct Continued {
    pub(super) source: ArtifactInputV1,
    pub(super) manifest: Manifest,
    pub(super) findings_id: String,
    pub(super) findings: TaskReviewFindingsV1,
}

impl Continued {
    pub(super) fn port(&self) -> ArtifactInputV1 {
        ArtifactInputV1 {
            artifact_ids: vec![self.findings_id.clone()],
            artifact_type: TASK_REVIEW_FINDINGS_V1.into(),
            cardinality: PortCardinality::One,
            snapshot_id: None,
        }
    }
}

/// One line of reviewer text as the findings carry it: control characters become spaces, and
/// the line is cut at `max` characters.
fn line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.trim().chars().take(max).collect()
}

fn text(body: &str) -> String {
    body.chars().take(65536).collect()
}

/// The round's findings as `af task findings --round N` prints them, one builder for both
/// (`findings::round_findings`), each under `NAME-N`, its reviewer's slot and its position in
/// that reviewer's list. A finding names the reviewer result it came from, or the round's
/// Finding Set when the reviewer has no selected result.
fn reviewer_findings(
    cas: &Cas,
    round: &review_core::task::review::TaskReviewRoundV1,
) -> Result<Vec<TaskReviewFindingV1>, String> {
    let mut findings = Vec::new();
    for reviewer in super::findings::round_findings(cas, round)? {
        let Some(artifact_id) = reviewer
            .result_id
            .clone()
            .or_else(|| round.finding_set_id.clone())
        else {
            continue;
        };
        for (index, finding) in reviewer.findings.iter().enumerate() {
            let title = line(&finding.title, 1024);
            findings.push(TaskReviewFindingV1 {
                id: format!("{}-{}", reviewer.reviewer, index + 1),
                origin: TaskReviewFindingOriginV1::Reviewer,
                by: reviewer.reviewer.clone(),
                artifact_id: artifact_id.clone(),
                severity: Some(finding.severity),
                file: finding.file.as_deref().map(|file| line(file, 1024)),
                line: finding.line.and_then(|line| u64::try_from(line).ok()),
                title: if title.is_empty() {
                    "(untitled)".into()
                } else {
                    title
                },
                body: text(&finding.evidence),
                fix: finding.fix.as_deref().map(text),
            });
        }
    }
    Ok(findings)
}

/// The evaluator's verdict on the round at `position` in the recorded rounds, as a finding when
/// it did not pass: the verdict the Task log recorded after that round, as `af task findings
/// --round N` shows it, never one of another round that reviewed the same Snapshot.
fn evaluator_finding(
    cas: &Cas,
    store: &EventStore,
    run_id: &str,
    position: usize,
) -> Result<Option<TaskReviewFindingV1>, String> {
    let followed = task_report::round_evaluations(cas, store, run_id)?;
    let Some((_, id)) = followed.get(position).cloned().flatten() else {
        return Ok(None);
    };
    let evaluation: TaskEvaluationV1 = artifact(cas, &id, TASK_EVALUATION_V1)?;
    let outcome = match evaluation.outcome {
        ReceiptOutcomeV1::Passed => return Ok(None),
        ReceiptOutcomeV1::Failed => "failed",
        ReceiptOutcomeV1::Inconclusive => "inconclusive",
    };
    Ok(Some(TaskReviewFindingV1 {
        id: "evaluator".into(),
        origin: TaskReviewFindingOriginV1::Evaluator,
        by: "evaluator".into(),
        artifact_id: id,
        severity: None,
        file: None,
        line: None,
        title: format!("Evaluator verdict: {outcome}"),
        body: text(&evaluation.reason),
        fix: None,
    }))
}

/// The durable output that holds the candidate `round` reviewed, as the record ID and port a
/// continuation's [`ReferencedTaskV1`] names: the Task's result `snapshot` output when it holds
/// that Snapshot (the last round's, as a rule), or else the first `af/TaskOutput@1` the Task log
/// published with a SourceTree port that holds it, the round's own candidate output. `None`
/// when nothing recorded holds it: a reference to another Snapshot's output is never recorded.
fn candidate_output(
    cas: &Cas,
    store: &EventStore,
    run_id: &str,
    result: &TaskResultV1,
    result_id: &str,
    round: &TaskReviewRoundV1,
) -> Result<Option<(String, String)>, String> {
    let holds = result.outputs.get("snapshot").is_some_and(|port| {
        port.artifact_type == SOURCE_TREE_V1
            && port.snapshot_id.as_deref() == Some(round.snapshot_id.as_str())
    });
    if holds {
        return Ok(Some((result_id.to_owned(), "snapshot".into())));
    }
    task_report::output_holding(cas, store, run_id, &round.snapshot_id)
}

fn phase_word(phase: &TaskPhaseV1) -> &'static str {
    match phase {
        TaskPhaseV1::Submitted {} => "submitted",
        TaskPhaseV1::Ready {} => "ready",
        TaskPhaseV1::Running {} => "running",
        TaskPhaseV1::Waiting { .. } => "waiting",
        TaskPhaseV1::Finished { .. } => "finished",
    }
}

/// Resolve `reference` in this Store: the finished Task, its review round, the candidate that
/// round reviewed and the findings it recorded. Every refusal names the Task as the Task file or
/// the command line wrote it, and says what to do.
pub(super) fn resolve(
    cas: &Cas,
    store: &EventStore,
    reference: &ContinueRefV1,
) -> Result<Continued, String> {
    let task = &reference.task;
    let shown = preview::short(task, 48);
    if !is_name(task) {
        return Err(format!(
            "continue: {shown:?} is not a Task ID: 1 to 128 ASCII letters, digits, `-` and `_`"
        ));
    }
    let projection = store
        .task_projection(cas, task)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "continue: no Task `{shown}` is recorded in this Store; `af task list` shows the \
                 Tasks it holds, and --state or --workspace selects another Store"
            )
        })?;
    let TaskPhaseV1::Finished { result_id } = &projection.phase else {
        return Err(format!(
            "continue: Task `{shown}` is {}, not finished; continue it once it has finished \
             (`af task show {shown}` says where it stands)",
            phase_word(&projection.phase)
        ));
    };
    let result: TaskResultV1 = artifact(cas, result_id, TASK_RESULT_V1)?;
    result.validate()?;
    let run_id = review_store::store::task::task_run_id(task).map_err(|e| e.to_string())?;
    let rounds = task_report::recorded_outputs(cas, store, &run_id, TASK_REVIEW_ROUND_V1)?;
    let mut recorded = Vec::new();
    for id in rounds {
        let round: TaskReviewRoundV1 = artifact(cas, &id, TASK_REVIEW_ROUND_V1)?;
        recorded.push((id, round));
    }
    if recorded.is_empty() {
        return Err(format!(
            "continue: Task `{shown}` recorded no review round, so it has no reviewed candidate \
             to continue from; bind its output instead with an `inputs` table \
             (`af help task-file`)"
        ));
    }
    let chosen = match reference.round {
        Some(number) => recorded
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (_, round))| round.round == number)
            .ok_or_else(|| {
                let numbers: BTreeSet<String> = recorded
                    .iter()
                    .map(|(_, round)| round.round.to_string())
                    .collect();
                format!(
                    "continue: Task `{shown}` has no review round {number}; its rounds are {}. \
                     Name one of them with --round, or omit --round for the last",
                    numbers.into_iter().collect::<Vec<_>>().join(", ")
                )
            })?,
        None => recorded.iter().enumerate().next_back().expect("nonempty"),
    };
    let (position, (round_id, round)) = (chosen.0, chosen.1.clone());
    let (output_id, port) = candidate_output(cas, store, &run_id, &result, result_id, &round)?
        .ok_or_else(|| {
            format!(
                "continue: Task `{shown}` round {} reviewed Snapshot {}, and no output the Task \
                 recorded holds it, so a continuation could not record where its source came \
                 from; continue from another round (`af task findings {shown} --round N` shows \
                 each), or omit --round for the last",
                round.round, round.snapshot_id
            )
        })?;
    let referenced = ReferencedTaskV1 {
        task_id: task.clone(),
        task_revision_id: result.task_revision_id.clone(),
        result_id: output_id,
        port,
        acceptance: result.acceptance,
        domain_conclusion: result.domain_conclusion.clone(),
    };
    referenced.validate()?;
    // The candidate the round reviewed, as an `af/SourceTree@1` of its own: a sealed candidate
    // is derived, so binding it re-roots it over the identical Manifest, exactly as an `inputs`
    // binding of a derived tree does (ADR-0117).
    let candidate = source_tree(cas, producer(), &round.snapshot_id, vec![round_id.clone()])?;
    let (source, _, manifest) = input_bindings::bind_source(cas, candidate, Some(&referenced))
        .map_err(|error| format!("continue: Task `{shown}`'s candidate: {error}"))?;
    let mut findings = reviewer_findings(cas, &round)?;
    findings.extend(evaluator_finding(cas, store, &run_id, position)?);
    if findings.len() > MAX_FINDINGS {
        return Err(format!(
            "continue: Task `{shown}` round {} recorded {} findings, more than the {MAX_FINDINGS} \
             one continuation carries",
            round.round,
            findings.len()
        ));
    }
    let record = TaskReviewFindingsV1 {
        schema: TASK_REVIEW_FINDINGS_SCHEMA.into(),
        continues: TaskContinuesV1 {
            task_id: task.clone(),
            task_revision_id: result.task_revision_id.clone(),
            result_id: result_id.clone(),
            round: round.round,
            round_id: round_id.clone(),
            snapshot_id: round.snapshot_id.clone(),
        },
        findings,
        missing_reviewers: round.missing_reviewers.clone(),
    };
    record.validate()?;
    let mut refs: Vec<String> = std::iter::once(round_id)
        .chain(record.findings.iter().map(|f| f.artifact_id.clone()))
        .collect();
    refs.sort();
    refs.dedup();
    let payload = serde_json::to_value(&record).map_err(|e| e.to_string())?;
    let (findings_id, _) = cas
        .put_artifact(TASK_REVIEW_FINDINGS_V1, producer(), refs, None, payload)
        .map_err(|e| e.to_string())?;
    Ok(Continued {
        source,
        manifest,
        findings_id,
        findings: record,
    })
}

/// The continuation a recorded revision carries, read back from its provenance: the
/// `af/TaskReviewFindings@1` artifact and what it names. `None` for every Task that continues
/// nothing.
pub(super) fn recorded(
    cas: &Cas,
    revision: &TaskRevisionV1,
) -> Result<Option<(String, TaskReviewFindingsV1)>, String> {
    for id in &revision.provenance.input_artifact_ids {
        let found = cas.get_optional_artifact(id);
        let Some(envelope) = found.map_err(|e| e.to_string())? else {
            continue;
        };
        if envelope.artifact_type != TASK_REVIEW_FINDINGS_V1 {
            continue;
        }
        let record: TaskReviewFindingsV1 =
            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
        record.validate()?;
        return Ok(Some((id.clone(), record)));
    }
    Ok(None)
}

/// The `continues` member of `af/task-inspection@13`: the continued Task and round, the
/// candidate the new Task reads, and the findings artifact with its count.
pub(super) fn inspection_value(
    findings_id: &str,
    record: &TaskReviewFindingsV1,
) -> serde_json::Value {
    let continues = &record.continues;
    json!({
        "task_id": continues.task_id,
        "round": continues.round,
        "result_id": continues.result_id,
        "snapshot_id": continues.snapshot_id,
        "findings_id": findings_id,
        "findings": record.findings.len(),
    })
}

/// The line `af task show` prints for a continued Task, from its inspection document.
pub(super) fn show_line(inspection: &serde_json::Value) -> Option<String> {
    let continues = inspection.get("continues")?;
    let count = continues["findings"].as_u64().unwrap_or_default();
    Some(format!(
        "continues {} round {} ({count} finding{} on the findings port)",
        preview::text(continues["task_id"].as_str().unwrap_or_default()),
        continues["round"],
        if count == 1 { "" } else { "s" }
    ))
}

/// The Task file `af task run NEW --from TASK` plans: the continued Task's own captured file
/// with the new ID, the `continue` table, and, when given, the new goal. A `source` or
/// `findings` binding of the old file is dropped, because the continuation supplies both.
pub(super) fn derived_file(
    cas: &Cas,
    store: &EventStore,
    new_id: &str,
    from: &str,
    round: Option<u32>,
    goal: Option<String>,
) -> Result<Vec<u8>, String> {
    let shown = preview::short(from, 48);
    let projection = store
        .task_projection(cas, from)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "--from {shown}: no such Task is recorded in this Store; `af task list` shows the \
                 Tasks it holds, and --state or --workspace selects another Store"
            )
        })?;
    let adapter = task_file_adapter(cas, &projection.revision)?.ok_or_else(|| {
        format!(
            "--from {shown}: that Task was not planned from a Task file, so there is no file to \
             continue; write one with a `continue` table (`af help task-file`)"
        )
    })?;
    let file_id = adapter["source_file_id"]
        .as_str()
        .ok_or("The Task file adapter names no captured file")?;
    let bytes = cas.get(file_id).map_err(|e| e.to_string())?;
    // The captured bytes are JSON or TOML, as their author wrote them.
    let mut file: TaskFile = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(_) => parse(Path::new("captured.toml"), &bytes)?,
    };
    file.task_id = new_id.to_owned();
    file.continue_from = Some(ContinueRefV1 {
        task: from.to_owned(),
        round,
    });
    if let Some(goal) = goal {
        file.goal = goal;
    }
    // The continued candidate is the new Task's base, whatever the old file declared.
    file.base = None;
    if let Some(inputs) = &mut file.inputs {
        inputs.remove("source");
        inputs.remove(FINDINGS_PORT);
        if inputs.is_empty() {
            file.inputs = None;
        }
    }
    let mut derived = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    derived.push(b'\n');
    Ok(derived)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewer_lines_lose_their_control_characters_and_stay_bounded() {
        assert_eq!(line("two\nlines\t", 1024), "two lines");
        assert_eq!(line(&"x".repeat(2000), 1024).chars().count(), 1024);
        assert_eq!(text(&"y".repeat(70000)).chars().count(), 65536);
    }

    #[test]
    fn a_continue_table_is_closed() {
        let parsed: ContinueRefV1 = serde_json::from_value(json!({"task":"a","round":2})).unwrap();
        assert_eq!(parsed.round, Some(2));
        assert!(serde_json::from_value::<ContinueRefV1>(json!({"task":"a","from":1})).is_err());
        assert!(serde_json::from_value::<ContinueRefV1>(json!({"task":"a","round":null})).is_err());
    }
}
