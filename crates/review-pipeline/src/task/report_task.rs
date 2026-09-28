//! Report Tasks over the common Task scheduler and ledger (ADR-0125). A report is a Document
//! written against one exact source Snapshot: the author reads that Snapshot, the installed
//! renderer and checks bind every artifact to it, and acceptance requires an independent
//! verifier bound to the same Snapshot. Nothing here seals a tree or produces a candidate.
use super::document::{render_validated, safe_source_location};
use super::host::TaskDomain;
use super::source::{invocation_producer, source_snapshot};
use super::{TaskOperatorHost, TaskWorkOutput, envelope};
use review_core::PortCardinality;
use review_core::Producer;
use review_core::task::document::*;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::measurement::{MEASUREMENT_COMPARISON_V1, MEASUREMENT_V1};
use review_core::task::pipeline::*;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::report_task::*;
use review_core::task::{
    ArtifactInputV1, TaskAcceptanceV1, TaskExecutionV1, TaskResultV1, TaskRevisionV1,
};
use review_graph::task::{CompiledOperator, CompiledTask};
use review_graph::task::{OperatorAttemptCost, OperatorSignature};
use review_graph::{NodeOutcome, RunReport};
use review_source_git::task::SOURCE_TREE_V1;
use review_source_git::{EntryKind, Manifest};
use review_store::Cas;
use review_store::store::task::TaskProjection;
use review_store::store::task::execution::PreparedTaskAttempt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const REPORT_TASK_POLICY_SCHEMA: &str = "af.report-task-policy/1";

/// The report profile's captured policy: the Document policy's checks, plus whether a report
/// must cite at least one repository location. Its identity is the evidence policy every
/// report receipt names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportTaskPolicy {
    pub schema: String,
    pub max_document_bytes: u64,
    pub required_sections: BTreeSet<String>,
    pub require_citations: bool,
    pub require_repository_citations: bool,
    pub check_wall_ms: u64,
    pub require_container: bool,
}
impl ReportTaskPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != REPORT_TASK_POLICY_SCHEMA
            || !(1..=1048576).contains(&self.max_document_bytes)
            || self.required_sections.is_empty()
            || self.required_sections.len() > 32
            || self
                .required_sections
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 256 || s.chars().any(char::is_control))
            || !(1..=60000).contains(&self.check_wall_ms)
        {
            return Err(
                "Report policy requires bounded content, required sections and check time".into(),
            );
        }
        Ok(())
    }
    pub fn isolation(&self) -> review_sandbox::Policy {
        if self.require_container {
            review_sandbox::Policy::safe()
        } else {
            review_sandbox::Policy::trusted_local()
        }
    }
}

fn port(artifact_type: &str, optional: bool) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: artifact_type.into(),
        cardinality: PortCardinality::One,
        optional,
        affinity: PortAffinityV1::Unbound {},
        root_default: None,
        covers: BTreeSet::new(),
    }
}
/// A port whose artifacts carry the source Snapshot as their subject: the compiler proves the
/// lineage and the Store proves the Snapshot ID of every value that crosses it.
fn same_source(artifact_type: &str, optional: bool) -> PipelinePortV1 {
    let mut value = port(artifact_type, optional);
    value.affinity = PortAffinityV1::SameAs {
        input: "source".into(),
    };
    value
}
fn many(artifact_type: &str) -> PipelinePortV1 {
    let mut value = port(artifact_type, true);
    value.cardinality = PortCardinality::Many;
    value
}

/// Every root input a report Worker may declare, with its exact port. `requirements` and
/// `source` are required of every report Worker; the rest are the Worker's choice.
fn worker_inputs() -> BTreeMap<&'static str, PipelinePortV1> {
    BTreeMap::from([
        ("requirements", port("af/Requirements@1", false)),
        ("source", port(SOURCE_TREE_V1, false)),
        ("sources", port(REPORT_SOURCES_V1, true)),
        ("comparison", port(MEASUREMENT_COMPARISON_V1, true)),
        ("measurements", many(MEASUREMENT_V1)),
    ])
}

/// Whether `contract` is an admissible report author: it declares `requirements` and `source`,
/// any of the other root inputs with their exact ports, and one `draft` of either version bound
/// to the source Snapshot.
fn author_contract(contract: &PipelineContractV1) -> bool {
    let inputs = worker_inputs();
    ["requirements", "source"]
        .iter()
        .all(|name| contract.inputs.contains_key(*name))
        && contract
            .inputs
            .iter()
            .all(|(name, declared)| inputs.get(name.as_str()) == Some(declared))
        && contract.outputs.len() == 1
        && contract.outputs.get("draft").is_some_and(|draft| {
            *draft == same_source(DOCUMENT_DRAFT_V1, false)
                || *draft == same_source(DOCUMENT_DRAFT_V2, false)
        })
}

/// Whether `contract` is an admissible report verifier: it declares `requirements`,
/// `document`, `checks` and `source`, optionally `sources`, `comparison` and `measurements`, and
/// one evaluation bound to the source Snapshot.
fn verifier_contract(contract: &PipelineContractV1) -> bool {
    let mut inputs = worker_inputs();
    inputs.insert("document", same_source(DOCUMENT_V1, false));
    inputs.insert("checks", same_source(REPORT_CHECK_RECEIPT_V1, false));
    ["requirements", "document", "checks", "source"]
        .iter()
        .all(|name| contract.inputs.contains_key(*name))
        && contract
            .inputs
            .iter()
            .all(|(name, declared)| inputs.get(name.as_str()) == Some(declared))
        && contract.outputs
            == BTreeMap::from([("result".into(), same_source(REPORT_EVALUATION_V1, false))])
}

pub fn report_signatures(
    policy_id: &str,
    policy: &ReportTaskPolicy,
) -> Result<BTreeMap<String, OperatorSignature>, String> {
    policy.validate()?;
    if !review_core::is_digest(policy_id) {
        return Err("Report policy has no exact identity".into());
    }
    let signature = |inputs, outputs, evidence, retains, attempt, outcome_port| OperatorSignature {
        contract: PipelineContractV1 { inputs, outputs },
        effects: BTreeSet::new(),
        evidence,
        retains,
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        attempt,
        outcome_port,
    };
    let names = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    // Either draft version seals; the domain requires exactly one to be bound.
    let seal = signature(
        BTreeMap::from([
            ("draft".into(), same_source(DOCUMENT_DRAFT_V2, true)),
            ("draft_v1".into(), same_source(DOCUMENT_DRAFT_V1, true)),
            ("sources".into(), port(REPORT_SOURCES_V1, true)),
            ("source".into(), port(SOURCE_TREE_V1, false)),
        ]),
        BTreeMap::from([("document".into(), same_source(DOCUMENT_V1, false))]),
        BTreeMap::new(),
        BTreeMap::from([(
            "document".into(),
            names(&["draft", "draft_v1", "sources", "source"]),
        )]),
        None,
        None,
    );
    let check = signature(
        BTreeMap::from([
            ("document".into(), same_source(DOCUMENT_V1, false)),
            ("sources".into(), port(REPORT_SOURCES_V1, true)),
            ("source".into(), port(SOURCE_TREE_V1, false)),
        ]),
        BTreeMap::from([("result".into(), same_source(REPORT_CHECK_RECEIPT_V1, false))]),
        BTreeMap::from([("result".into(), BTreeSet::from([policy_id.into()]))]),
        BTreeMap::from([("result".into(), names(&["document", "sources", "source"]))]),
        Some(OperatorAttemptCost {
            tokens: 0,
            wall_ms: policy.check_wall_ms,
        }),
        Some("result".into()),
    );
    let accept = signature(
        BTreeMap::from([
            ("document".into(), same_source(DOCUMENT_V1, false)),
            ("checks".into(), same_source(REPORT_CHECK_RECEIPT_V1, false)),
            ("evaluation".into(), same_source(REPORT_EVALUATION_V1, true)),
            ("source".into(), port(SOURCE_TREE_V1, false)),
        ]),
        BTreeMap::from([
            ("document".into(), same_source(DOCUMENT_V1, false)),
            ("result".into(), same_source(REPORT_VERIFICATION_V1, false)),
        ]),
        BTreeMap::from([("result".into(), BTreeSet::from([policy_id.into()]))]),
        BTreeMap::from([
            (
                "result".into(),
                names(&["document", "checks", "evaluation", "source"]),
            ),
            ("document".into(), names(&["document"])),
        ]),
        None,
        Some("result".into()),
    );
    Ok(BTreeMap::from([
        ("operator/report-seal".into(), seal),
        ("operator/report-check".into(), check),
        ("operator/report-accept".into(), accept),
    ]))
}

/// A report draft of either admitted version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportDraft {
    V1(DocumentDraftV1),
    V2(DocumentDraftV2),
}
impl ReportDraft {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::V1(draft) => draft.validate(),
            Self::V2(draft) => draft.validate(),
        }
    }
    fn first_version(&self) -> DocumentDraftV1 {
        match self {
            Self::V1(draft) => draft.clone(),
            Self::V2(draft) => draft.as_first_version(),
        }
    }
    pub fn title(&self) -> &str {
        match self {
            Self::V1(draft) => &draft.title,
            Self::V2(draft) => &draft.title,
        }
    }
    fn repository_citations(&self) -> BTreeSet<RepositoryCitationV1> {
        match self {
            Self::V1(_) => BTreeSet::new(),
            Self::V2(draft) => draft.repository_citations.clone(),
        }
    }
}

/// A Markdown code span holding `text` verbatim: its fence is one backtick longer than the
/// longest run inside, so no cited spelling can close it or become markup.
fn code_span(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// The Document renderer's exact bytes for the draft, followed by one line per repository
/// citation, printed as `path` or `path:line`.
pub fn render_report(draft: &ReportDraft, sources: &ReportSourcesV1) -> Result<String, String> {
    draft.validate()?;
    sources.validate()?;
    let mut rendered = render_validated(&draft.first_version(), |name| sources.sources.get(name))?;
    let citations = draft.repository_citations();
    if !citations.is_empty() {
        rendered.push_str("\n## Repository citations\n");
    }
    for citation in &citations {
        rendered.push_str(&format!("\n- {}\n", code_span(&citation.display())));
    }
    Ok(rendered)
}

/// Counts lines and looks for a NUL byte in the first 8 KiB of a verified blob stream.
#[derive(Default)]
struct TextShape {
    bytes: u64,
    newlines: u64,
    last: Option<u8>,
    nul_in_head: bool,
}
impl std::io::Write for TextShape {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let head = 8192u64.saturating_sub(self.bytes).min(buffer.len() as u64) as usize;
        self.nul_in_head |= buffer[..head].contains(&0);
        self.newlines += buffer.iter().filter(|b| **b == b'\n').count() as u64;
        self.bytes += buffer.len() as u64;
        if let Some(last) = buffer.last() {
            self.last = Some(*last);
        }
        Ok(buffer.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl TextShape {
    fn lines(&self) -> u64 {
        self.newlines + u64::from(self.last.is_some_and(|last| last != b'\n'))
    }
}

/// Why `citation` does not name a text line of the exact Manifest, or `None` when it does. The
/// path is compared byte for byte with the Manifest's own `encode_path` spelling: there is no
/// second alphabet, normalization or case folding.
pub fn resolve_citation(
    cas: &Cas,
    manifest: &Manifest,
    citation: &RepositoryCitationV1,
) -> Result<Option<ReportCitationFailureReasonV1>, String> {
    use ReportCitationFailureReasonV1::*;
    let Some(entry) = manifest.get(&citation.path) else {
        let beneath = format!("{}/", citation.path);
        return Ok(Some(
            if manifest
                .entries
                .iter()
                .any(|entry| entry.path.starts_with(&beneath))
            {
                Directory
            } else {
                Absent
            },
        ));
    };
    if entry.kind == EntryKind::Symlink {
        return Ok(Some(Symlink));
    }
    let mut shape = TextShape::default();
    cas.copy_to_and_verify(&entry.content, &mut shape)
        .map_err(|e| format!("Cited source entry is unreadable: {e}"))?;
    Ok(if shape.nul_in_head {
        Some(Binary)
    } else if citation.line.is_some_and(|line| line > shape.lines()) {
        Some(LineOutOfRange)
    } else {
        None
    })
}

fn read<T: serde::de::DeserializeOwned>(
    cas: &Cas,
    id: &str,
    expected: &str,
    snapshot: Option<&str>,
) -> Result<T, String> {
    let value = envelope(cas, id)?;
    if value.artifact_type != expected || value.subject_snapshot_id.as_deref() != snapshot {
        return Err(match snapshot {
            Some(_) => format!("Expected one {expected} of the report's source Snapshot"),
            None => format!("Expected one data-only {expected} artifact"),
        });
    }
    serde_json::from_value(value.payload).map_err(|e| e.to_string())
}
fn read_draft(cas: &Cas, id: &str, snapshot: &str) -> Result<ReportDraft, String> {
    let value = envelope(cas, id)?;
    if value.subject_snapshot_id.as_deref() != Some(snapshot) {
        return Err("Report draft was written against another source Snapshot".into());
    }
    let draft = match value.artifact_type.as_str() {
        DOCUMENT_DRAFT_V1 => {
            ReportDraft::V1(serde_json::from_value(value.payload).map_err(|e| e.to_string())?)
        }
        DOCUMENT_DRAFT_V2 => {
            ReportDraft::V2(serde_json::from_value(value.payload).map_err(|e| e.to_string())?)
        }
        _ => return Err("Report draft is not a DocumentDraft".into()),
    };
    draft.validate()?;
    Ok(draft)
}
fn input_id<'a>(
    input: &'a TaskInvocationV1,
    port: &str,
    expected: &str,
    snapshot: Option<&str>,
) -> Result<&'a str, String> {
    let value = input
        .inputs
        .get(port)
        .ok_or_else(|| format!("Report operation lacks {port}"))?;
    value.validate()?;
    if value.artifact_type != expected
        || value.cardinality != PortCardinality::One
        || value.snapshot_id.as_deref() != snapshot
    {
        return Err(format!(
            "Report input {port} changed its contract or Snapshot"
        ));
    }
    Ok(&value.artifact_ids[0])
}
/// The one bound draft port of a seal invocation: exactly one of `draft` and `draft_v1`.
fn draft_input<'a>(input: &'a TaskInvocationV1, snapshot: &str) -> Result<&'a str, String> {
    match (
        input.inputs.contains_key("draft"),
        input.inputs.contains_key("draft_v1"),
    ) {
        (true, false) => input_id(input, "draft", DOCUMENT_DRAFT_V2, Some(snapshot)),
        (false, true) => input_id(input, "draft_v1", DOCUMENT_DRAFT_V1, Some(snapshot)),
        _ => Err("Report seal needs exactly one draft, as draft or draft_v1".into()),
    }
}
fn outcome(yes: bool) -> ReceiptOutcomeV1 {
    if yes {
        ReceiptOutcomeV1::Passed
    } else {
        ReceiptOutcomeV1::Failed
    }
}

pub struct ReportTaskDomain {
    policy_id: String,
    policy: ReportTaskPolicy,
    graph: CompiledTask,
}
impl ReportTaskDomain {
    pub fn captured(cas: &Cas, policy_id: &str, graph: CompiledTask) -> Result<Self, String> {
        let policy: ReportTaskPolicy =
            serde_json::from_value(cas.get_json(policy_id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let installed = report_signatures(policy_id, &policy)?;
        for node in graph.nodes.values() {
            let CompiledOperator::Primitive {
                operator,
                signature,
            } = &node.operator
            else {
                continue;
            };
            match operator {
                TaskOperatorV1::ReportSeal {}
                | TaskOperatorV1::ReportCheck {}
                | TaskOperatorV1::ReportAccept {} => {
                    if installed
                        .get(signature)
                        .is_none_or(|s| s.contract != node.contract)
                    {
                        return Err("Report operator differs from its installed contract".into());
                    }
                    if matches!(operator, TaskOperatorV1::ReportSeal {})
                        && node.inputs.contains_key("draft") == node.inputs.contains_key("draft_v1")
                    {
                        return Err("Report seal binds exactly one draft".into());
                    }
                }
                TaskOperatorV1::Worker { .. } => {
                    if !author_contract(&node.contract) {
                        return Err(
                            "Report author must read requirements and source and return one draft bound to the source Snapshot"
                                .into(),
                        );
                    }
                }
                TaskOperatorV1::Verify { .. } => {
                    if !verifier_contract(&node.contract) {
                        return Err("Report verifier changed its exact-input contract".into());
                    }
                }
                _ => {
                    return Err("Operator is unavailable in the captured report domain".into());
                }
            }
        }
        Ok(Self {
            policy_id: policy_id.into(),
            policy,
            graph,
        })
    }
    fn operator(&self, input: &TaskInvocationV1) -> Result<&TaskOperatorV1, String> {
        match &self
            .graph
            .nodes
            .get(&input.node)
            .ok_or("Unknown report node")?
            .operator
        {
            CompiledOperator::Primitive { operator, .. } => Ok(operator),
            _ => Err("Not a report operator".into()),
        }
    }
    fn is_node(&self, node: &str, operator: &TaskOperatorV1) -> bool {
        self.graph.nodes.get(node).is_some_and(|n| {
            matches!(&n.operator, CompiledOperator::Primitive { operator: actual, .. }
                if std::mem::discriminant(actual) == std::mem::discriminant(operator))
        })
    }
    /// The invocation's exact source Snapshot ID and Manifest.
    fn source(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<(String, String, Manifest), String> {
        let (id, snapshot, manifest) = source_snapshot(
            cas,
            input
                .inputs
                .get("source")
                .ok_or("Report operation lacks its source")?,
        )?;
        Ok((id, snapshot.manifest_id, manifest))
    }
    fn sealed(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<DocumentV1, String> {
        let (snapshot, _, _) = self.source(cas, input)?;
        let draft_id = draft_input(input, &snapshot)?;
        let draft = read_draft(cas, draft_id, &snapshot)?;
        // The `sources` port is optional: a Pipeline that binds nothing there seals against the
        // empty set, recorded once as the kernel's own artifact — the same identity in every
        // Task — so the document still names the exact sources it was rendered with.
        let (sources_id, sources) = if input.inputs.contains_key("sources") {
            let id = input_id(input, "sources", REPORT_SOURCES_V1, None)?.to_owned();
            let sources: ReportSourcesV1 = read(cas, &id, REPORT_SOURCES_V1, None)?;
            (id, sources)
        } else {
            let empty = ReportSourcesV1::empty();
            let id = cas
                .put_artifact(
                    REPORT_SOURCES_V1,
                    Producer::KernelOperation {
                        run_id: "report-seal-v1".into(),
                        node_id: None,
                        operation_id: "empty-sources@1".into(),
                    },
                    vec![],
                    None,
                    serde_json::to_value(&empty).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
                .0;
            (id, empty)
        };
        Ok(DocumentV1 {
            schema: "af.document/1".into(),
            draft_id: draft_id.into(),
            sources_id,
            format: DocumentFormatV1::Markdown,
            text: render_report(&draft, &sources)?,
        })
    }
    fn document(
        &self,
        cas: &Cas,
        id: &str,
        snapshot: &str,
    ) -> Result<(DocumentV1, ReportDraft, ReportSourcesV1), String> {
        let document: DocumentV1 = read(cas, id, DOCUMENT_V1, Some(snapshot))?;
        document.validate()?;
        let draft = read_draft(cas, &document.draft_id, snapshot)?;
        let sources: ReportSourcesV1 = read(cas, &document.sources_id, REPORT_SOURCES_V1, None)?;
        if document.text != render_report(&draft, &sources)? {
            return Err("Report differs from its exact rendered draft and sources".into());
        }
        Ok((document, draft, sources))
    }
    fn checks(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<ReportCheckReceiptV1, String> {
        let (snapshot, manifest_id, manifest) = self.source(cas, input)?;
        let document_id = input_id(input, "document", DOCUMENT_V1, Some(&snapshot))?;
        let (document, draft, sources) = self.document(cas, document_id, &snapshot)?;
        // With `sources` bound, the checks judge exactly the set the document was sealed with;
        // without, the document must have been sealed against the empty set.
        let sources_id = if input.inputs.contains_key("sources") {
            let id = input_id(input, "sources", REPORT_SOURCES_V1, None)?;
            if document.sources_id != id {
                return Err("Report checks bind different captured sources".into());
            }
            id.to_owned()
        } else {
            if !sources.sources.is_empty() {
                return Err(
                    "Report checks bind no sources but the report was sealed with some".into(),
                );
            }
            document.sources_id.clone()
        };
        let draft_first = draft.first_version();
        let cited = draft.repository_citations();
        let mut citation_failures = Vec::new();
        for citation in &cited {
            if let Some(reason) = resolve_citation(cas, &manifest, citation)? {
                citation_failures.push(ReportCitationFailureV1 {
                    citation: citation.clone(),
                    reason,
                });
            }
        }
        let checks =
            BTreeMap::from([
                (
                    "size".into(),
                    outcome(document.text.len() as u64 <= self.policy.max_document_bytes),
                ),
                (
                    "required_sections".into(),
                    outcome(self.policy.required_sections.iter().all(|required| {
                        draft_first.sections.iter().any(|s| &s.heading == required)
                    })),
                ),
                (
                    "citations".into(),
                    outcome(!self.policy.require_citations || !draft_first.citations.is_empty()),
                ),
                (
                    "source_locations".into(),
                    outcome(
                        sources
                            .sources
                            .values()
                            .all(|s| safe_source_location(&s.uri)),
                    ),
                ),
                (
                    "repository_citations".into(),
                    outcome(
                        citation_failures.is_empty()
                            && (!self.policy.require_repository_citations || !cited.is_empty()),
                    ),
                ),
            ]);
        let passed = checks.values().all(|r| *r == ReceiptOutcomeV1::Passed);
        Ok(ReportCheckReceiptV1 {
            plan_id: input.plan_id.clone(),
            document_id: document_id.into(),
            sources_id,
            policy_id: self.policy_id.clone(),
            source_snapshot_id: snapshot,
            manifest_id,
            checks,
            citation_failures,
            outcome: outcome(passed),
        })
    }
    /// The current check receipt an operation downstream of the checks binds. Its source
    /// Snapshot must be the one this invocation reads: a verifier or acceptance bound to
    /// another tree is refused here, before any Worker is dispatched.
    fn checked(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<ReportCheckReceiptV1, String> {
        let (snapshot, _, _) = self.source(cas, input)?;
        let id = input_id(input, "checks", REPORT_CHECK_RECEIPT_V1, Some(&snapshot))?;
        let artifact = envelope(cas, id)?;
        if artifact.artifact_type != REPORT_CHECK_RECEIPT_V1 {
            return Err("Report checks are not a report check receipt".into());
        }
        let receipt: ReportCheckReceiptV1 =
            serde_json::from_value(artifact.payload.clone()).map_err(|e| e.to_string())?;
        receipt.validate()?;
        if receipt.source_snapshot_id != snapshot
            || artifact.subject_snapshot_id.as_deref() != Some(snapshot.as_str())
        {
            return Err(format!(
                "Report check receipt judged source Snapshot {}, not this invocation's {snapshot}",
                receipt.source_snapshot_id
            ));
        }
        let document = input_id(input, "document", DOCUMENT_V1, Some(&snapshot))?;
        let Producer::Attempt { node_id, .. } = &artifact.producer else {
            return Err("Report checks have no started Attempt producer".into());
        };
        if !self.is_node(node_id, &TaskOperatorV1::ReportCheck {}) {
            return Err("Report check producer is not the installed check operation".into());
        }
        let check_input = TaskInvocationV1 {
            node: node_id.clone(),
            inputs: BTreeMap::from([
                ("document".into(), input.inputs["document"].clone()),
                ("source".into(), input.inputs["source"].clone()),
                (
                    "sources".into(),
                    ArtifactInputV1 {
                        artifact_ids: vec![receipt.sources_id.clone()],
                        artifact_type: REPORT_SOURCES_V1.into(),
                        cardinality: PortCardinality::One,
                        snapshot_id: None,
                    },
                ),
            ]),
            ..input.clone()
        };
        if receipt.document_id != document
            || receipt != self.checks(cas, &check_input)?
            || !artifact.input_artifacts.contains(&receipt.document_id)
            || !artifact.input_artifacts.contains(&receipt.sources_id)
            || !artifact.input_artifacts.contains(&receipt.manifest_id)
        {
            return Err("Report check receipt does not judge the current exact report".into());
        }
        Ok(receipt)
    }
    fn evaluation(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        id: &str,
    ) -> Result<ReportEvaluationV1, String> {
        let checks = self.checked(cas, input)?;
        let snapshot = &checks.source_snapshot_id;
        let evaluation: ReportEvaluationV1 = read(cas, id, REPORT_EVALUATION_V1, Some(snapshot))?;
        evaluation.validate()?;
        let check_id = input_id(input, "checks", REPORT_CHECK_RECEIPT_V1, Some(snapshot))?;
        let source_id = input_id(input, "source", SOURCE_TREE_V1, Some(snapshot))?;
        let artifact = envelope(cas, id)?;
        let Producer::Attempt { node_id, .. } = &artifact.producer else {
            return Err("Report evaluation has no verifier Attempt".into());
        };
        if evaluation.source_snapshot_id != *snapshot {
            return Err(format!(
                "Report evaluation names source Snapshot {}, not the Snapshot {snapshot} its checks judged",
                evaluation.source_snapshot_id
            ));
        }
        if !self.is_node(
            node_id,
            &TaskOperatorV1::Verify {
                slot: String::new(),
            },
        ) || checks.outcome != ReceiptOutcomeV1::Passed
            || evaluation.document_id != checks.document_id
            || evaluation.sources_id != checks.sources_id
            || evaluation.check_receipt_id != check_id
            || [
                &evaluation.document_id,
                &evaluation.requirements_id,
                &evaluation.check_receipt_id,
                &source_id.to_owned(),
            ]
            .iter()
            .any(|id| !artifact.input_artifacts.contains(id))
        {
            return Err("Report evaluation is stale or lacks exact verifier inputs".into());
        }
        Ok(evaluation)
    }
    fn verification(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<ReportVerificationV1, String> {
        let checks = self.checked(cas, input)?;
        let evaluation_id = input
            .inputs
            .get("evaluation")
            .map(|_| {
                input_id(
                    input,
                    "evaluation",
                    REPORT_EVALUATION_V1,
                    Some(&checks.source_snapshot_id),
                )
                .map(String::from)
            })
            .transpose()?;
        let outcome = if checks.outcome == ReceiptOutcomeV1::Failed {
            ReceiptOutcomeV1::Failed
        } else if let Some(id) = &evaluation_id {
            self.evaluation(cas, input, id)?.outcome
        } else {
            ReceiptOutcomeV1::Inconclusive
        };
        Ok(ReportVerificationV1 {
            invocation: input.clone(),
            document_id: checks.document_id,
            policy_id: self.policy_id.clone(),
            check_receipt_id: input_id(
                input,
                "checks",
                REPORT_CHECK_RECEIPT_V1,
                Some(&checks.source_snapshot_id),
            )?
            .into(),
            evaluation_id,
            source_snapshot_id: checks.source_snapshot_id,
            outcome,
        })
    }
    /// Admission of a verifier invocation: its checks passed, and they judged the document,
    /// sources and source Snapshot this verifier is about to read.
    pub fn admit_verifier(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<ReportCheckReceiptV1, String> {
        let checks = self.checked(cas, input)?;
        if checks.outcome != ReceiptOutcomeV1::Passed {
            return Err("Report verifier cannot run before its exact checks pass".into());
        }
        if input.inputs.contains_key("sources")
            && checks.sources_id != input_id(input, "sources", REPORT_SOURCES_V1, None)?
        {
            return Err("Report verifier reads other sources than its checks judged".into());
        }
        input_id(input, "requirements", "af/Requirements@1", None)?;
        Ok(checks)
    }

    fn assess(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        let mut missing = BTreeSet::new();
        let mut failed = false;
        let source = task
            .inputs
            .get("source")
            .and_then(|port| port.snapshot_id.clone())
            .ok_or("Report Task lost its source Snapshot")?;
        for (name, obligation) in &task.acceptance {
            let address = self
                .graph
                .coverage
                .get(name)
                .ok_or("Report Task lacks named coverage")?;
            let origins = self.graph.evidence_origins(address)?;
            let mut found = false;
            let mut passed = false;
            for id in &result.evidence {
                let artifact = envelope(cas, id)?;
                if !matches!(&artifact.producer, Producer::KernelOperation {node_id:Some(node),..}
                    if origins.iter().any(|a| &a.node == node))
                {
                    continue;
                }
                if found {
                    return Err("Ambiguous report acceptance evidence".into());
                }
                found = true;
                if artifact.artifact_type != REPORT_VERIFICATION_V1
                    || obligation.evidence_type != REPORT_VERIFICATION_V1
                {
                    return Err("Report Task has unsupported acceptance evidence".into());
                }
                let receipt: ReportVerificationV1 =
                    read(cas, id, REPORT_VERIFICATION_V1, Some(&source))?;
                receipt.validate()?;
                if receipt.policy_id != obligation.verifier_policy
                    || receipt.policy_id != self.policy_id
                    || receipt.source_snapshot_id != source
                    || self.operator(&receipt.invocation)? != &(TaskOperatorV1::ReportAccept {})
                    || artifact.producer != invocation_producer(cas, &receipt.invocation, None)?
                    || receipt != self.verification(cas, &receipt.invocation)?
                {
                    return Err(
                        "Report acceptance changed its exact invocation, policy or Snapshot".into(),
                    );
                }
                let report = result
                    .outputs
                    .get("report")
                    .ok_or("Report acceptance has no public report")?;
                let verification = result
                    .outputs
                    .get("verification")
                    .ok_or("Report acceptance has no public receipt")?;
                if report.artifact_type != DOCUMENT_V1
                    || report.artifact_ids != [receipt.document_id.clone()]
                    || verification.artifact_type != REPORT_VERIFICATION_V1
                    || verification.artifact_ids != [id.clone()]
                    || report.snapshot_id.as_ref() != Some(&source)
                    || verification.snapshot_id.as_ref() != Some(&source)
                {
                    return Err("Report evidence does not judge the actual public report".into());
                }
                let (document, _, _) = self.document(cas, &receipt.document_id, &source)?;
                let sources = task
                    .inputs
                    .get("sources")
                    .ok_or("Report Task lost its captured sources")?;
                let requirements = task
                    .inputs
                    .get("requirements")
                    .ok_or("Report Task lost its requirements")?;
                if sources.artifact_ids != [document.sources_id] {
                    return Err("Report acceptance used other Task sources".into());
                }
                if let Some(evaluation) = &receipt.evaluation_id {
                    let evaluation = self.evaluation(cas, &receipt.invocation, evaluation)?;
                    if requirements.artifact_ids != [evaluation.requirements_id] {
                        return Err("Report verifier judged another Task's requirements".into());
                    }
                }
                failed |= receipt.outcome == ReceiptOutcomeV1::Failed;
                passed |= receipt.outcome == ReceiptOutcomeV1::Passed;
            }
            if !passed {
                missing.insert(name.clone());
            }
        }
        result.acceptance = if missing.is_empty() {
            TaskAcceptanceV1::Satisfied
        } else if failed {
            TaskAcceptanceV1::Unsatisfied
        } else {
            TaskAcceptanceV1::Inconclusive
        };
        result.domain_conclusion = match result.acceptance {
            TaskAcceptanceV1::Satisfied => "verified",
            TaskAcceptanceV1::Unsatisfied => "changes_requested",
            TaskAcceptanceV1::Inconclusive => "incomplete",
        }
        .into();
        result.missing_obligations = missing;
        Ok(())
    }

    /// The kernel operation's context bytes. The trait entry point and the admission recheck
    /// must render identically, so both go through here. Report kernel operations use the
    /// Document profile's context shape: an exact invocation and its admitted feedback.
    fn render_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        self.operator(input)?;
        cas.put_artifact(
            "af/DocumentContext@1",
            invocation_producer(cas, input, None)?,
            input
                .inputs
                .values()
                .flat_map(|p| p.artifact_ids.iter().cloned())
                .chain(feedback.iter().cloned())
                .collect(),
            None,
            serde_json::json!({"invocation":input,"feedback_ids":feedback}),
        )
        .map(|(id, _)| id)
        .map_err(|e| e.to_string())
    }

    /// Store one kernel output bound to the source Snapshot. It retains every input and the
    /// Snapshot's Manifest, so the record names exactly the tree it was made from.
    fn put(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        ty: &str,
        value: &impl Serialize,
    ) -> Result<ArtifactInputV1, String> {
        let (snapshot, manifest_id, _) = self.source(cas, input)?;
        let mut refs: BTreeSet<String> = input
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.iter().cloned())
            .collect();
        refs.insert(manifest_id);
        let id = cas
            .put_artifact(
                ty,
                invocation_producer(cas, input, attempt)?,
                refs.into_iter().collect(),
                Some(snapshot.clone()),
                serde_json::to_value(value).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        Ok(ArtifactInputV1 {
            artifact_ids: vec![id],
            artifact_type: ty.into(),
            cardinality: PortCardinality::One,
            snapshot_id: Some(snapshot),
        })
    }
}

impl TaskOperatorHost for ReportTaskDomain {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        attempt: &review_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.render_context(cas, input, attempt.feedback_ids())
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = super::control::check(cancellation) {
            return super::control::refused(error);
        }
        let outputs = (|| match self.operator(input)? {
            TaskOperatorV1::ReportSeal {} => Ok(BTreeMap::from([(
                "document".into(),
                self.put(cas, input, None, DOCUMENT_V1, &self.sealed(cas, input)?)?,
            )])),
            TaskOperatorV1::ReportCheck {} => {
                let attempt = attempt.ok_or("Report checks need a durably started Attempt")?;
                Ok(BTreeMap::from([(
                    "result".into(),
                    self.put(
                        cas,
                        input,
                        Some(attempt),
                        REPORT_CHECK_RECEIPT_V1,
                        &self.checks(cas, input)?,
                    )?,
                )]))
            }
            TaskOperatorV1::ReportAccept {} => Ok(BTreeMap::from([
                ("document".into(), input.inputs["document"].clone()),
                (
                    "result".into(),
                    self.put(
                        cas,
                        input,
                        None,
                        REPORT_VERIFICATION_V1,
                        &self.verification(cas, input)?,
                    )?,
                ),
            ])),
            _ => Err("Report Worker requires its captured host".into()),
        })();
        TaskWorkOutput {
            usage_observation: None,
            usage: None,
            outputs,
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
        }
    }
}
impl TaskDomain for ReportTaskDomain {
    fn assemble_result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let execution = state
            .execution
            .as_ref()
            .ok_or("Report Task has no execution")?;
        let get = |address: &review_graph::task::Address| {
            execution
                .outputs
                .get(&address.node)
                .and_then(|(_, out)| out.outputs.get(&address.port))
        };
        let mut result = TaskResultV1 {
            task_revision_id: state.revision_id.clone(),
            execution: TaskExecutionV1::Completed,
            acceptance: TaskAcceptanceV1::Inconclusive,
            domain_conclusion: "incomplete".into(),
            outputs: self
                .graph
                .outputs
                .iter()
                .filter_map(|(name, address)| get(address).map(|p| (name.clone(), p.clone())))
                .collect(),
            evidence: self
                .graph
                .coverage
                .values()
                .filter_map(get)
                .flat_map(|p| p.artifact_ids.iter().cloned())
                .collect(),
            missing_obligations: BTreeSet::new(),
        };
        self.assess(cas, &state.revision, &mut result)?;
        if report
            .outcomes
            .iter()
            .any(|(_, outcome)| matches!(outcome, NodeOutcome::Failed { .. }))
        {
            result.execution = TaskExecutionV1::Exhausted;
        }
        result.validate()?;
        Ok(result)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &review_store::store::task::execution::ReservedTaskAttempt,
        context_id: &str,
    ) -> Result<(), String> {
        match self.operator(input)? {
            TaskOperatorV1::Worker { .. } => {
                self.source(cas, input)?;
                if input.inputs.contains_key("sources") {
                    let sources: ReportSourcesV1 = read(
                        cas,
                        input_id(input, "sources", REPORT_SOURCES_V1, None)?,
                        REPORT_SOURCES_V1,
                        None,
                    )?;
                    sources.validate()?;
                }
                input_id(input, "requirements", "af/Requirements@1", None)?;
            }
            TaskOperatorV1::Verify { .. } => {
                self.admit_verifier(cas, input)?;
            }
            _ => {
                if self.render_context(cas, input, attempt.feedback_ids())? != context_id {
                    return Err("Report context changed its invocation".into());
                }
            }
        }
        Ok(())
    }
    fn validate_output(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        _definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        let (snapshot, _, _) = self.source(cas, input)?;
        let output_id = |port: &str, ty: &str| -> Result<String, String> {
            let value = output
                .outputs
                .get(port)
                .ok_or("Report operation omitted an output")?;
            value.validate()?;
            if value.artifact_type != ty
                || value.cardinality != PortCardinality::One
                || value.snapshot_id.as_deref() != Some(snapshot.as_str())
            {
                return Err("Report output changed its contract or Snapshot".into());
            }
            Ok(value.artifact_ids[0].clone())
        };
        match self.operator(input)? {
            TaskOperatorV1::Worker { .. } => {
                let declared = output
                    .outputs
                    .get("draft")
                    .map(|port| port.artifact_type.clone())
                    .ok_or("Report author returned no draft")?;
                let draft = read_draft(cas, &output_id("draft", &declared)?, &snapshot)?;
                let sources = match input.inputs.get("sources") {
                    Some(_) => read(
                        cas,
                        input_id(input, "sources", REPORT_SOURCES_V1, None)?,
                        REPORT_SOURCES_V1,
                        None,
                    )?,
                    None => ReportSourcesV1::empty(),
                };
                render_report(&draft, &sources)?;
            }
            TaskOperatorV1::Verify { .. } => {
                let id = output_id("result", REPORT_EVALUATION_V1)?;
                let evaluation = self.evaluation(cas, input, &id)?;
                if evaluation.requirements_id
                    != input_id(input, "requirements", "af/Requirements@1", None)?
                {
                    return Err("Report verifier changed its requirements identity".into());
                }
            }
            TaskOperatorV1::ReportSeal {} => {
                let actual: DocumentV1 = read(
                    cas,
                    &output_id("document", DOCUMENT_V1)?,
                    DOCUMENT_V1,
                    Some(&snapshot),
                )?;
                if actual != self.sealed(cas, input)? {
                    return Err("Report renderer changed its captured inputs".into());
                }
            }
            TaskOperatorV1::ReportCheck {} => {
                let actual: ReportCheckReceiptV1 = read(
                    cas,
                    &output_id("result", REPORT_CHECK_RECEIPT_V1)?,
                    REPORT_CHECK_RECEIPT_V1,
                    Some(&snapshot),
                )?;
                if actual != self.checks(cas, input)? {
                    return Err("Report checks changed their captured result".into());
                }
            }
            TaskOperatorV1::ReportAccept {} => {
                let actual: ReportVerificationV1 = read(
                    cas,
                    &output_id("result", REPORT_VERIFICATION_V1)?,
                    REPORT_VERIFICATION_V1,
                    Some(&snapshot),
                )?;
                if actual != self.verification(cas, input)?
                    || output.outputs.get("document") != input.inputs.get("document")
                {
                    return Err("Report acceptance changed its current evidence or output".into());
                }
            }
            _ => return Err("Unsupported report output".into()),
        }
        Ok(())
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let mut expected = result.clone();
        self.assess(cas, task, &mut expected)?;
        if expected != *result {
            return Err("Report result changed its evidence-derived acceptance".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
