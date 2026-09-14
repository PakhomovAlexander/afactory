//! Document rendering and verification over the common Task scheduler and ledger.
use super::host::TaskDomain;
use super::source::invocation_producer;
use super::{TaskOperatorHost, TaskWorkOutput, envelope};
use review_core::PortCardinality;
use review_core::Producer;
use review_core::task::document::*;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::*;
use review_core::task::plan::ExecutionPlanV1;
use review_core::task::{
    ArtifactInputV1, TaskAcceptanceV1, TaskExecutionV1, TaskResultV1, TaskRevisionV1,
};
use review_graph::task::{CompiledOperator, CompiledTask};
use review_graph::task::{OperatorAttemptCost, OperatorSignature};
use review_graph::{NodeOutcome, RunReport};
use review_store::Cas;
use review_store::store::task::TaskProjection;
use review_store::store::task::execution::PreparedTaskAttempt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentTaskPolicy {
    pub schema: String,
    pub max_document_bytes: u64,
    pub required_sections: BTreeSet<String>,
    pub require_citations: bool,
    pub check_wall_ms: u64,
    pub require_container: bool,
}
impl DocumentTaskPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "af.document-task-policy/1"
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
                "Document policy requires bounded content, required sections and check time".into(),
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
fn same_document(ty: &str, optional: bool) -> PipelinePortV1 {
    let mut value = port(ty, optional);
    value.affinity = PortAffinityV1::SameAs {
        input: "document".into(),
    };
    value
}
pub fn document_signatures(
    policy_id: &str,
    policy: &DocumentTaskPolicy,
) -> Result<BTreeMap<String, OperatorSignature>, String> {
    policy.validate()?;
    if !review_core::is_digest(policy_id) {
        return Err("Document policy has no exact identity".into());
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
    let seal = signature(
        BTreeMap::from([
            ("draft".into(), port(DOCUMENT_DRAFT_V1, false)),
            ("sources".into(), port(DOCUMENT_SOURCES_V1, false)),
        ]),
        BTreeMap::from([("document".into(), port(DOCUMENT_V1, false))]),
        BTreeMap::new(),
        BTreeMap::from([(
            "document".into(),
            BTreeSet::from(["draft".into(), "sources".into()]),
        )]),
        None,
        None,
    );
    let check = signature(
        BTreeMap::from([
            ("document".into(), port(DOCUMENT_V1, false)),
            ("sources".into(), port(DOCUMENT_SOURCES_V1, false)),
        ]),
        BTreeMap::from([(
            "result".into(),
            same_document(DOCUMENT_CHECK_RECEIPT_V1, false),
        )]),
        BTreeMap::from([("result".into(), BTreeSet::from([policy_id.into()]))]),
        BTreeMap::from([(
            "result".into(),
            BTreeSet::from(["document".into(), "sources".into()]),
        )]),
        Some(OperatorAttemptCost {
            tokens: 0,
            wall_ms: policy.check_wall_ms,
        }),
        Some("result".into()),
    );
    let accept = signature(
        BTreeMap::from([
            ("document".into(), port(DOCUMENT_V1, false)),
            (
                "checks".into(),
                same_document(DOCUMENT_CHECK_RECEIPT_V1, false),
            ),
            (
                "evaluation".into(),
                same_document(DOCUMENT_EVALUATION_V1, true),
            ),
        ]),
        BTreeMap::from([
            ("document".into(), same_document(DOCUMENT_V1, false)),
            (
                "result".into(),
                same_document(DOCUMENT_VERIFICATION_V1, false),
            ),
        ]),
        BTreeMap::from([("result".into(), BTreeSet::from([policy_id.into()]))]),
        BTreeMap::from([
            (
                "result".into(),
                BTreeSet::from(["document".into(), "checks".into(), "evaluation".into()]),
            ),
            ("document".into(), BTreeSet::from(["document".into()])),
        ]),
        None,
        Some("result".into()),
    );
    Ok(BTreeMap::from([
        ("operator/document-seal".into(), seal),
        ("operator/document-check".into(), check),
        ("operator/document-accept".into(), accept),
    ]))
}

/// Escape authored plain text. Only this renderer emits Markdown link syntax, from captured
/// source locations that pass the fixed grammar below. Rendering never fetches a URL.
fn markdown_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            result.push('\\');
        }
        result.push(c);
    }
    result
}

pub fn safe_source_location(uri: &str) -> bool {
    if !uri.is_ascii()
        || uri
            .bytes()
            .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
    {
        return false;
    }
    if let Some(path) = uri.strip_prefix("repo:") {
        return review_config::task::shared::safe_relative_path(path)
            && !path
                .chars()
                .any(|c| matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']'));
    }
    let Some(rest) = uri.strip_prefix("https://") else {
        return false;
    };
    if rest.chars().any(|c| {
        matches!(
            c,
            '@' | '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']' | '\\'
        )
    }) {
        return false;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty()
        || host.len() > 253
        || !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return false;
    }
    let bytes = rest.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !bytes
                .get(i + 1..i + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
        {
            return false;
        }
    }
    true
}

pub fn render_document(
    draft: &DocumentDraftV1,
    sources: &DocumentSourcesV1,
) -> Result<String, String> {
    draft.validate()?;
    sources.validate()?;
    let mut rendered = format!("# {}\n", markdown_text(&draft.title));
    for section in &draft.sections {
        rendered.push_str(&format!(
            "\n## {}\n\n{}\n",
            markdown_text(&section.heading),
            markdown_text(&section.body)
        ));
    }
    if !draft.citations.is_empty() {
        rendered.push_str("\n## Sources\n");
    }
    for citation in &draft.citations {
        let source = sources
            .sources
            .get(citation)
            .ok_or("Document cites a source outside its captured input")?;
        if safe_source_location(&source.uri) && source.uri.starts_with("https://") {
            rendered.push_str(&format!(
                "\n- [{}](<{}>) — revision {}\n",
                markdown_text(&source.title),
                source.uri,
                markdown_text(&source.revision)
            ));
        } else {
            // Invalid/unsupported links stay inert text; checks decide whether they can pass.
            rendered.push_str(&format!(
                "\n- {} — {} — revision {}\n",
                markdown_text(&source.title),
                markdown_text(&source.uri),
                markdown_text(&source.revision)
            ));
        }
    }
    Ok(rendered)
}

fn read<T: serde::de::DeserializeOwned>(cas: &Cas, id: &str, expected: &str) -> Result<T, String> {
    let value = envelope(cas, id)?;
    if value.artifact_type != expected || value.subject_snapshot_id.is_some() {
        return Err(format!("Expected one data-only {expected} artifact"));
    }
    serde_json::from_value(value.payload).map_err(|e| e.to_string())
}
fn input_id<'a>(
    input: &'a TaskInvocationV1,
    port: &str,
    expected: &str,
) -> Result<&'a str, String> {
    let value = input
        .inputs
        .get(port)
        .ok_or_else(|| format!("Document operation lacks {port}"))?;
    value.validate()?;
    if value.artifact_type != expected
        || value.cardinality != PortCardinality::One
        || value.snapshot_id.is_some()
    {
        return Err(format!("Document input {port} changed its contract"));
    }
    Ok(&value.artifact_ids[0])
}
fn put(
    cas: &Cas,
    input: &TaskInvocationV1,
    attempt: Option<&PreparedTaskAttempt>,
    ty: &str,
    value: &impl Serialize,
) -> Result<ArtifactInputV1, String> {
    let id = cas
        .put_artifact(
            ty,
            invocation_producer(cas, input, attempt)?,
            input
                .inputs
                .values()
                .flat_map(|p| p.artifact_ids.iter().cloned())
                .collect(),
            None,
            serde_json::to_value(value).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    Ok(ArtifactInputV1 {
        artifact_ids: vec![id],
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        snapshot_id: None,
    })
}

pub struct DocumentTaskDomain {
    policy_id: String,
    policy: DocumentTaskPolicy,
    graph: CompiledTask,
}
impl DocumentTaskDomain {
    pub fn captured(cas: &Cas, policy_id: &str, graph: CompiledTask) -> Result<Self, String> {
        let policy: DocumentTaskPolicy =
            serde_json::from_value(cas.get_json(policy_id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let installed = document_signatures(policy_id, &policy)?;
        for node in graph.nodes.values() {
            if let CompiledOperator::Primitive {
                operator,
                signature,
            } = &node.operator
            {
                match operator {
                    TaskOperatorV1::DocumentSeal {}
                    | TaskOperatorV1::DocumentCheck {}
                    | TaskOperatorV1::DocumentAccept {} => {
                        if installed
                            .get(signature)
                            .is_none_or(|s| s.contract != node.contract)
                        {
                            return Err(
                                "Document operator differs from its installed contract".into()
                            );
                        }
                    }
                    TaskOperatorV1::Worker { .. } => {
                        let expected = PipelineContractV1 {
                            inputs: BTreeMap::from([
                                ("requirements".into(), port("af/Requirements@1", false)),
                                ("sources".into(), port(DOCUMENT_SOURCES_V1, false)),
                            ]),
                            outputs: BTreeMap::from([(
                                "draft".into(),
                                port(DOCUMENT_DRAFT_V1, false),
                            )]),
                        };
                        if node.contract != expected {
                            return Err(
                                "Document author changed its data-only draft contract".into()
                            );
                        }
                    }
                    TaskOperatorV1::Verify { .. } => {
                        let expected = PipelineContractV1 {
                            inputs: BTreeMap::from([
                                ("requirements".into(), port("af/Requirements@1", false)),
                                ("sources".into(), port(DOCUMENT_SOURCES_V1, false)),
                                ("document".into(), port(DOCUMENT_V1, false)),
                                (
                                    "checks".into(),
                                    same_document(DOCUMENT_CHECK_RECEIPT_V1, false),
                                ),
                            ]),
                            outputs: BTreeMap::from([(
                                "result".into(),
                                same_document(DOCUMENT_EVALUATION_V1, false),
                            )]),
                        };
                        if node.contract != expected {
                            return Err("Document verifier changed its exact-input contract".into());
                        }
                    }
                    _ => {
                        return Err(
                            "Operator is unavailable in the captured document domain".into()
                        );
                    }
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
            .ok_or("Unknown document node")?
            .operator
        {
            CompiledOperator::Primitive { operator, .. } => Ok(operator),
            _ => Err("Not a document operator".into()),
        }
    }
    fn sealed(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<DocumentV1, String> {
        let draft_id = input_id(input, "draft", DOCUMENT_DRAFT_V1)?;
        let sources_id = input_id(input, "sources", DOCUMENT_SOURCES_V1)?;
        let draft = read(cas, draft_id, DOCUMENT_DRAFT_V1)?;
        let sources = read(cas, sources_id, DOCUMENT_SOURCES_V1)?;
        Ok(DocumentV1 {
            schema: "af.document/1".into(),
            draft_id: draft_id.into(),
            sources_id: sources_id.into(),
            format: DocumentFormatV1::Markdown,
            text: render_document(&draft, &sources)?,
        })
    }
    fn document(
        &self,
        cas: &Cas,
        id: &str,
    ) -> Result<(DocumentV1, DocumentDraftV1, DocumentSourcesV1), String> {
        let document: DocumentV1 = read(cas, id, DOCUMENT_V1)?;
        document.validate()?;
        let draft = read(cas, &document.draft_id, DOCUMENT_DRAFT_V1)?;
        let sources = read(cas, &document.sources_id, DOCUMENT_SOURCES_V1)?;
        if document.text != render_document(&draft, &sources)? {
            return Err("Document differs from its exact rendered draft and sources".into());
        }
        Ok((document, draft, sources))
    }
    fn checks(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<DocumentCheckReceiptV1, String> {
        let document_id = input_id(input, "document", DOCUMENT_V1)?;
        let sources_id = input_id(input, "sources", DOCUMENT_SOURCES_V1)?;
        let (document, draft, sources) = self.document(cas, document_id)?;
        if document.sources_id != sources_id {
            return Err("Document checks bind different captured sources".into());
        }
        let pass = |yes| {
            if yes {
                ReceiptOutcomeV1::Passed
            } else {
                ReceiptOutcomeV1::Failed
            }
        };
        let checks = BTreeMap::from([
            (
                "size".into(),
                pass(document.text.len() as u64 <= self.policy.max_document_bytes),
            ),
            (
                "required_sections".into(),
                pass(
                    self.policy
                        .required_sections
                        .iter()
                        .all(|required| draft.sections.iter().any(|s| &s.heading == required)),
                ),
            ),
            (
                "citations".into(),
                pass(!self.policy.require_citations || !draft.citations.is_empty()),
            ),
            (
                "source_locations".into(),
                pass(
                    sources
                        .sources
                        .values()
                        .all(|s| safe_source_location(&s.uri)),
                ),
            ),
        ]);
        let outcome = pass(
            checks
                .values()
                .all(|result| *result == ReceiptOutcomeV1::Passed),
        );
        Ok(DocumentCheckReceiptV1 {
            plan_id: input.plan_id.clone(),
            document_id: document_id.into(),
            sources_id: sources_id.into(),
            policy_id: self.policy_id.clone(),
            checks,
            outcome,
        })
    }
    fn checked(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<DocumentCheckReceiptV1, String> {
        let id = input_id(input, "checks", DOCUMENT_CHECK_RECEIPT_V1)?;
        let receipt: DocumentCheckReceiptV1 = read(cas, id, DOCUMENT_CHECK_RECEIPT_V1)?;
        receipt.validate()?;
        let document = input_id(input, "document", DOCUMENT_V1)?;
        let artifact = envelope(cas, id)?;
        let Producer::Attempt { node_id, .. } = &artifact.producer else {
            return Err("Document checks have no started Attempt producer".into());
        };
        if !self.graph.nodes.get(node_id).is_some_and(|n| {
            matches!(
                n.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::DocumentCheck {},
                    ..
                }
            )
        }) {
            return Err("Document check producer is not the installed check operation".into());
        }
        let check_input = TaskInvocationV1 {
            node: node_id.clone(),
            inputs: BTreeMap::from([
                ("document".into(), input.inputs["document"].clone()),
                (
                    "sources".into(),
                    ArtifactInputV1 {
                        artifact_ids: vec![receipt.sources_id.clone()],
                        artifact_type: DOCUMENT_SOURCES_V1.into(),
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
        {
            return Err("Document check receipt does not judge the current exact document".into());
        }
        Ok(receipt)
    }
    fn evaluation(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        id: &str,
    ) -> Result<DocumentEvaluationV1, String> {
        let evaluation: DocumentEvaluationV1 = read(cas, id, DOCUMENT_EVALUATION_V1)?;
        evaluation.validate()?;
        let check_id = input_id(input, "checks", DOCUMENT_CHECK_RECEIPT_V1)?;
        let checks = self.checked(cas, input)?;
        let artifact = envelope(cas, id)?;
        let Producer::Attempt { node_id, .. } = &artifact.producer else {
            return Err("Document evaluation has no verifier Attempt".into());
        };
        if !self.graph.nodes.get(node_id).is_some_and(|n| {
            matches!(
                n.operator,
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::Verify { .. },
                    ..
                }
            )
        }) || checks.outcome != ReceiptOutcomeV1::Passed
            || evaluation.document_id != checks.document_id
            || evaluation.sources_id != checks.sources_id
            || evaluation.check_receipt_id != check_id
            || [
                &evaluation.document_id,
                &evaluation.sources_id,
                &evaluation.requirements_id,
                &evaluation.check_receipt_id,
            ]
            .iter()
            .any(|id| !artifact.input_artifacts.contains(id))
        {
            return Err("Document evaluation is stale or lacks exact verifier inputs".into());
        }
        Ok(evaluation)
    }
    fn verification(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
    ) -> Result<DocumentVerificationV1, String> {
        let checks = self.checked(cas, input)?;
        let evaluation_id = input
            .inputs
            .get("evaluation")
            .map(|_| input_id(input, "evaluation", DOCUMENT_EVALUATION_V1).map(String::from))
            .transpose()?;
        let outcome = if checks.outcome == ReceiptOutcomeV1::Failed {
            ReceiptOutcomeV1::Failed
        } else if let Some(id) = &evaluation_id {
            self.evaluation(cas, input, id)?.outcome
        } else {
            ReceiptOutcomeV1::Inconclusive
        };
        Ok(DocumentVerificationV1 {
            invocation: input.clone(),
            document_id: checks.document_id,
            policy_id: self.policy_id.clone(),
            check_receipt_id: input_id(input, "checks", DOCUMENT_CHECK_RECEIPT_V1)?.into(),
            evaluation_id,
            outcome,
        })
    }

    fn assess(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &mut TaskResultV1,
    ) -> Result<(), String> {
        let mut missing = BTreeSet::new();
        let mut failed = false;
        for (name, obligation) in &task.acceptance {
            let address = self
                .graph
                .coverage
                .get(name)
                .ok_or("Document Task lacks named coverage")?;
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
                    return Err("Ambiguous document acceptance evidence".into());
                }
                found = true;
                if artifact.artifact_type != DOCUMENT_VERIFICATION_V1
                    || obligation.evidence_type != DOCUMENT_VERIFICATION_V1
                {
                    return Err("Document Task has unsupported acceptance evidence".into());
                }
                let receipt: DocumentVerificationV1 = read(cas, id, DOCUMENT_VERIFICATION_V1)?;
                receipt.validate()?;
                if receipt.policy_id != obligation.verifier_policy
                    || receipt.policy_id != self.policy_id
                    || self.operator(&receipt.invocation)? != &(TaskOperatorV1::DocumentAccept {})
                    || artifact.producer != invocation_producer(cas, &receipt.invocation, None)?
                    || receipt != self.verification(cas, &receipt.invocation)?
                {
                    return Err("Document acceptance changed its exact invocation or policy".into());
                }
                let document = result
                    .outputs
                    .get("document")
                    .ok_or("Document acceptance has no public document")?;
                let verification = result
                    .outputs
                    .get("verification")
                    .ok_or("Document acceptance has no public receipt")?;
                if document.artifact_type != DOCUMENT_V1
                    || document.artifact_ids != [receipt.document_id.clone()]
                    || verification.artifact_type != DOCUMENT_VERIFICATION_V1
                    || verification.artifact_ids != [id.clone()]
                    || document.snapshot_id.is_some()
                    || verification.snapshot_id.is_some()
                {
                    return Err("Document evidence does not judge the actual public output".into());
                }
                let (document, _, _) = self.document(cas, &receipt.document_id)?;
                let sources = task
                    .inputs
                    .get("sources")
                    .ok_or("Document Task lost its captured sources")?;
                let requirements = task
                    .inputs
                    .get("requirements")
                    .ok_or("Document Task lost its requirements")?;
                if sources.artifact_ids != [document.sources_id] {
                    return Err("Document acceptance used other Task sources".into());
                }
                if let Some(evaluation) = &receipt.evaluation_id {
                    let evaluation = self.evaluation(cas, &receipt.invocation, evaluation)?;
                    if requirements.artifact_ids != [evaluation.requirements_id] {
                        return Err("Document evaluator judged another Task's requirements".into());
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
}

impl TaskOperatorHost for DocumentTaskDomain {
    fn execute_controlled(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        broker: Option<&dyn review_broker::ExactBrokerClient>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = super::control::check(cancellation) {
            return super::control::refused(error);
        }
        self.execute_with_broker(cas, input, attempt, broker)
    }

    fn prepare_context(
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
    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> TaskWorkOutput {
        let outputs = (|| match self.operator(input)? {
            TaskOperatorV1::DocumentSeal {} => Ok(BTreeMap::from([(
                "document".into(),
                put(cas, input, None, DOCUMENT_V1, &self.sealed(cas, input)?)?,
            )])),
            TaskOperatorV1::DocumentCheck {} => {
                let attempt = attempt.ok_or("Document checks need a durably started Attempt")?;
                Ok(BTreeMap::from([(
                    "result".into(),
                    put(
                        cas,
                        input,
                        Some(attempt),
                        DOCUMENT_CHECK_RECEIPT_V1,
                        &self.checks(cas, input)?,
                    )?,
                )]))
            }
            TaskOperatorV1::DocumentAccept {} => Ok(BTreeMap::from([
                ("document".into(), input.inputs["document"].clone()),
                (
                    "result".into(),
                    put(
                        cas,
                        input,
                        None,
                        DOCUMENT_VERIFICATION_V1,
                        &self.verification(cas, input)?,
                    )?,
                ),
            ])),
            _ => Err("Document Worker requires its captured host".into()),
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
impl TaskDomain for DocumentTaskDomain {
    fn assemble_result(
        &self,
        cas: &Cas,
        state: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let execution = state
            .execution
            .as_ref()
            .ok_or("Document Task has no execution")?;
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
        feedback: &[String],
        context_id: &str,
    ) -> Result<(), String> {
        match self.operator(input)? {
            TaskOperatorV1::Worker { .. } => {
                let sources: DocumentSourcesV1 = read(
                    cas,
                    input_id(input, "sources", DOCUMENT_SOURCES_V1)?,
                    DOCUMENT_SOURCES_V1,
                )?;
                sources.validate()?;
                input_id(input, "requirements", "af/Requirements@1")?;
            }
            TaskOperatorV1::Verify { .. } => {
                let checks = self.checked(cas, input)?;
                if checks.outcome != ReceiptOutcomeV1::Passed
                    || checks.sources_id != input_id(input, "sources", DOCUMENT_SOURCES_V1)?
                {
                    return Err("Document verifier cannot run before its exact checks pass".into());
                }
                input_id(input, "requirements", "af/Requirements@1")?;
            }
            _ => {
                if self.prepare_context(cas, input, feedback)? != context_id {
                    return Err("Document context changed its invocation".into());
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
    ) -> Result<(), String> {
        let output_id = |port: &str, ty: &str| -> Result<String, String> {
            let value = output
                .outputs
                .get(port)
                .ok_or("Document operation omitted an output")?;
            value.validate()?;
            if value.artifact_type != ty
                || value.cardinality != PortCardinality::One
                || value.snapshot_id.is_some()
            {
                return Err("Document output changed its contract".into());
            }
            Ok(value.artifact_ids[0].clone())
        };
        match self.operator(input)? {
            TaskOperatorV1::Worker { .. } => {
                let draft: DocumentDraftV1 = read(
                    cas,
                    &output_id("draft", DOCUMENT_DRAFT_V1)?,
                    DOCUMENT_DRAFT_V1,
                )?;
                let sources: DocumentSourcesV1 = read(
                    cas,
                    input_id(input, "sources", DOCUMENT_SOURCES_V1)?,
                    DOCUMENT_SOURCES_V1,
                )?;
                render_document(&draft, &sources)?;
            }
            TaskOperatorV1::Verify { .. } => {
                let id = output_id("result", DOCUMENT_EVALUATION_V1)?;
                let evaluation = self.evaluation(cas, input, &id)?;
                if evaluation.requirements_id
                    != input_id(input, "requirements", "af/Requirements@1")?
                {
                    return Err("Document verifier changed its requirements identity".into());
                }
            }
            TaskOperatorV1::DocumentSeal {} => {
                let actual: DocumentV1 =
                    read(cas, &output_id("document", DOCUMENT_V1)?, DOCUMENT_V1)?;
                if actual != self.sealed(cas, input)? {
                    return Err("Document renderer changed its captured inputs".into());
                }
            }
            TaskOperatorV1::DocumentCheck {} => {
                let actual: DocumentCheckReceiptV1 = read(
                    cas,
                    &output_id("result", DOCUMENT_CHECK_RECEIPT_V1)?,
                    DOCUMENT_CHECK_RECEIPT_V1,
                )?;
                if actual != self.checks(cas, input)? {
                    return Err("Document checks changed their captured result".into());
                }
            }
            TaskOperatorV1::DocumentAccept {} => {
                let actual: DocumentVerificationV1 = read(
                    cas,
                    &output_id("result", DOCUMENT_VERIFICATION_V1)?,
                    DOCUMENT_VERIFICATION_V1,
                )?;
                if actual != self.verification(cas, input)?
                    || output.outputs.get("document") != input.inputs.get("document")
                {
                    return Err("Document acceptance changed its current evidence or output".into());
                }
            }
            _ => return Err("Unsupported document output".into()),
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
            return Err("Document result changed its evidence-derived acceptance".into());
        }
        Ok(())
    }
}
