//! Exact preparation evidence for the installed implementation lifecycle adapter.
//! The native port remains an Action: its evidence has this independently versioned shape.
use super::contract::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Prepared {
    pub schema: String,
    pub owner: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub previous_owner: Option<String>,
    pub config_digest: String,
    pub repository: String,
    pub repository_id: u64,
    pub task_id: String,
    pub result_id: String,
    pub snapshot_id: String,
    pub source_snapshot: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub source_predecessor: Option<String>,
    pub source_commit: String,
    pub branch: String,
    pub base: String,
    pub commit: String,
    pub tree: String,
    pub worktree: std::path::PathBuf,
    pub title: String,
    pub body: String,
}
impl Prepared {
    pub fn parse(action: &ActionReceipt) -> Result<Self> {
        require(
            action.operation == Operation::ImplPrepareV1,
            "Implementation projection requires a preparation Action",
        )?;
        let prepared: Self = serde_json::from_str(&action.evidence).map_err(|e| e.to_string())?;
        prepared.validate()?;
        require(
            action.destination == format!("github:{}", prepared.repository)
                && prepared.owner == action.intent_id
                && prepared.result_id == action.subject
                && prepared.snapshot_id == action.revision,
            "Preparation differs from its accepted Task and reserved intent",
        )?;
        Ok(prepared)
    }
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-impl-prepared@1",
            "Unsupported implementation preparation",
        )?;
        for id in [
            &self.owner,
            &self.config_digest,
            &self.result_id,
            &self.snapshot_id,
            &self.source_snapshot,
        ] {
            require(
                review_core::is_digest(id),
                "Invalid implementation identity",
            )?;
        }
        require(
            self.previous_owner
                .as_ref()
                .is_none_or(|s| review_core::is_digest(s)),
            "Invalid repair predecessor",
        )?;
        require(
            self.source_predecessor
                .as_ref()
                .is_none_or(|s| review_core::is_digest(s))
                && self.source_predecessor.is_some() == self.previous_owner.is_some(),
            "Invalid source predecessor provenance",
        )?;
        name(&self.task_id)?;
        for sha in [&self.source_commit, &self.commit, &self.tree] {
            require(
                sha.len() == 40
                    && sha
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "Invalid implementation Git identity",
            )?;
        }
        for s in [
            &self.repository,
            &self.branch,
            &self.base,
            &self.title,
            &self.body,
        ] {
            text(s)?;
        }
        let wire = serde_json::to_string(self).map_err(|e| e.to_string())?;
        require(
            serde_json::to_string(&wire)
                .map_err(|e| e.to_string())?
                .len()
                <= 60000,
            "Preparation exceeds the native response bound",
        )?;
        require(
            self.repository_id > 0
                && self.repository_id <= review_core::json::SAFE_INTEGER_MAX as u64
                && self.worktree.is_absolute()
                && self.branch != self.base,
            "Invalid implementation destination",
        )
    }
    pub fn subject(&self) -> Result<String> {
        Ok(format!("impl:{}", hash(self)?))
    }
}

pub(super) fn validate_inputs(
    operation: Operation,
    inputs: &std::collections::BTreeMap<String, Datum>,
) -> Result<()> {
    if operation == Operation::ImplPrepareV1 {
        let Some(Datum::Task(candidate)) = inputs.get("candidate") else {
            return Err("Implementation preparation requires an accepted Task receipt".into());
        };
        let snapshot = candidate
            .result
            .outputs
            .get("snapshot")
            .and_then(|p| p.snapshot_id.as_deref())
            .ok_or("Implementation Task has no accepted candidate Snapshot")?;
        if let Some(Datum::Action(previous)) = inputs.get("previous") {
            let previous = Prepared::parse(previous)?;
            let Some(Datum::Observation(observed)) = inputs.get("repair") else {
                return Err("Repair preparation lacks native failure evidence".into());
            };
            let outcome = CheckOutcome::parse(observed)?;
            require(
                outcome.repair
                    && outcome.result_id == previous.result_id
                    && outcome.snapshot_id == previous.snapshot_id
                    && outcome.subject == previous.subject()?
                    && outcome.revision == previous.commit,
                "Repair preparation differs from exact previous publication",
            )?;
        }
        require(
            candidate.result_id == inputs["subject"].as_text()?
                && snapshot == inputs["revision"].as_text()?,
            "Preparation must bind the exact accepted candidate",
        )?;
    }
    if matches!(
        operation,
        Operation::ImplPublishV1 | Operation::ImplCleanupV1
    ) {
        let Some(Datum::Action(action)) = inputs.get("prepared") else {
            return Err("Implementation lifecycle requires native preparation provenance".into());
        };
        let prepared = Prepared::parse(action)?;
        let body: Prepared =
            serde_json::from_str(inputs["body"].as_text()?).map_err(|e| e.to_string())?;
        require(
            prepared == body
                && prepared.subject()? == inputs["subject"].as_text()?
                && prepared.commit == inputs["revision"].as_text()?,
            "Implementation action differs from exact preparation",
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceRequest {
    pub schema: String,
    pub receipt: TaskReceipt,
    pub source_commit: String,
    pub candidate_snapshot_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub predecessor_snapshot_id: Option<String>,
}
impl SourceRequest {
    pub fn digest(&self) -> Result<String> {
        hash(self)
    }
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-impl-source-request@1",
            "Unsupported implementation source request",
        )?;
        Datum::Task(self.receipt.clone()).validate()?;
        require(
            self.predecessor_snapshot_id
                .as_ref()
                .is_none_or(|s| review_core::is_digest(s))
                && review_core::is_digest(&self.candidate_snapshot_id)
                && self.source_commit.len() == 40
                && self
                    .source_commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Invalid implementation source identity",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceReceipt {
    pub schema: String,
    pub request_digest: String,
    pub repository_id: String,
    pub source_snapshot: String,
    pub candidate_snapshot: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub predecessor_snapshot: Option<String>,
    pub source_content_digest: String,
    pub source_commit: String,
    pub candidate_tree: String,
    pub transport_commit: String,
    pub repository: std::path::PathBuf,
}

impl SourceReceipt {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        serde_json::from_slice(&super::store::read(path, 65536)?).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckOutcome {
    pub schema: String,
    pub subject: String,
    pub revision: String,
    pub result_id: String,
    pub snapshot_id: String,
    pub ready: bool,
    pub repair: bool,
    pub reasons: Vec<String>,
    pub pr_id: u64,
    pub pr_number: u64,
}
impl CheckOutcome {
    pub fn parse(observation: &Observation) -> Result<Self> {
        let out: Self = serde_json::from_str(&observation.evidence).map_err(|e| e.to_string())?;
        require(
            out.schema == "af/scenario-impl-outcome@1"
                && observation.condition == "implementation_outcome"
                && observation.satisfied
                && out.subject == observation.subject
                && out.revision == observation.revision
                && out.ready != out.repair
                && out.reasons.len() <= 32
                && out.ready == out.reasons.is_empty()
                && out.pr_id <= review_core::json::SAFE_INTEGER_MAX as u64
                && out.pr_number <= review_core::json::SAFE_INTEGER_MAX as u64
                && out.revision.len() == 40
                && out
                    .revision
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && out
                    .subject
                    .strip_prefix("impl:")
                    .is_some_and(review_core::is_digest)
                && out.pr_id > 0
                && out.pr_number > 0
                && review_core::is_digest(&out.result_id)
                && review_core::is_digest(&out.snapshot_id),
            "Invalid exact-head implementation outcome",
        )?;
        for reason in &out.reasons {
            text(reason)?;
        }
        Ok(out)
    }
}

pub(super) fn validate_preparation(
    action: &ActionReceipt,
    inputs: &std::collections::BTreeMap<String, Datum>,
) -> Result<()> {
    let p = Prepared::parse(action)?;
    let Some(Datum::Task(candidate)) = inputs.get("candidate") else {
        return Err("Preparation lacks accepted Task provenance".into());
    };
    require(
        p.task_id == candidate.task_id,
        "Preparation names another Task",
    )?;
    let publication: Publication =
        serde_json::from_str(inputs["body"].as_text()?).map_err(|e| e.to_string())?;
    require(
        publication.schema == "af/scenario-impl-publication@1"
            && publication.title == p.title
            && publication.body == p.body,
        "Preparation changed pinned publication text",
    )?;
    if let Some(Datum::Action(previous)) = inputs.get("previous") {
        let previous = Prepared::parse(previous)?;
        require(
            p.previous_owner.as_ref() == Some(&previous.owner)
                && p.source_commit == previous.commit
                && p.source_predecessor.as_ref() == Some(&previous.snapshot_id)
                && p.branch == previous.branch
                && p.repository == previous.repository,
            "Preparation changed repair source lineage",
        )
    } else {
        require(
            p.previous_owner.is_none(),
            "Initial preparation has an undeclared predecessor",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Publication {
    schema: String,
    title: String,
    body: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{template::Template, tests::valid};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn prepared() -> (Prepared, ActionReceipt) {
        let p = Prepared {
            schema: "af/scenario-impl-prepared@1".into(),
            owner: digest(b"owner"),
            previous_owner: None,
            config_digest: digest(b"machine"),
            repository: "octo/project".into(),
            repository_id: 17,
            task_id: "accepted-implementation".into(),
            result_id: digest(b"result"),
            snapshot_id: digest(b"candidate"),
            source_snapshot: digest(b"source"),
            source_predecessor: None,
            source_commit: "a".repeat(40),
            branch: "impl/owned".into(),
            base: "main".into(),
            commit: "b".repeat(40),
            tree: "c".repeat(40),
            worktree: "/private/owned".into(),
            title: "Implement CLI".into(),
            body: "Exact body".into(),
        };
        let a = ActionReceipt {
            intent_id: p.owner.clone(),
            operation: Operation::ImplPrepareV1,
            destination: "github:octo/project".into(),
            subject: p.result_id.clone(),
            revision: p.snapshot_id.clone(),
            evidence: serde_json::to_string(&p).unwrap(),
        };
        (p, a)
    }
    #[test]
    fn implementation_preparation_schema_projections_bind_exact_task_commit_and_destination() {
        let (p, a) = prepared();
        valid("scenario-impl-prepared-v1.json", &p);
        assert_eq!(Prepared::parse(&a).unwrap(), p);
        assert_eq!(
            Projection::ImplSubject
                .apply(Datum::Action(a.clone()))
                .unwrap(),
            Datum::Text(p.subject().unwrap())
        );
        assert_eq!(
            Projection::ImplRevision
                .apply(Datum::Action(a.clone()))
                .unwrap(),
            Datum::Text(p.commit.clone())
        );
        let Datum::Text(body) = Projection::ImplBody
            .apply(Datum::Action(a.clone()))
            .unwrap()
        else {
            panic!("body is not text")
        };
        assert_eq!(serde_json::from_str::<Prepared>(&body).unwrap(), p);
        for field in ["intent", "result", "snapshot", "destination", "operation"] {
            let mut wrong = a.clone();
            match field {
                "intent" => wrong.intent_id = digest(b"other"),
                "result" => wrong.subject = digest(b"other"),
                "snapshot" => wrong.revision = digest(b"other"),
                "destination" => wrong.destination = "github:foreign/project".into(),
                _ => wrong.operation = Operation::PublishPr,
            }
            assert!(Prepared::parse(&wrong).is_err(), "accepted changed {field}");
        }
        let mut old = serde_json::to_value(&p).unwrap();
        old["schema"] = json!("af/scenario-impl-prepared@0");
        assert!(!crate::scenario::tests::schema("scenario-impl-prepared-v1.json").is_valid(&old));
        assert!(
            serde_json::from_value::<Prepared>(old)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    #[test]
    fn implementation_outcome_requires_exact_head_and_never_equates_failure_with_readiness() {
        let (p, _) = prepared();
        let out = CheckOutcome {
            schema: "af/scenario-impl-outcome@1".into(),
            subject: p.subject().unwrap(),
            revision: p.commit,
            result_id: p.result_id,
            snapshot_id: p.snapshot_id,
            ready: false,
            repair: true,
            reasons: vec!["CI failed".into()],
            pr_id: 23,
            pr_number: 5,
        };
        valid("scenario-impl-outcome-v1.json", &out);
        let mut observed = Observation {
            subject: out.subject.clone(),
            revision: out.revision.clone(),
            condition: "implementation_outcome".into(),
            satisfied: true,
            evidence: serde_json::to_string(&out).unwrap(),
        };
        assert!(CheckOutcome::parse(&observed).unwrap().repair);
        assert_eq!(
            Projection::ImplRepairNeeded
                .apply(Datum::Observation(observed.clone()))
                .unwrap(),
            Datum::Boolean(true)
        );
        observed.revision = "0".repeat(40);
        assert!(CheckOutcome::parse(&observed).is_err());
        observed.revision = out.revision.clone();
        let mut impossible = out;
        impossible.ready = true;
        observed.evidence = serde_json::to_string(&impossible).unwrap();
        assert!(CheckOutcome::parse(&observed).is_err());
    }
    #[test]
    fn real_impl_builder_bundle_has_native_schema_admission_signed_publication_and_bounded_repairs()
    {
        let directory = tempfile::tempdir().unwrap();
        let doc: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/scenarios/impl-task.json"
        ))
        .unwrap();
        let template = Template {
            schema: "af/scenario-task-template@1".into(),
            repo: "/repo".into(),
            state: "/tasks".into(),
            authority_commit: "a".repeat(40),
            repository_id: "fixture".into(),
            source_content_digest: digest(b"source"),
            authority_id: digest(b"authority"),
            max_tokens: doc["limits"]["tokens"].as_u64().unwrap(),
            document: doc,
            bindings: None,
        };
        template.validate().unwrap();
        let template_id = template.id().unwrap();
        let pin_path = directory.path().join("template-pin.json");
        std::fs::write(&pin_path, serde_json::to_vec(&json!({"schema":"af/scenario-template-pin@1","template_id":template_id,"template":template})).unwrap()).unwrap();
        let body_path = directory.path().join("body.txt");
        std::fs::write(&body_path, "Exact publication text").unwrap();
        let output_path = directory.path().join("bundle.json");
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let out = std::process::Command::new("/usr/bin/python3")
            .args(["-I"])
            .arg(root.join("scripts/scenario-impl.py"))
            .arg("bundle")
            .arg("--pin")
            .arg(&pin_path)
            .args([
                "--destination",
                "github:octo/project",
                "--title",
                "Implement CLI",
                "--body-file",
            ])
            .arg(&body_path)
            .arg("--output")
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bundle: Bundle = serde_json::from_slice(&std::fs::read(output_path).unwrap()).unwrap();
        bundle.validate().unwrap();
        valid("scenario-bundle-v4.json", &bundle);
        let (_, mut policy) = crate::scenario::tests::simple();
        policy.task_templates.insert(template_id);
        policy.adapters.insert(
            "impl".into(),
            Adapter {
                executable: "/installed/impl".into(),
                digest: digest(b"adapter"),
                operations: [
                    Operation::ImplPrepareV1,
                    Operation::ImplPublishV1,
                    Operation::ImplCleanupV1,
                    Operation::Observe,
                ]
                .into(),
                destinations: ["github:octo/project".into()].into(),
                max_wall_ms: 30000,
                max_output_bytes: 65536,
            },
        );
        let pin = Pin::new(bundle.clone(), policy).unwrap();
        valid("scenario-pin-v4.json", &pin);
        assert_eq!(
            bundle.definitions["impl"]
                .steps
                .iter()
                .filter(|s| matches!(s.operation, Kind::FreshTask { .. }))
                .count(),
            3
        );
        let mut no_signature = bundle.clone();
        let Kind::Action {
            requires_decision, ..
        } = &mut no_signature.definitions.get_mut("impl").unwrap().steps[3].operation
        else {
            panic!("publish not Action")
        };
        *requires_decision = false;
        assert!(
            no_signature
                .validate()
                .unwrap_err()
                .contains("exact human decision")
        );
        let mut no_merge = bundle;
        let steps = &mut no_merge.definitions.get_mut("impl").unwrap().steps;
        let Kind::Action {
            required_condition, ..
        } = &mut steps.last_mut().unwrap().operation
        else {
            panic!("cleanup not Action")
        };
        *required_condition = None;
        assert!(
            no_merge
                .validate()
                .unwrap_err()
                .contains("exact merged evidence")
        );
    }
    #[test]
    fn typed_selection_reads_only_the_selected_available_receipt() {
        let (bundle, policy) = crate::scenario::tests::simple();
        let run = crate::scenario::state::Run::new(
            "select".into(),
            Pin::new(bundle, policy).unwrap(),
            BTreeMap::new(),
            crate::scenario::tests::limits(),
            crate::scenario::tests::START,
            None,
        )
        .unwrap();
        let binding = Binding::Select {
            condition: Box::new(Binding::Literal {
                value: Box::new(Datum::Boolean(false)),
            }),
            passed: Box::new(Binding::Step {
                step: "not-run".into(),
                port: "result".into(),
            }),
            failed: Box::new(Binding::Literal {
                value: Box::new(Datum::Text("available".into())),
            }),
        };
        assert_eq!(
            run.resolve("root", &binding).unwrap(),
            Datum::Text("available".into())
        );
    }
}
