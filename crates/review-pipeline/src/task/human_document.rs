//! The human presentation of one recorded Document or report (ADR-0151): readable Markdown
//! derived from the exact draft and sources its canonical Document was rendered from, with the
//! Task's acceptance and the matching independent review stated in plain words. The
//! presentation is never recorded, checked or reviewed; the canonical Document stays the
//! accepted bytes.
//!
//! Authored text stays text in CommonMark with the GitHub Flavored Markdown extensions (tables,
//! strikethrough, autolinks, task lists, footnotes and the tag filter) and dollar math. Every
//! line written here starts at a column this renderer chose: a generated heading, list marker
//! or paragraph start, never an authored indentation. The first character of every line's
//! authored text is escaped when it could begin a block, and characters that open inline
//! syntax anywhere are escaped wherever they appear. Only ASCII spaces and tabs are layout, as
//! Markdown reads it: any other whitespace is authored text and kept where it stands, even at a
//! line start, in an indented example or on an otherwise empty line. Words a GFM autolink could
//! claim go into code spans of one backtick, which no extension reads into. Only a captured
//! source location that passes the canonical renderer's grammar becomes a link, and rendering
//! fetches nothing.
use super::document::{render_document, safe_source_location};
use super::envelope;
use super::report_task::{read_draft, render_report};
use super::source::{invocation_producer, source_snapshot};
use review_core::PortCardinality;
use review_core::task::document::*;
use review_core::task::execution::TaskInvocationV1;
use review_core::task::pipeline::ReceiptOutcomeV1;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::report_task::*;
use review_core::task::{
    ArtifactInputV1, EXECUTION_PLAN_V1, TaskAcceptanceV1, TaskExecutionV1, TaskResultV1,
    TaskRevisionV1,
};
use review_core::{ArtifactEnvelope, Producer, is_digest};
use review_store::Cas;
use review_store::store::task::task_run_id;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};

/// The `af task output --format` value that selects this presentation.
pub const HUMAN_MARKDOWN: &str = "human-markdown";

/// The deepest list nesting the presentation draws; deeper items stay at this depth.
const MAX_LIST_DEPTH: usize = 6;

/// What the automated checks of one exact Document concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Checks {
    outcome: ReceiptOutcomeV1,
    failed: Vec<String>,
    citation_failures: Vec<ReportCitationFailureV1>,
}

/// The recorded acceptance of one exact Document.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Review {
    /// No acceptance receipt this Task counted as evidence judges this Document.
    Unrecorded,
    Recorded {
        checks: Checks,
        /// The independent verifier's outcome and summary. The summary is untrusted text,
        /// rendered as inertly as the draft.
        evaluation: Option<(ReceiptOutcomeV1, String)>,
    },
}

/// One recorded Document, its exact draft and sources, and the acceptance that judged it,
/// every identity already checked against the recorded lineage.
#[derive(Debug, Clone)]
pub struct HumanDocument {
    task_id: String,
    port: String,
    draft: DocumentDraftV1,
    repository_citations: BTreeSet<RepositoryCitationV1>,
    sources: BTreeMap<String, DocumentSourceV1>,
    acceptance: TaskAcceptanceV1,
    execution: TaskExecutionV1,
    review: Review,
}

/// The human Markdown of `port` of a finished Task, or why it cannot be shown. `task` is the
/// revision the result names: review evidence is shown only when a plan of exactly this
/// revision, its inputs and a verifier policy it names produced it.
pub fn human_markdown(
    cas: &Cas,
    task_id: &str,
    port: &str,
    result: &TaskResultV1,
    task: &TaskRevisionV1,
) -> Result<String, String> {
    Ok(HumanDocument::recorded(cas, task_id, port, result, task)?.render())
}

fn payload<T: DeserializeOwned>(artifact: &ArtifactEnvelope) -> Result<T, String> {
    serde_json::from_value(artifact.payload.clone()).map_err(|e| e.to_string())
}

/// The artifact `id`, which must have type `ty` and the subject Snapshot `snapshot`.
fn typed(
    cas: &Cas,
    id: &str,
    ty: &str,
    snapshot: Option<&str>,
) -> Result<ArtifactEnvelope, String> {
    let artifact = envelope(cas, id)?;
    if artifact.artifact_type != ty || artifact.subject_snapshot_id.as_deref() != snapshot {
        return Err(format!(
            "Recorded review evidence names {id}, which is not the {ty} of this report"
        ));
    }
    Ok(artifact)
}

/// The one acceptance receipt of type `ty` among the result's outputs, counted as its
/// evidence, that judges `document_id`. A receipt of another Document is not this one's.
fn receipt<T: DeserializeOwned>(
    cas: &Cas,
    result: &TaskResultV1,
    ty: &str,
    document_id: &str,
    judged: impl Fn(&T) -> &str,
) -> Result<Option<(ArtifactEnvelope, T)>, String> {
    let mut found = None;
    for output in result.outputs.values() {
        if output.artifact_type != ty {
            continue;
        }
        for id in &output.artifact_ids {
            if !result.evidence.contains(id) {
                continue;
            }
            let artifact = envelope(cas, id)?;
            if artifact.artifact_type != ty {
                return Err("Task output differs from its recorded artifact contract".into());
            }
            let value: T = payload(&artifact)?;
            if judged(&value) != document_id {
                continue;
            }
            if found.is_some() {
                return Err("Several acceptance receipts judge this one Document".into());
            }
            found = Some((artifact, value));
        }
    }
    Ok(found)
}

fn failed_checks(checks: &BTreeMap<String, ReceiptOutcomeV1>) -> Vec<String> {
    checks
        .iter()
        .filter(|(_, outcome)| **outcome != ReceiptOutcomeV1::Passed)
        .map(|(name, _)| name.clone())
        .collect()
}

/// The outcome acceptance derives from these checks and this evaluation, as both profiles
/// derive it; a receipt that states another is refused.
fn derived(checks: ReceiptOutcomeV1, evaluation: Option<ReceiptOutcomeV1>) -> ReceiptOutcomeV1 {
    match (checks, evaluation) {
        (ReceiptOutcomeV1::Failed, _) => ReceiptOutcomeV1::Failed,
        (_, Some(outcome)) => outcome,
        (_, None) => ReceiptOutcomeV1::Inconclusive,
    }
}

const STALE: &str =
    "Recorded review evidence does not judge this exact report; nothing was written";

const NO_REQUIREMENTS: &str =
    "The Task revision has no single requirements input to match its review";

/// Whether `artifact` retains every one of `ids` among its inputs.
fn retains(artifact: &ArtifactEnvelope, ids: &[&str]) -> bool {
    ids.iter().all(|id| {
        artifact
            .input_artifacts
            .iter()
            .any(|kept| kept.as_str() == *id)
    })
}

/// One artifact bound to a single port, as an invocation records it.
fn bound(id: &str, ty: &str, snapshot: Option<&str>) -> ArtifactInputV1 {
    ArtifactInputV1 {
        artifact_ids: vec![id.into()],
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        snapshot_id: snapshot.map(String::from),
    }
}

/// The Task whose evidence may be shown: the revision its finished result names, and the run
/// its Attempts and kernel operations are recorded under.
struct Lineage<'a> {
    result: &'a TaskResultV1,
    task: &'a TaskRevisionV1,
    run_id: String,
}

impl<'a> Lineage<'a> {
    fn new(
        task_id: &str,
        result: &'a TaskResultV1,
        task: &'a TaskRevisionV1,
    ) -> Result<Self, String> {
        if task.task_id != task_id {
            return Err("The Task result names a revision of another Task".into());
        }
        Ok(Self {
            result,
            task,
            run_id: task_run_id(task_id).map_err(|e| e.to_string())?,
        })
    }

    /// The one artifact of this revision's input `port`.
    fn input(&self, port: &str) -> Option<&'a str> {
        let task: &'a TaskRevisionV1 = self.task;
        match task.inputs.get(port) {
            Some(input) if input.artifact_ids.len() == 1 => Some(input.artifact_ids[0].as_str()),
            _ => None,
        }
    }

    /// Whether `producer` is a started Attempt of this Task's run.
    fn attempt(&self, producer: &Producer) -> bool {
        matches!(producer, Producer::Attempt { run_id, .. } if *run_id == self.run_id)
    }

    /// Whether an acceptance receipt is this Task's own: recorded by the kernel operation of
    /// its exact invocation, under a plan of this very revision with its inputs and authority,
    /// for a verifier policy this revision names for `evidence_type`, binding exactly `inputs`
    /// and retaining every artifact they name.
    fn accepted(
        &self,
        cas: &Cas,
        receipt: &ArtifactEnvelope,
        invocation: &TaskInvocationV1,
        policy_id: &str,
        evidence_type: &str,
        inputs: &BTreeMap<String, ArtifactInputV1>,
    ) -> Result<bool, String> {
        let plan = envelope(cas, &invocation.plan_id)?;
        if plan.artifact_type != EXECUTION_PLAN_V1 {
            return Ok(false);
        }
        let plan: ExecutionPlanV1 = payload(&plan)?;
        if plan.task_revision_id != self.result.task_revision_id
            || plan.authority != self.task.authority
            || plan.inputs != self.task.inputs
            || !self.task.acceptance.values().any(|obligation| {
                obligation.evidence_type == evidence_type && obligation.verifier_policy == policy_id
            })
            || invocation.inputs != *inputs
        {
            return Ok(false);
        }
        let ids: Vec<&str> = inputs
            .values()
            .flat_map(|input| input.artifact_ids.iter().map(String::as_str))
            .collect();
        Ok(
            receipt.producer == invocation_producer(cas, invocation, None)?
                && retains(receipt, &ids),
        )
    }
}

fn report_review(
    cas: &Cas,
    lineage: &Lineage<'_>,
    document_id: &str,
    document: &DocumentV1,
    snapshot: &str,
    captured: &ReportSourcesV1,
) -> Result<Review, String> {
    let Some((artifact, verification)) = receipt::<ReportVerificationV1>(
        cas,
        lineage.result,
        REPORT_VERIFICATION_V1,
        document_id,
        |v| &v.document_id,
    )?
    else {
        return Ok(Review::Unrecorded);
    };
    verification.validate()?;
    let check_artifact = typed(
        cas,
        &verification.check_receipt_id,
        REPORT_CHECK_RECEIPT_V1,
        Some(snapshot),
    )?;
    let checks: ReportCheckReceiptV1 = payload(&check_artifact)?;
    checks.validate()?;
    // The Task's own source input names the Snapshot every receipt binds and the Manifest its
    // checks resolved citations against.
    let source = lineage
        .task
        .inputs
        .get("source")
        .ok_or("The Task revision has no source Snapshot to match its review")?;
    let (source_snapshot_id, recorded, _) = source_snapshot(cas, source)?;
    let source_id = source.artifact_ids[0].as_str();
    let manifest_id = recorded.manifest_id.as_str();
    // A Task that captured sources is judged against exactly those; one that bound none, only
    // against the empty set the seal records.
    let sources_bound = match lineage.task.inputs.get("sources") {
        Some(sources) => sources.artifact_ids == [document.sources_id.clone()],
        None => captured.sources.is_empty(),
    };
    let mut inputs = BTreeMap::from([
        (
            "document".to_string(),
            bound(document_id, DOCUMENT_V1, Some(snapshot)),
        ),
        (
            "checks".to_string(),
            bound(
                &verification.check_receipt_id,
                REPORT_CHECK_RECEIPT_V1,
                Some(snapshot),
            ),
        ),
        ("source".to_string(), source.clone()),
    ]);
    if let Some(id) = &verification.evaluation_id {
        inputs.insert(
            "evaluation".into(),
            bound(id, REPORT_EVALUATION_V1, Some(snapshot)),
        );
    }
    if source_snapshot_id != snapshot
        || !sources_bound
        || artifact.subject_snapshot_id.as_deref() != Some(snapshot)
        || verification.source_snapshot_id != snapshot
        || !lineage.accepted(
            cas,
            &artifact,
            &verification.invocation,
            &verification.policy_id,
            REPORT_VERIFICATION_V1,
            &inputs,
        )?
        || !retains(&artifact, &[manifest_id])
        || checks.document_id != document_id
        || checks.sources_id != document.sources_id
        || checks.source_snapshot_id != snapshot
        || checks.plan_id != verification.invocation.plan_id
        || checks.policy_id != verification.policy_id
        || checks.manifest_id != manifest_id
        || !lineage.attempt(&check_artifact.producer)
        || !retains(
            &check_artifact,
            &[
                document_id,
                document.sources_id.as_str(),
                manifest_id,
                source_id,
            ],
        )
    {
        return Err(STALE.into());
    }
    let evaluation = match &verification.evaluation_id {
        Some(id) => {
            let evaluation_artifact = typed(cas, id, REPORT_EVALUATION_V1, Some(snapshot))?;
            let value: ReportEvaluationV1 = payload(&evaluation_artifact)?;
            value.validate()?;
            let requirements = lineage.input("requirements").ok_or(NO_REQUIREMENTS)?;
            // A verifier reads exactly the sources its checks judged; only the recorded empty
            // set may go unbound at it (ADR-0133), so captured sources must be among its inputs.
            let read_sources = captured.sources.is_empty()
                || retains(&evaluation_artifact, &[document.sources_id.as_str()]);
            if value.document_id != document_id
                || value.sources_id != document.sources_id
                || value.check_receipt_id != verification.check_receipt_id
                || value.source_snapshot_id != snapshot
                || value.requirements_id != requirements
                || !lineage.attempt(&evaluation_artifact.producer)
                || !read_sources
                || !retains(
                    &evaluation_artifact,
                    &[
                        document_id,
                        requirements,
                        verification.check_receipt_id.as_str(),
                        source_id,
                    ],
                )
            {
                return Err(STALE.into());
            }
            Some((value.outcome, value.summary))
        }
        None => None,
    };
    if verification.outcome != derived(checks.outcome, evaluation.as_ref().map(|e| e.0)) {
        return Err(STALE.into());
    }
    Ok(Review::Recorded {
        checks: Checks {
            outcome: checks.outcome,
            failed: failed_checks(&checks.checks),
            citation_failures: checks.citation_failures,
        },
        evaluation,
    })
}

fn document_review(
    cas: &Cas,
    lineage: &Lineage<'_>,
    document_id: &str,
    document: &DocumentV1,
) -> Result<Review, String> {
    let Some((artifact, verification)) = receipt::<DocumentVerificationV1>(
        cas,
        lineage.result,
        DOCUMENT_VERIFICATION_V1,
        document_id,
        |v| &v.document_id,
    )?
    else {
        return Ok(Review::Unrecorded);
    };
    verification.validate()?;
    let check_artifact = typed(
        cas,
        &verification.check_receipt_id,
        DOCUMENT_CHECK_RECEIPT_V1,
        None,
    )?;
    let checks: DocumentCheckReceiptV1 = payload(&check_artifact)?;
    checks.validate()?;
    let mut inputs = BTreeMap::from([
        (
            "document".to_string(),
            bound(document_id, DOCUMENT_V1, None),
        ),
        (
            "checks".to_string(),
            bound(
                &verification.check_receipt_id,
                DOCUMENT_CHECK_RECEIPT_V1,
                None,
            ),
        ),
    ]);
    if let Some(id) = &verification.evaluation_id {
        inputs.insert("evaluation".into(), bound(id, DOCUMENT_EVALUATION_V1, None));
    }
    if lineage.input("sources") != Some(document.sources_id.as_str())
        || artifact.subject_snapshot_id.is_some()
        || !lineage.accepted(
            cas,
            &artifact,
            &verification.invocation,
            &verification.policy_id,
            DOCUMENT_VERIFICATION_V1,
            &inputs,
        )?
        || checks.document_id != document_id
        || checks.sources_id != document.sources_id
        || checks.plan_id != verification.invocation.plan_id
        || checks.policy_id != verification.policy_id
        || !lineage.attempt(&check_artifact.producer)
        || !retains(
            &check_artifact,
            &[document_id, document.sources_id.as_str()],
        )
    {
        return Err(STALE.into());
    }
    let evaluation = match &verification.evaluation_id {
        Some(id) => {
            let evaluation_artifact = typed(cas, id, DOCUMENT_EVALUATION_V1, None)?;
            let value: DocumentEvaluationV1 = payload(&evaluation_artifact)?;
            value.validate()?;
            let requirements = lineage.input("requirements").ok_or(NO_REQUIREMENTS)?;
            if value.document_id != document_id
                || value.sources_id != document.sources_id
                || value.check_receipt_id != verification.check_receipt_id
                || value.requirements_id != requirements
                || !lineage.attempt(&evaluation_artifact.producer)
                || !retains(
                    &evaluation_artifact,
                    &[
                        document_id,
                        document.sources_id.as_str(),
                        requirements,
                        verification.check_receipt_id.as_str(),
                    ],
                )
            {
                return Err(STALE.into());
            }
            Some((value.outcome, value.summary))
        }
        None => None,
    };
    if verification.outcome != derived(checks.outcome, evaluation.as_ref().map(|e| e.0)) {
        return Err(STALE.into());
    }
    Ok(Review::Recorded {
        checks: Checks {
            outcome: checks.outcome,
            failed: failed_checks(&checks.checks),
            citation_failures: Vec::new(),
        },
        evaluation,
    })
}

impl HumanDocument {
    /// Read `port` of a finished Task. Its Document must render again, byte for byte, from the
    /// draft and sources it names, and only an acceptance receipt that judges exactly that
    /// Document, recorded by this Task revision's own plan and verifier policy with its own
    /// check receipt and evaluation, is shown as its review.
    pub fn recorded(
        cas: &Cas,
        task_id: &str,
        port: &str,
        result: &TaskResultV1,
        task: &TaskRevisionV1,
    ) -> Result<Self, String> {
        let lineage = Lineage::new(task_id, result, task)?;
        let value = result
            .outputs
            .get(port)
            .ok_or_else(|| port_not_found(task_id, port, result))?;
        if value.cardinality != PortCardinality::One || value.artifact_ids.len() != 1 {
            return Err("Output command requires a single artifact port".into());
        }
        let document_id = &value.artifact_ids[0];
        let artifact = envelope(cas, document_id)?;
        if artifact.artifact_type != DOCUMENT_V1 {
            return Err(format!(
                "Human Markdown presents only a Document or report (af/Document@1); port {port} holds {}. Write it with --format markdown or --format json",
                artifact.artifact_type
            ));
        }
        if artifact.artifact_id != *document_id
            || value.artifact_type != DOCUMENT_V1
            || artifact.subject_snapshot_id != value.snapshot_id
        {
            return Err("Task output differs from its recorded artifact contract".into());
        }
        let document: DocumentV1 = payload(&artifact)?;
        document.validate()?;
        if !artifact.input_artifacts.contains(&document.draft_id)
            || !artifact.input_artifacts.contains(&document.sources_id)
        {
            return Err("Document does not retain the draft and sources it names".into());
        }
        let snapshot = artifact.subject_snapshot_id.as_deref();
        let sources = envelope(cas, &document.sources_id)?;
        if sources.subject_snapshot_id.is_some() {
            return Err("Document sources must be data-only".into());
        }
        let (draft, repository_citations, captured, canonical, review) =
            match (sources.artifact_type.as_str(), snapshot) {
                (REPORT_SOURCES_V1, Some(snapshot)) if is_digest(snapshot) => {
                    let captured: ReportSourcesV1 = payload(&sources)?;
                    let draft = read_draft(cas, &document.draft_id, snapshot)?;
                    let canonical = render_report(&draft, &captured)?;
                    let review =
                        report_review(cas, &lineage, document_id, &document, snapshot, &captured)?;
                    (
                        draft.first_version(),
                        draft.repository_citations(),
                        captured.sources,
                        canonical,
                        review,
                    )
                }
                (DOCUMENT_SOURCES_V1, None) => {
                    let captured: DocumentSourcesV1 = payload(&sources)?;
                    let draft: DocumentDraftV1 =
                        payload(&typed(cas, &document.draft_id, DOCUMENT_DRAFT_V1, None)?)?;
                    let canonical = render_document(&draft, &captured)?;
                    let review = document_review(cas, &lineage, document_id, &document)?;
                    (draft, BTreeSet::new(), captured.sources, canonical, review)
                }
                _ => {
                    return Err(
                        "Document names sources of neither the Document nor the report profile"
                            .into(),
                    );
                }
            };
        if canonical != document.text {
            return Err(
                "The recorded Document differs from its draft and sources rendered again; nothing was written"
                    .into(),
            );
        }
        Ok(Self {
            task_id: task_id.into(),
            port: port.into(),
            draft,
            repository_citations,
            sources: captured,
            acceptance: result.acceptance,
            execution: result.execution,
            review,
        })
    }

    /// The presentation: title, plain-language result, the authored sections in their order,
    /// sources, repository citations, the review and a note on what this file is.
    pub fn render(&self) -> String {
        let mut blocks = vec![
            format!("# {}", heading(&self.draft.title)),
            self.result_line(),
        ];
        for section in &self.draft.sections {
            blocks.push(format!("## {}", heading(&section.heading)));
            blocks.extend(body(&section.body));
        }
        if !self.draft.citations.is_empty() {
            blocks.push("## Sources".into());
            blocks.push(self.sources_list());
        }
        if !self.repository_citations.is_empty() {
            blocks.push("## Repository citations".into());
            blocks.push(
                self.repository_citations
                    .iter()
                    .map(|citation| format!("- {}", inert_code(&citation.display())))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        blocks.push("## Review".into());
        blocks.extend(self.review_blocks());
        blocks.push(self.about());
        let mut rendered = blocks.join("\n\n");
        rendered.push('\n');
        rendered
    }

    fn result_line(&self) -> String {
        let label = match self.acceptance {
            TaskAcceptanceV1::Satisfied => "accepted",
            TaskAcceptanceV1::Unsatisfied => "changes requested",
            TaskAcceptanceV1::Inconclusive => "incomplete",
        };
        let reason = match &self.review {
            Review::Unrecorded => {
                "No acceptance receipt of this Task judges this exact output, so no checks or review are shown for it."
            }
            Review::Recorded { checks, evaluation } => match (checks.outcome, evaluation) {
                (ReceiptOutcomeV1::Failed, _) => {
                    "The report failed its automated checks, so no independent review ran."
                }
                (ReceiptOutcomeV1::Inconclusive, _) => {
                    "The report's automated checks were inconclusive."
                }
                (_, Some((ReceiptOutcomeV1::Passed, _))) => {
                    "The report passed its automated checks and its independent review."
                }
                (_, Some((ReceiptOutcomeV1::Failed, _))) => {
                    "The report passed its automated checks, but the independent reviewer asked for changes."
                }
                (_, Some((ReceiptOutcomeV1::Inconclusive, _))) => {
                    "The report passed its automated checks; its independent review was inconclusive."
                }
                (_, None) => {
                    "The report passed its automated checks; no independent review was recorded."
                }
            },
        };
        let execution = match self.execution {
            TaskExecutionV1::Completed => "",
            TaskExecutionV1::Incomplete => " The Task stopped before it completed.",
            TaskExecutionV1::Exhausted => " The Task used up its budget before it completed.",
        };
        format!("**Result: {label}.** {reason}{execution}")
    }

    fn sources_list(&self) -> String {
        self.draft
            .citations
            .iter()
            .filter_map(|name| self.sources.get(name))
            .map(|source| {
                if safe_source_location(&source.uri) && source.uri.starts_with("https://") {
                    format!(
                        "- [{}](<{}>), revision {}",
                        inline(&source.title, false),
                        link_destination(&source.uri),
                        inline(&source.revision, false)
                    )
                } else {
                    // Unsupported locations stay inert, as the canonical renderer keeps them.
                    format!(
                        "- {}: {}, revision {}",
                        inline(&source.title, true),
                        inert_code(&source.uri),
                        inline(&source.revision, false)
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn review_blocks(&self) -> Vec<String> {
        let (checks, evaluation) = match &self.review {
            Review::Unrecorded => {
                return vec![
                    "No acceptance receipt of this Task judges this exact output: there are no checks or review to show."
                        .into(),
                ];
            }
            Review::Recorded { checks, evaluation } => (checks, evaluation),
        };
        let mut blocks = vec![match checks.outcome {
            ReceiptOutcomeV1::Passed => "Automated checks: passed.".to_string(),
            ReceiptOutcomeV1::Failed => format!(
                "Automated checks: failed ({}).",
                checks
                    .failed
                    .iter()
                    .map(|name| check_name(name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ReceiptOutcomeV1::Inconclusive => "Automated checks: inconclusive.".into(),
        }];
        if !checks.citation_failures.is_empty() {
            blocks.push("These repository citations do not name a text line of the source:".into());
            blocks.push(
                checks
                    .citation_failures
                    .iter()
                    .map(|failure| {
                        format!(
                            "- {}: {}",
                            inert_code(&failure.citation.display()),
                            citation_reason(failure.reason)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        blocks.push(match (evaluation, checks.outcome) {
            (Some((outcome, _)), _) => format!("Independent review: {}.", outcome_word(*outcome)),
            (None, ReceiptOutcomeV1::Failed) => {
                "Independent review: did not run, because the automated checks failed.".into()
            }
            (None, _) => "Independent review: not recorded.".into(),
        });
        if let Some((_, summary)) = evaluation {
            blocks.push("The reviewer's summary, as recorded:".into());
            blocks.extend(body(summary));
        }
        blocks
    }

    /// What this file is, and what judged the canonical report: only the checks and the review
    /// that were actually recorded for it.
    fn about(&self) -> String {
        let canonical = inert_code(&format!(
            "af task output {} --port {} --format canonical-markdown",
            self.task_id, self.port
        ));
        let judged = match &self.review {
            Review::Unrecorded => format!(
                "No recorded checks or review judge this output; {canonical} writes its canonical report."
            ),
            Review::Recorded {
                evaluation: Some(_),
                ..
            } => format!(
                "The automated checks and the independent reviewer judged the canonical report, which {canonical} writes, not this layout."
            ),
            Review::Recorded { checks, .. } if checks.outcome == ReceiptOutcomeV1::Failed => {
                format!(
                    "The automated checks judged the canonical report, which {canonical} writes, not this layout. They failed, so no independent reviewer ran."
                )
            }
            Review::Recorded { .. } => format!(
                "The automated checks judged the canonical report, which {canonical} writes, not this layout. No independent review was recorded."
            ),
        };
        format!(
            "About this file: a readable presentation of the {} output of Task {}, made from the draft and sources that render exactly to its recorded report. {judged}",
            inert_code(&self.port),
            inert_code(&self.task_id),
        )
    }
}

fn outcome_word(outcome: ReceiptOutcomeV1) -> &'static str {
    match outcome {
        ReceiptOutcomeV1::Passed => "passed",
        ReceiptOutcomeV1::Failed => "changes requested",
        ReceiptOutcomeV1::Inconclusive => "inconclusive",
    }
}

fn check_name(name: &str) -> String {
    match name {
        "size" => "size limit".into(),
        "required_sections" => "required sections".into(),
        "citations" => "source citations".into(),
        "source_locations" => "source locations".into(),
        "repository_citations" => "repository citations".into(),
        other => inline(&other.replace(['_', '-'], " "), false),
    }
}

fn citation_reason(reason: ReportCitationFailureReasonV1) -> &'static str {
    use ReportCitationFailureReasonV1::*;
    match reason {
        Absent => "no such file in the source Snapshot",
        Directory => "a directory, not a file",
        Symlink => "a symbolic link",
        Binary => "a binary file",
        LineOutOfRange => "the line is past the end of the file",
    }
}

/// The layout padding of a line: ASCII spaces and tabs only, as [`indent`] counts and as
/// Markdown reads indentation, blank lines and the padding around a fence, a heading or a
/// paragraph. Any other whitespace, such as a no-break, em or ideographic space, is authored
/// content: it is never trimmed, dedented or read as a blank line, and a line holding it beside
/// a run of fence characters neither opens nor closes an example.
const PAD: [char; 2] = [' ', '\t'];

/// Whether a line holds only layout padding.
fn blank(line: &str) -> bool {
    line.trim_matches(PAD).is_empty()
}

/// The column width of a line's leading spaces and tabs, a tab reaching the next stop of four.
fn indent(line: &str) -> usize {
    let mut width = 0;
    for c in line.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width += 4 - width % 4,
            _ => break,
        }
    }
    width
}

/// One authored plain-text body as Markdown blocks: paragraphs, lists, and code-like examples
/// (an authored fence that closes, a block indented by four columns, or a pipe table) shown
/// verbatim in a code block. Everything else is text.
fn body(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let fences = closed_fences(&lines);
    let mut blocks = Vec::new();
    let mut i = 0;
    let mut block_start = true;
    // The run of the list just drawn, which a list after only blank lines would continue.
    let mut open: Option<Run> = None;
    let ends = |at: usize| blank(lines[at]) || fences[at].is_some();
    while i < lines.len() {
        let line = lines[i];
        if blank(line) {
            i += 1;
            block_start = true;
            continue;
        }
        if let Some((next, block)) = fenced(&lines, fences[i], i) {
            blocks.push(block);
            open = None;
            i = next;
            block_start = true;
            continue;
        }
        if block_start && indent(line) >= 4 {
            let mut last = i;
            let mut end = i;
            while end < lines.len() && (blank(lines[end]) || indent(lines[end]) >= 4) {
                if !blank(lines[end]) {
                    last = end;
                }
                end += 1;
            }
            blocks.push(code_block("", &dedent(&lines[i..=last])));
            open = None;
            i = last + 1;
            continue;
        }
        let start = i;
        i += 1;
        if list_item(line).is_some() {
            while i < lines.len()
                && !ends(i)
                && (list_item(lines[i]).is_some() || indent(lines[i]) > 0)
            {
                i += 1;
            }
            let (block, run) = list(&lines[start..i], open);
            blocks.push(block);
            open = run;
            block_start = false;
            continue;
        } else {
            while i < lines.len() && !ends(i) && list_item(lines[i]).is_none() {
                i += 1;
            }
            blocks.push(paragraph(&lines[start..i]));
        }
        open = None;
        block_start = false;
    }
    blocks
}

/// Lines of text, each placed where a block could begin. A pipe table is shown verbatim.
fn paragraph(lines: &[&str]) -> String {
    let table = lines.len() >= 2
        && lines
            .iter()
            .all(|line| line.trim_start_matches(PAD).starts_with('|'))
        && lines[1].contains('-')
        && lines[1]
            .trim_matches(PAD)
            .chars()
            .all(|c| matches!(c, '|' | ':' | '-' | ' '));
    if table {
        return code_block("", &dedent(lines));
    }
    lines
        .iter()
        .map(|line| inline(line.trim_matches(PAD), true))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An authored fence that a later line closes: the opening line's indentation, its info word
/// and the index of the closing line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fence<'a> {
    width: usize,
    info: &'a str,
    close: usize,
}

/// The fence character, run length, indentation and info word of a line that opens a fence. A
/// fence whose info string is not one plain word stays text.
fn opener(line: &str) -> Option<(char, usize, usize, &str)> {
    let width = indent(line);
    if width > 3 {
        return None;
    }
    let text = line.trim_start_matches(PAD);
    let fence = text.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = text.chars().take_while(|c| *c == fence).count();
    if length < 3 {
        return None;
    }
    let info = text[length..].trim_matches(PAD);
    if info.len() > 32
        || !info
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.' | '#'))
    {
        return None;
    }
    Some((fence, length, width, info))
}

/// For every line, the fence it opens when a later line closes it: the first later line,
/// indented at most three columns as an opener is, that holds only the same fence character,
/// at least as many times, padded by [`PAD`] alone; a line indented further is content
/// of the example. One backward pass keeps, per fence character, the closing lines no nearer
/// line at least as long hides, nearest last and so with lengths falling towards the end; each
/// opener finds its closer by binary search. An unclosed opener therefore costs no scan of the
/// lines after it.
fn closed_fences<'a>(lines: &[&'a str]) -> Vec<Option<Fence<'a>>> {
    let mut fences = vec![None; lines.len()];
    let mut closers: [Vec<(usize, usize)>; 2] = [Vec::new(), Vec::new()];
    for (at, &line) in lines.iter().enumerate().rev() {
        if let Some((fence, length, width, info)) = opener(line) {
            let stack = &closers[usize::from(fence == '~')];
            let longer = stack.partition_point(|&(_, run)| run >= length);
            if let Some(&(close, _)) = longer.checked_sub(1).and_then(|top| stack.get(top)) {
                fences[at] = Some(Fence { width, info, close });
            }
        }
        let candidate = line.trim_matches(PAD);
        if indent(line) <= 3
            && let Some(fence) = candidate.chars().next().filter(|c| matches!(c, '`' | '~'))
            && candidate.len() >= 3
            && candidate.chars().all(|c| c == fence)
        {
            let stack = &mut closers[usize::from(fence == '~')];
            while stack.last().is_some_and(|&(_, run)| run <= candidate.len()) {
                stack.pop();
            }
            stack.push((at, candidate.len()));
        }
    }
    fences
}

/// The authored example `fence` opens at `lines[at]`: the index after its closing line and the
/// example as a code block.
fn fenced(lines: &[&str], fence: Option<Fence<'_>>, at: usize) -> Option<(usize, String)> {
    let Fence { width, info, close } = fence?;
    let content: Vec<&str> = lines[at + 1..close]
        .iter()
        .map(|line| {
            let strip = line
                .char_indices()
                .take(width)
                .take_while(|(_, c)| *c == ' ')
                .count();
            &line[strip..]
        })
        .collect();
    Some((close + 1, code_block(info, &content)))
}

/// The lines without the leading [`PAD`] every non-blank one shares. Whitespace after that
/// indentation, such as a no-break space, is content of the example and stays.
fn dedent<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut common: Option<&str> = None;
    for line in lines.iter().filter(|line| !blank(line)) {
        let lead = &line[..line.len() - line.trim_start_matches(PAD).len()];
        common = Some(match common {
            None => lead,
            Some(prefix) => {
                let shared = prefix
                    .char_indices()
                    .zip(lead.chars())
                    .take_while(|((_, a), b)| a == b)
                    .last()
                    .map_or(0, |((i, c), _)| i + c.len_utf8());
                &prefix[..shared]
            }
        });
    }
    let common = common.unwrap_or("");
    lines
        .iter()
        .map(|line| line.strip_prefix(common).unwrap_or(""))
        .collect()
}

/// A fenced code block holding `lines` verbatim. Its fence is longer than any run of backticks
/// inside, so no line can close it.
fn code_block(info: &str, lines: &[&str]) -> String {
    let mut longest = 0;
    for line in lines {
        let mut run = 0;
        for c in line.chars() {
            run = if c == '`' { run + 1 } else { 0 };
            longest = longest.max(run);
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    let mut block = format!("{fence}{info}\n");
    for line in lines {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str(&fence);
    block
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Marker {
    Bullet,
    Ordered { digits: String, delimiter: char },
}

struct Item<'a> {
    indent: usize,
    marker: Marker,
    text: &'a str,
}

/// A plain-text list item: `-`, `*`, `+` or `•`, or one to nine digits and `.` or `)`, then
/// whitespace and some text.
fn list_item(line: &str) -> Option<Item<'_>> {
    let rest = line.trim_start_matches(PAD);
    let first = rest.chars().next()?;
    let (marker, after) = match first {
        '-' | '*' | '+' | '•' => (Marker::Bullet, &rest[first.len_utf8()..]),
        _ => {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            let delimiter = rest[digits..].chars().next()?;
            if !(1..=9).contains(&digits) || !matches!(delimiter, '.' | ')') {
                return None;
            }
            (
                Marker::Ordered {
                    digits: rest[..digits].into(),
                    delimiter,
                },
                &rest[digits + 1..],
            )
        }
    };
    if !after.starts_with(PAD) || blank(after) {
        return None;
    }
    Some(Item {
        indent: indent(line),
        marker,
        text: after.trim_matches(PAD),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    Bullet,
    /// An ordered run and the number its next item must carry to continue it.
    Ordered(u64),
}

struct Level {
    indent: usize,
    column: usize,
    content: usize,
    run: Option<Run>,
}

/// A plain-text list, nested by its authored indentation, and the run its outermost level ends
/// with. An ordered item keeps its number: one that does not continue its run is drawn as a
/// bullet that shows the number as text. Lines that are not items continue the deepest open
/// item. `open` is the run of a list drawn just before, separated only by blank lines, which
/// Markdown continues.
fn list(lines: &[&str], open: Option<Run>) -> (String, Option<Run>) {
    let mut out: Vec<String> = Vec::new();
    let mut levels: Vec<Level> = Vec::new();
    for line in lines {
        let Some(item) = list_item(line) else {
            let column = levels.last().map_or(0, |level| level.content);
            out.push(format!(
                "{}{}",
                " ".repeat(column),
                inline(line.trim_matches(PAD), true)
            ));
            continue;
        };
        while levels.len() > 1 && levels.last().is_some_and(|l| l.indent > item.indent) {
            levels.pop();
        }
        if levels
            .last()
            .is_none_or(|l| item.indent > l.indent && levels.len() < MAX_LIST_DEPTH)
        {
            let column = levels.last().map_or(0, |level| level.content);
            levels.push(Level {
                indent: item.indent,
                column,
                content: column,
                run: if levels.is_empty() { open } else { None },
            });
        }
        let level = levels.last_mut().expect("an open list level");
        let number = match &item.marker {
            Marker::Ordered { digits, .. } if digits == "0" || !digits.starts_with('0') => {
                digits.parse::<u64>().ok()
            }
            _ => None,
        };
        let (marker, shown, run) = match (&item.marker, number) {
            (Marker::Bullet, _) => ("- ".to_string(), String::new(), Run::Bullet),
            (Marker::Ordered { .. }, Some(n))
                if matches!(level.run, None | Some(Run::Bullet))
                    || level.run == Some(Run::Ordered(n)) =>
            {
                (format!("{n}. "), String::new(), Run::Ordered(n + 1))
            }
            (Marker::Ordered { digits, delimiter }, _) => (
                "- ".to_string(),
                format!("{digits}\\{delimiter} "),
                Run::Bullet,
            ),
        };
        // A new kind of list, or an ordered list that does not start at one, could not
        // interrupt the line above it: a blank line starts it.
        let restart = match (level.run, run) {
            (Some(Run::Bullet), Run::Bullet) => false,
            (Some(Run::Ordered(_)), Run::Ordered(_)) => false,
            (None, Run::Ordered(next)) => next != 2,
            (None, Run::Bullet) => false,
            _ => true,
        };
        if restart && !out.is_empty() {
            out.push(String::new());
        }
        out.push(format!(
            "{}{marker}{shown}{}",
            " ".repeat(level.column),
            inline(item.text, shown.is_empty())
        ));
        level.content = level.column + marker.len();
        level.run = Some(run);
    }
    (out.join("\n"), levels.first().and_then(|level| level.run))
}

/// The not-found refusal of an output port, in the shape every not-found refusal takes: the
/// port, the Task, and the ports its result does have.
pub fn port_not_found(task_id: &str, port: &str, result: &TaskResultV1) -> String {
    let ports: Vec<&str> = result.outputs.keys().map(String::as_str).collect();
    format!(
        "Output port {port} not found in the result of Task {task_id}; its ports are {}",
        if ports.is_empty() {
            "none".to_string()
        } else {
            ports.join(", ")
        }
    )
}

/// How a line's first character is judged. The human presentation escapes it only where it
/// opens a block; the canonical `af.document/2` rendering keeps the rule its recorded bytes were
/// written with, every ASCII punctuation but [`CANONICAL_BLOCK_SAFE`], since a Document is
/// accepted only when its draft renders to exactly those bytes again.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineStart {
    Human,
    Canonical,
}

/// Line-start characters the canonical `af.document/2` rendering leaves unescaped.
const CANONICAL_BLOCK_SAFE: &str = "()\"',./?;";

/// Authored text as inline Markdown that renders exactly that text. `start` marks text at the
/// start of a line, where a block could begin; only what is markup where it stands is escaped.
pub(crate) fn inline(text: &str, start: bool) -> String {
    escape(text, start, false, LineStart::Human)
}

/// [`inline`] as the canonical `af.document/2` rendering writes it.
pub(crate) fn canonical_inline(text: &str, start: bool) -> String {
    escape(text, start, false, LineStart::Canonical)
}

/// A generated heading's authored text; `#` is escaped so no closing sequence is dropped.
pub(crate) fn heading(text: &str) -> String {
    escape(text.trim_matches(PAD), false, true, LineStart::Human)
}

/// [`heading`] as the canonical `af.document/2` rendering writes it.
pub(crate) fn canonical_heading(text: &str) -> String {
    escape(text.trim_matches(PAD), false, true, LineStart::Canonical)
}

fn escape(text: &str, start: bool, heading: bool, rule: LineStart) -> String {
    // Leading padding at a line start would move where the block begins. Other whitespace is
    // text, and the first word after it is still escaped as a line's first.
    let text = if start { text.trim_matches(PAD) } else { text };
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut first = start;
    let mut rest = text;
    while !rest.is_empty() {
        let space = rest.starts_with(char::is_whitespace);
        let end = rest
            .find(|c: char| c.is_whitespace() != space)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        if space {
            out.push_str(run);
        } else if linkable(run) {
            out.push_str(&inert_code(run));
        } else {
            escape_word(run, first, heading, rule, &mut out);
        }
        first &= space;
    }
    out
}

/// A word a GFM autolink, an email address or a mention could claim.
fn linkable(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    word.contains('@')
        || ["://", "www.", "mailto:", "xmpp:"]
            .iter()
            .any(|p| lower.contains(p))
}

/// A captured HTTPS location as a `<...>` link destination a parser reads back exactly. The
/// location grammar already refuses `<`, `>`, `\` and whitespace; a parser still decodes
/// entity and numeric character references there, so every `&` is written as `&amp;`.
fn link_destination(uri: &str) -> String {
    uri.replace('&', "&amp;")
}

/// `text` verbatim and inert: code spans fenced by one backtick, with each backtick of `text`
/// escaped between them, so no fence is long enough to open a code block anywhere.
fn inert_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for (i, piece) in text.split('`').enumerate() {
        if i > 0 {
            out.push_str("\\`");
        }
        if piece.is_empty() {
            continue;
        }
        // A span strips one ASCII space from each end when both ends hold one and it is not
        // all ASCII spaces; pad to keep them. Tabs and other whitespace are never stripped.
        let pad = piece.starts_with(' ')
            && piece.ends_with(' ')
            && !piece.bytes().all(|byte| byte == b' ');
        let pad = if pad { " " } else { "" };
        out.push_str(&format!("`{pad}{piece}{pad}`"));
    }
    out
}

fn escape_word(word: &str, first: bool, heading: bool, rule: LineStart, out: &mut String) {
    let chars: Vec<char> = word.chars().collect();
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    let numbered = first
        && (1..=9).contains(&digits)
        && chars.len() == digits + 1
        && matches!(chars[digits], '.' | ')');
    let alphanumeric = |at: Option<usize>| {
        at.and_then(|i| chars.get(i))
            .is_some_and(|c| c.is_alphanumeric())
    };
    for (i, &c) in chars.iter().enumerate() {
        let escaped = match c {
            '\\' | '`' | '*' | '[' | ']' | '<' | '~' | '|' | '$' => true,
            // An underscore between letters or digits can neither open nor close emphasis.
            '_' => !(alphanumeric(i.checked_sub(1)) && alphanumeric(Some(i + 1))),
            '&' => entity(&chars[i + 1..]),
            '#' if heading => true,
            '.' | ')' if numbered && i == digits => true,
            _ if !first || i > 0 => false,
            _ if rule == LineStart::Canonical => {
                c.is_ascii_punctuation() && !CANONICAL_BLOCK_SAFE.contains(c)
            }
            _ => opens_block(&chars),
        };
        if escaped {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Whether a line's first word could open a block where it stands, so its first character is
/// escaped: a bullet (`-`, `+` or `*` before whitespace or the end), an ATX heading (one to six
/// `#` before a space or the end), a block quote (`>`), a thematic break, setext underline or
/// table delimiter row (`-`, `=` and `:` alone). Code fences (`` ` ``, `~`) and HTML (`<`) are
/// escaped wherever they stand, and a numbered marker at its `.` or `)`. Any other word, such as
/// `-foo`, `+1` or `!wow`, is a paragraph's text and stays as it is.
fn opens_block(word: &[char]) -> bool {
    match word[0] {
        '-' | '+' | '*' if word.len() == 1 => true,
        '#' => word.len() <= 6 && word.iter().all(|c| *c == '#'),
        '>' => true,
        '-' | '=' | ':' => word.iter().all(|c| matches!(c, '-' | '=' | ':')),
        _ => false,
    }
}

/// Whether the text after an `&` would read as an entity or numeric character reference.
fn entity(rest: &[char]) -> bool {
    let name = rest
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '#')
        .count();
    (1..=32).contains(&name) && rest.get(name) == Some(&';')
}

#[cfg(test)]
#[path = "human_document/tests.rs"]
mod tests;
