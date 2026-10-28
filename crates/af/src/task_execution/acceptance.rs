//! A Task file's `acceptance` (ADR-0255): the items the result must meet, each under the ID the
//! evaluator judges it by.
//!
//! The list is captured at plan time as one `af/TaskAcceptance@1` record, kept in the revision's
//! provenance and bound to the selected Pipeline's `acceptance` root port, which hands it to the
//! evaluator as its own input; the check node copies it into its `af/TaskCheckReceipt@2`. A
//! Pipeline that does not declare the port would leave its evaluator without the items, so the
//! plan is refused before anything is recorded. A Task file without the list records nothing.

use super::*;
use review_core::task::acceptance::{
    ACCEPTANCE_PORT, TASK_ACCEPTANCE_V1, TaskAcceptanceItemV1, TaskAcceptanceV1,
};

/// The record a Task file's `acceptance` list states, or why it does not.
pub(super) fn capture(items: &[TaskAcceptanceItemV1]) -> Result<TaskAcceptanceV1, String> {
    // Every refusal of the record already names the field: `acceptance names item id "a"
    // twice`, `acceptance item id "-a" is not a name …`.
    TaskAcceptanceV1::new(items.to_vec())
}

/// Refuse a plan whose Pipeline would never hand its evaluator the items: the Pipeline the Task
/// file names, or every captured Pipeline accepting its kind, declares the optional
/// `acceptance` root port as one `af/TaskAcceptance@1`. The refusal is the Task file's
/// [`refused`] `acceptance` field.
pub(super) fn require_port(
    declared: &input_bindings::DeclaredPorts,
    named: Option<&str>,
    kind: &str,
    file: &Path,
    record: &TaskAcceptanceV1,
) -> Result<(), String> {
    let takes = declared
        .declares(ACCEPTANCE_PORT, TASK_ACCEPTANCE_V1)
        .map_err(|error| format!("acceptance: {error}"))?;
    if takes {
        return Ok(());
    }
    let whose = match named {
        Some(name) => format!("the Pipeline {} does not declare", preview::text(name)),
        None => format!(
            "no captured Pipeline accepting kind {} declares",
            preview::text(kind)
        ),
    };
    Err(refused(
        file,
        record,
        &format!(
            "{whose} the optional `acceptance` input, so its evaluator could never judge the \
             items by their IDs; name a Pipeline that does in the Task file's `pipeline` (`af \
             task catalog` lists the captured ones), or drop `acceptance` and state it in the goal"
        ),
    ))
}

/// Refuse acceptance items for a Task whose domain never judges them (ADR-0255): only the code
/// domain, which the implementation profiles run their evaluator under, refuses a pass that
/// leaves an item unjudged. A Review Task's reviewers are read by the review domain, which
/// reduces their findings and never reads an item ID; a Document, report or optimization domain
/// would accept its evaluator's pass whatever the items said. The refusal is the Task file's
/// [`refused`] `acceptance` field.
pub(super) fn require_judging_domain(
    profile: TaskKindProfile,
    kind: &str,
    file: &Path,
    record: &TaskAcceptanceV1,
) -> Result<(), String> {
    match profile {
        TaskKindProfile::Implementation
        | TaskKindProfile::ReviewedImplementation
        | TaskKindProfile::RepairAllowedImplementation => Ok(()),
        TaskKindProfile::Review
        | TaskKindProfile::Document
        | TaskKindProfile::Report
        | TaskKindProfile::OptimizationAnalysis
        | TaskKindProfile::OptimizationCandidate => Err(refused(
            file,
            record,
            &format!(
                "a Task of kind {} is judged by a domain that does not judge acceptance items by \
                 their IDs, so its evaluator could pass without meeting them; use `acceptance` \
                 with an `implement` Task, or drop it and state the items in the goal",
                preview::text(kind)
            ),
        )),
    }
}

/// The typed `task_file_invalid` refusal of the Task file `file`'s `acceptance` field (`why`),
/// as `limits.wall_ms` is refused ([`refusal`]), noted for the command's `af/error@2` document.
fn refused(file: &Path, record: &TaskAcceptanceV1, why: &str) -> String {
    let (message, detail) = refusal(file, record, why);
    crate::error_document::typed(message, detail)
}

/// The sentence and the detail of [`refused`]: the file, the field and its item IDs, and the
/// command that plans the file again once it is fixed.
fn refusal(
    file: &Path,
    record: &TaskAcceptanceV1,
    why: &str,
) -> (String, crate::error_document::Detail) {
    use crate::error_document::{Cause, Detail};
    let shown = file.display().to_string();
    let again = plan_again(file);
    let ids: Vec<&str> = record.items.iter().map(|item| item.id.as_str()).collect();
    (
        format!("Task file {shown}: acceptance: {why}; then run `{again}` again"),
        Detail::new(Cause::TaskFileInvalid)
            .next(again)
            .with("file", shown)
            .with("field", "acceptance")
            .with("value", ids),
    )
}

/// Record `record` as an `af/TaskAcceptance@1` artifact and the root port it fills.
pub(super) fn record(
    cas: &Cas,
    record: &TaskAcceptanceV1,
) -> Result<(String, ArtifactInputV1), String> {
    let payload = serde_json::to_value(record).map_err(|e| e.to_string())?;
    let (id, _) = cas
        .put_artifact(TASK_ACCEPTANCE_V1, producer(), Vec::new(), None, payload)
        .map_err(|e| e.to_string())?;
    let port = ArtifactInputV1 {
        artifact_ids: vec![id.clone()],
        artifact_type: TASK_ACCEPTANCE_V1.into(),
        cardinality: PortCardinality::One,
        snapshot_id: None,
    };
    Ok((id, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_file_s_items_are_refused_with_the_field_named() {
        let item = |id: &str| TaskAcceptanceItemV1 {
            id: id.into(),
            text: "met".into(),
        };
        assert_eq!(capture(&[item("a"), item("b")]).unwrap().items.len(), 2);
        let twice = capture(&[item("a"), item("a")]).unwrap_err();
        assert_eq!(twice, "acceptance names item id \"a\" twice");
        let empty = capture(&[]).unwrap_err();
        assert!(empty.starts_with("acceptance lists 0 items"), "{empty}");
    }

    #[test]
    fn only_a_domain_that_judges_items_by_id_takes_acceptance() {
        let file = Path::new("/w/task file.json");
        let record = capture(&[TaskAcceptanceItemV1 {
            id: "a".into(),
            text: "met".into(),
        }])
        .unwrap();
        for profile in [
            TaskKindProfile::Implementation,
            TaskKindProfile::ReviewedImplementation,
            TaskKindProfile::RepairAllowedImplementation,
        ] {
            require_judging_domain(profile, "implement", file, &record).unwrap();
        }
        for (profile, kind) in [
            (TaskKindProfile::Review, "review"),
            (TaskKindProfile::Document, "document"),
            (TaskKindProfile::Report, "document"),
            (TaskKindProfile::OptimizationAnalysis, "document"),
            (TaskKindProfile::OptimizationCandidate, "document"),
        ] {
            let refused = require_judging_domain(profile, kind, file, &record).unwrap_err();
            assert!(
                refused.starts_with(&format!(
                    "Task file /w/task file.json: acceptance: a Task of kind {kind} is judged by \
                     a domain"
                )) && refused.contains("drop it and state the items in the goal"),
                "{refused}"
            );
            // The typed task_file_invalid diagnosis, as `limits.wall_ms` is refused.
            let (message, detail) = refusal(file, &record, "why");
            assert!(message.starts_with("Task file /w/task file.json: acceptance: why; "));
            assert_eq!(detail.cause, crate::error_document::Cause::TaskFileInvalid);
            assert_eq!(
                serde_json::Value::Object(detail.details),
                serde_json::json!({
                    "file": "/w/task file.json", "field": "acceptance", "value": ["a"]
                })
            );
            let next = detail.next_action.unwrap();
            assert!(
                next.starts_with("af task plan --file '/w/task file.json'"),
                "{next}"
            );
            assert!(
                refused.ends_with(&format!("then run `{next}` again")),
                "{refused}"
            );
        }
    }
}
