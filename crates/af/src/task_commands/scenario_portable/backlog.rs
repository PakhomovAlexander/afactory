//! Resolve native Scenario backlog actions against persisted Task and Project receipts.
use super::*;
use af_contracts::backlog::batch::assessment::{Assessment, CollectedBatch};
use af_contracts::task::document::{DOCUMENT_DRAFT_V3, DOCUMENT_V1, DocumentDraft, DocumentV1};
use af_contracts::task::pipeline::ReceiptOutcomeV1;
use af_contracts::task::report_domain::{
    REPORT_CHECK_RECEIPT_V1, REPORT_EVALUATION_V1, REPORT_SOURCES_V1, REPORT_VERIFICATION_V1,
    ReportCheckReceiptV1, ReportEvaluationV1, ReportVerificationV1,
};
use af_project::backlog::batch::NativeAssessment;
use af_scenario::contract::TaskReceipt;
use af_scenario::state::{Dispatch, Run};
use af_source_git::task::{read_origin, read_snapshot};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub(crate) fn run_backlog_action(
    run: &Run,
    dispatch: &Dispatch,
    grant: &Grant,
    cancellation: &AtomicBool,
) -> Result<Option<String>, String> {
    let deadline = Instant::now() + Duration::from_millis(dispatch.timeout_ms);
    let Some(Datum::Task(receipt)) = dispatch.inputs.get("assessment") else {
        return Err("Backlog action requires an accepted native assessment".into());
    };
    let frame = &run.frames[&dispatch.frame];
    let definition = &run.pin.bundle.definitions[&frame.scenario];
    let (template, target) = frame
        .steps
        .iter()
        .zip(&definition.steps)
        .find_map(|(progress, step)| {
            let Kind::FreshTask { template, .. } = &step.operation else {
                return None;
            };
            if !progress.complete()
                || progress.outputs.get("result") != Some(&Datum::Task(receipt.clone()))
            {
                return None;
            }
            let target = progress
                .intents
                .last()
                .and_then(|intent| run.fresh_tasks.get(&intent.id))?;
            Some((template, target))
        })
        .ok_or("Backlog assessment must come from a completed fresh Task in this frame")?;
    let sources = bound_sources(template.document["requirements"].as_object())?
        .ok_or("Backlog template has no captured sources")?;
    let source = sources
        .sources
        .get(BATCH_SOURCE)
        .ok_or("Backlog template lacks a frozen collection")?;
    let collection: CollectedBatch =
        serde_json::from_str(&source.text).map_err(|_| "Invalid frozen backlog collection")?;
    let checkout = af_project::require_checkout(&template.repo)?;
    require(
        grant.adapter == "builtin"
            && collection.reference.project_id == checkout.project_id
            && collection.reference.batch_id == dispatch.inputs["subject"].as_text()?
            && collection.reference.policy_id == dispatch.inputs["revision"].as_text()?
            && checkout.repository_id == template.repository_id
            && collection.source_revision == template.authority_commit
            && source.revision == digest(source.text.as_bytes())
            && target.repo == template.repo
            && target.state == template.state
            && target.task_id == receipt.task_id,
        "Backlog action differs from the receiving Project, frozen batch or captured revision",
    )?;
    let opened = open_task(&target.task_id, &target.repo, Some(&target.state))?;
    let (result, _) = opened.view()?;
    require(
        opened.task.revision_id == receipt.revision_id
            && opened.task.plan_id.as_deref() == Some(&receipt.plan_id)
            && matches!(&opened.task.phase, TaskPhaseV1::Finished { result_id } if result_id == &receipt.result_id)
            && result.as_ref() == Some(&receipt.result),
        "Backlog assessment is not the exact persisted Task result",
    )?;
    Datum::Task(receipt.clone()).validate()?;
    let document_id = output(receipt, "report", DOCUMENT_V1)?;
    let verification_id = output(receipt, "verification", REPORT_VERIFICATION_V1)?;
    let document: DocumentV1 = read(&opened.cas, document_id, DOCUMENT_V1)?;
    let verification: ReportVerificationV1 =
        read(&opened.cas, verification_id, REPORT_VERIFICATION_V1)?;
    let draft: DocumentDraft = read(&opened.cas, &document.draft_id, DOCUMENT_DRAFT_V3)?;
    let captured: ReportSourcesV1 = read(&opened.cas, &document.sources_id, REPORT_SOURCES_V1)?;
    let checks: ReportCheckReceiptV1 = read(
        &opened.cas,
        &verification.check_receipt_id,
        REPORT_CHECK_RECEIPT_V1,
    )?;
    let evaluation_id = verification
        .evaluation_id
        .as_deref()
        .ok_or("Backlog verification lacks an independent evaluation")?;
    let evaluation: ReportEvaluationV1 = read(&opened.cas, evaluation_id, REPORT_EVALUATION_V1)?;
    let snapshot_id = opened
        .task
        .revision
        .inputs
        .get("source")
        .and_then(|input| input.snapshot_id.as_deref())
        .ok_or("Backlog Task lacks its source Snapshot")?;
    let (snapshot, manifest) = read_snapshot(&opened.cas, snapshot_id)?;
    let origin = read_origin(&opened.cas, &snapshot.origin_id)?;
    let path = template.document["requirements"][BACKLOG_FILE]
        .as_str()
        .ok_or("Backlog assessment lacks its committed portable artifact")?;
    let bytes = captured_file(&opened.cas, &manifest, path)?;
    let artifact: Artifact = parse(Path::new(path), &bytes)?;
    require(
        artifact.schema == BACKLOG_SCHEMA
            && artifact.definition.id == frame.scenario
            && digest(&bytes) == collection.scenario_id
            && template.document["requirements"]["scenario_artifact"].as_str()
                == Some(&hash(&artifact)?),
        "Backlog assessment differs from its pinned portable Scenario artifact",
    )?;
    require(
        captured == sources
            && snapshot.content_digest == template.source_content_digest
            && origin.repository_id() == template.repository_id
            && origin.source_revision() == Some(&collection.source_revision)
            && verification.document_id == document_id
            && verification.outcome == ReceiptOutcomeV1::Passed
            && verification.source_snapshot_id == snapshot_id
            && checks.plan_id == receipt.plan_id
            && checks.document_id == document_id
            && checks.sources_id == document.sources_id
            && checks.policy_id == verification.policy_id
            && checks.source_snapshot_id == snapshot_id
            && checks.manifest_id == snapshot.manifest_id
            && checks.outcome == ReceiptOutcomeV1::Passed
            && checks.checks.get("backlog_assessment") == Some(&ReceiptOutcomeV1::Passed)
            && evaluation.document_id == document_id
            && evaluation.sources_id == document.sources_id
            && evaluation.source_snapshot_id == snapshot_id
            && evaluation.check_receipt_id == verification.check_receipt_id
            && evaluation.outcome == ReceiptOutcomeV1::Passed,
        "Backlog verification does not bind the exact collected source, assessment and checks",
    )?;
    let (collection, decisions) = Assessment::checked(source, &draft, origin.source_revision())?;
    let mut store = af_project::ProjectStore::open_waiting(
        &crate::project::Application,
        &crate::xdg_state_root()?,
        &checkout.project_id,
        af_project::Wait::Until(deadline),
    )
    .map_err(af_project::Unopened::message)?;
    let proof = NativeAssessment {
        collection: &collection,
        index_mode: crate::scenario::backlog::index_mode(&store, run, template)?,
        decisions: &decisions,
        manifest: &manifest,
        limits: &run.limits,
        deadline,
        result_id: &receipt.result_id,
        verification_id,
    };
    match grant.operation {
        Operation::BacklogSettleV1 => {
            store.check_backlog_action_destination(&collection.reference, &grant.destination)?;
            store.settle_native_backlog(&proof, cancellation).map(Some)
        }
        Operation::BacklogDispatchV1 => {
            let Some(Datum::Action(settlement)) = dispatch.inputs.get("settlement") else {
                return Err("Backlog dispatch lacks a native settlement receipt".into());
            };
            require(
                settlement.operation == Operation::BacklogSettleV1
                    && settlement.destination == grant.destination
                    && settlement.subject == collection.reference.batch_id
                    && settlement.revision == collection.reference.policy_id
                    && frame.steps.iter().any(|step| {
                        step.complete()
                            && step.outputs.get("result") == dispatch.inputs.get("settlement")
                    }),
                "Backlog dispatch must use the preceding Project settlement for the same batch",
            )?;
            store.dispatch_native_backlog(
                &proof,
                &settlement.evidence,
                &grant.destination,
                cancellation,
            )
        }
        _ => Err("Unknown native backlog action".into()),
    }
}

fn read<T: serde::de::DeserializeOwned>(
    cas: &Cas,
    id: &str,
    artifact_type: &str,
) -> Result<T, String> {
    let artifact = cas.get_artifact(id).map_err(|e| e.to_string())?;
    validate_envelope(&artifact)?;
    require(
        artifact.artifact_type == artifact_type,
        "Backlog proof envelope type differs",
    )?;
    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())
}

fn output<'a>(
    receipt: &'a TaskReceipt,
    name: &str,
    artifact_type: &str,
) -> Result<&'a str, String> {
    let output = receipt
        .result
        .outputs
        .get(name)
        .ok_or("Backlog Task output is missing")?;
    require(
        output.artifact_type == artifact_type
            && output.cardinality == PortCardinality::One
            && output.artifact_ids.len() == 1,
        "Backlog Task output contract differs",
    )?;
    Ok(&output.artifact_ids[0])
}
