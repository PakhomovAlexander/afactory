//! Exact preparation evidence for the installed workspace lifecycle adapter.
//! The native port remains an Action: its evidence has this independently versioned shape.
use super::contract::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
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
            action.operation == Operation::PrepareCandidateV1,
            "Workspace projection requires a preparation Action",
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
            self.schema == "af/git-workspace-prepared@1",
            "Unsupported workspace preparation",
        )?;
        for id in [
            &self.owner,
            &self.config_digest,
            &self.result_id,
            &self.snapshot_id,
            &self.source_snapshot,
        ] {
            require(af_contracts::is_digest(id), "Invalid workspace identity")?;
        }
        require(
            self.previous_owner
                .as_ref()
                .is_none_or(|s| af_contracts::is_digest(s)),
            "Invalid repair predecessor",
        )?;
        require(
            self.source_predecessor
                .as_ref()
                .is_none_or(|s| af_contracts::is_digest(s))
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
                "Invalid workspace Git identity",
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
                && self.repository_id <= af_contracts::json::SAFE_INTEGER_MAX as u64
                && self.worktree.is_absolute()
                && self.branch != self.base,
            "Invalid workspace destination",
        )
    }
    pub fn publication(&self) -> serde_json::Value {
        serde_json::json!({"schema":"af/github-publish-pr@1", "head":self.branch,
            "base":self.base, "title":self.title, "body":self.body})
    }
    pub fn subject(&self) -> Result<String> {
        Ok(format!("github-pr:{}", hash(&self.publication())?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRequest {
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
            self.schema == "af/git-candidate-source-request@1",
            "Unsupported workspace source request",
        )?;
        Datum::Task(self.receipt.clone()).validate()?;
        require(
            self.predecessor_snapshot_id
                .as_ref()
                .is_none_or(|s| af_contracts::is_digest(s))
                && af_contracts::is_digest(&self.candidate_snapshot_id)
                && self.source_commit.len() == 40
                && self
                    .source_commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Invalid workspace source identity",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReceipt {
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

/// Bind the returned native Action to the Task receipt supplied to preparation.
pub fn validate_preparation(
    action: &ActionReceipt,
    inputs: &BTreeMap<String, Datum>,
) -> Result<()> {
    let p = Prepared::parse(action)?;
    let Some(Datum::Task(t)) = inputs.get("candidate") else {
        return Err("Preparation requires native Task provenance".into());
    };
    require(
        p.task_id == t.task_id
            && p.result_id == t.result_id
            && t.result
                .outputs
                .get("snapshot")
                .and_then(|v| v.snapshot_id.as_ref())
                == Some(&p.snapshot_id),
        "Preparation differs from its accepted candidate",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::valid;
    use serde_json::json;
    fn prepared() -> (Prepared, ActionReceipt) {
        let p = Prepared {
            schema: "af/git-workspace-prepared@1".into(),
            owner: digest(b"owner"),
            previous_owner: None,
            config_digest: digest(b"machine"),
            repository: "octo/project".into(),
            repository_id: 17,
            task_id: "accepted-refactor".into(),
            result_id: digest(b"result"),
            snapshot_id: digest(b"candidate"),
            source_snapshot: digest(b"source"),
            source_predecessor: None,
            source_commit: "a".repeat(40),
            branch: "work/owned".into(),
            base: "main".into(),
            commit: "b".repeat(40),
            tree: "c".repeat(40),
            worktree: "/private/owned".into(),
            title: "Implement CLI".into(),
            body: "Exact body".into(),
        };
        let a = ActionReceipt {
            intent_id: p.owner.clone(),
            operation: Operation::PrepareCandidateV1,
            destination: "github:octo/project".into(),
            subject: p.result_id.clone(),
            revision: p.snapshot_id.clone(),
            evidence: serde_json::to_string(&p).unwrap(),
        };
        (p, a)
    }
    #[test]
    fn workspace_preparation_schema_projections_bind_exact_task_commit_and_destination() {
        let (p, a) = prepared();
        valid("git-workspace-prepared-v1.json", &p);
        assert_eq!(Prepared::parse(&a).unwrap(), p);
        assert_eq!(
            Projection::WorkspaceSubject
                .apply(Datum::Action(a.clone()))
                .unwrap(),
            Datum::Text(p.subject().unwrap())
        );
        assert_eq!(
            Projection::WorkspaceRevision
                .apply(Datum::Action(a.clone()))
                .unwrap(),
            Datum::Text(p.commit.clone())
        );
        let Datum::Text(body) = Projection::WorkspaceReceipt
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
        old["schema"] = json!("af/git-workspace-prepared@0");
        assert!(!crate::tests::schema("git-workspace-prepared-v1.json").is_valid(&old));
        assert!(
            serde_json::from_value::<Prepared>(old)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    #[test]
    fn publication_projection_reuses_the_production_payload_and_subject() {
        let (p, a) = prepared();
        let Datum::Text(body) = Projection::WorkspacePublication
            .apply(Datum::Action(a))
            .unwrap()
        else {
            panic!()
        };
        let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
        valid("github-publish-pr-v1.json", &payload);
        assert_eq!(
            p.subject().unwrap(),
            format!("github-pr:{}", hash(&payload).unwrap())
        );
        assert_eq!(payload["head"], p.branch);
    }
}

#[cfg(test)]
mod composition_tests {
    use super::*;
    #[test]
    fn differently_named_scenario_composes_existing_publication_and_merge_waits() {
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let bundle: Bundle = serde_json::from_slice(
            &std::fs::read(root.join("fixtures/scenarios/workspace/bundle.json")).unwrap(),
        )
        .unwrap();
        crate::tests::valid("scenario-bundle-v6.json", &bundle);
        bundle.validate(&crate::tests::TestHost::default()).unwrap();
        assert_eq!(bundle.root, "documentation-update");
        let mut missing_merge = bundle.clone();
        let cleanup = missing_merge
            .definitions
            .get_mut("documentation-update")
            .unwrap()
            .steps
            .last_mut()
            .unwrap();
        let Kind::Action {
            required_condition, ..
        } = &mut cleanup.operation
        else {
            panic!()
        };
        *required_condition = None;
        assert!(
            missing_merge
                .validate(&crate::tests::TestHost::default())
                .unwrap_err()
                .contains("merge evidence")
        );
    }
}
