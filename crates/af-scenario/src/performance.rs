//! Standing publication authority is machine data, separate from generic signed publication.
use super::contract::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub repository: String,
    pub repository_id: u64,
    pub repo: PathBuf,
    pub task_state: PathBuf,
    pub base: String,
    pub branch_prefix: String,
    pub source_commit: String,
    pub source_content_digest: String,
    pub evidence_adapter: String,
    pub evidence_destination: String,
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        let Some((owner, repository)) = self.repository.split_once('/') else {
            return Err("Publication needs exact owner/repository".into());
        };
        require(
            !owner.is_empty()
                && owner.len() <= 39
                && owner
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric())
                && owner
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !repository.is_empty()
                && repository.len() <= 100
                && repository
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
                && repository
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "Invalid exact publication repository",
        )?;
        require(
            self.repository_id > 0
                && self.repository_id <= af_contracts::json::SAFE_INTEGER_MAX as u64
                && self.repo.is_absolute()
                && self.task_state.is_absolute()
                && !self.task_state.starts_with(&self.repo),
            "Invalid publication machine roots or numeric identity",
        )?;
        for branch in [&self.base, &self.branch_prefix] {
            require(
                !branch.is_empty()
                    && branch.len() <= 200
                    && branch
                        .bytes()
                        .next()
                        .is_some_and(|b| b.is_ascii_alphanumeric())
                    && !branch.contains("..")
                    && !branch.contains("//")
                    && !branch.ends_with(['/', '.'])
                    && branch
                        .split('/')
                        .all(|part| !part.starts_with('.') && !part.ends_with(".lock"))
                    && branch
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)),
                "Invalid publication branch scope",
            )?;
        }
        require(
            self.source_commit.len() == 40
                && self
                    .source_commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && af_contracts::is_digest(&self.source_content_digest),
            "Publication needs exact source authority",
        )?;
        name(&self.evidence_adapter)?;
        text(&self.evidence_destination)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofRequest {
    pub schema: String,
    pub candidate: TaskReceipt,
    pub verification: TaskReceipt,
    pub report: TaskReceipt,
    pub evidence: Observation,
    pub source_commit: String,
    pub source_content_digest: String,
}
impl ProofRequest {
    pub fn inputs(inputs: &BTreeMap<String, Datum>, scope: &Scope) -> Result<Self> {
        let task = |key: &str| match inputs.get(key) {
            Some(Datum::Task(t)) => Ok(t.clone()),
            _ => Err(format!("Publication needs native {key} receipt")),
        };
        let Some(Datum::Observation(evidence)) = inputs.get("measurement") else {
            return Err("Publication needs native measurement".into());
        };
        let request = Self {
            schema: "af/performance-pr-proof-request@1".into(),
            candidate: task("candidate")?,
            verification: task("verification")?,
            report: task("report")?,
            evidence: evidence.clone(),
            source_commit: scope.source_commit.clone(),
            source_content_digest: scope.source_content_digest.clone(),
        };
        require(
            inputs["subject"].as_text()? == request.candidate.result_id
                && request
                    .candidate
                    .result
                    .outputs
                    .get("snapshot")
                    .and_then(|p| p.snapshot_id.as_deref())
                    == Some(inputs["revision"].as_text()?),
            "Publication candidate selectors differ",
        )?;
        Ok(request)
    }
    pub fn digest(&self) -> Result<String> {
        hash(self)
    }
    pub fn validate(&self) -> Result<serde_json::Value> {
        require(
            self.schema == "af/performance-pr-proof-request@1",
            "Unsupported performance publication proof",
        )?;
        require(
            self.source_commit.len() == 40
                && self
                    .source_commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && af_contracts::is_digest(&self.source_content_digest),
            "Publication needs exact source identities",
        )?;
        for t in [&self.candidate, &self.verification, &self.report] {
            Datum::Task(t.clone()).validate()?;
        }
        require(
            self.candidate.task_id != self.verification.task_id
                && self.report.task_id != self.candidate.task_id
                && self.report.task_id != self.verification.task_id,
            "Publication needs independent Tasks",
        )?;
        self.evidence.validate()?;
        require(
            self.evidence.condition == "native_verified"
                && self.evidence.satisfied
                && af_contracts::is_digest(&self.evidence.subject),
            "Publication requires native verified comparison",
        )?;
        let completion: serde_json::Value =
            serde_json::from_str(&self.evidence.evidence).map_err(|e| e.to_string())?;
        let v = &completion["verdict"];
        require(
            completion["schema"] == "af.perf-scenario-completion/1"
                && completion["experiment"]["work"] == "core"
                && completion["request"]["work"] == "core"
                && completion["request"]["backend"] == "local"
                && completion["candidate_snapshot_id"] == self.evidence.revision
                && v["schema"] == "af.perf-verdict/2"
                && v["algorithm"] == "paired_median_tolerance_v2"
                && v["provenance"] == "measured"
                && v["aggregate"]["result"] == "pass"
                && v["aggregate"]["satisfies_required_check"] == false
                && matches!(v["scope"].as_str(), Some("partial_scope" | "bootstrap"))
                && v["aggregate"]["coverage"]["missing_records"] == 0,
            "Publication refuses unsupported, diagnostic or non-passing measurements",
        )?;
        let cases = v["cases"]
            .as_array()
            .ok_or("Native comparison cases absent")?;
        require(
            !cases.is_empty()
                && cases.iter().all(|c| {
                    c["outcome"] == "pass"
                        && matches!(
                            c["statistical_classification"].as_str(),
                            Some("improvement" | "within_tolerance" | "non_regression")
                        )
                })
                && cases.iter().any(|c| {
                    c["statistical_classification"] == "improvement"
                        && c["status"] == "improvement"
                        && c["exploratory"] == false
                        && !c["baseline"]["median"].is_null()
                        && !c["candidate"]["median"].is_null()
                }),
            "Publication requires a positive native measured improvement without inconclusive cases",
        )?;
        Ok(completion)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    pub schema: String,
    pub request_digest: String,
    pub source_snapshot: String,
    pub candidate_snapshot: String,
    pub body: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{schema, valid};
    use serde_json::json;

    fn request() -> ProofRequest {
        let receipt = |task: &str| {
            serde_json::from_value(json!({
            "task_id":task,"plan_id":digest(b"plan"),"revision_id":digest(b"revision"),
            "result_id":digest(task.as_bytes()),"charged_tokens":1,
            "result":{"task_revision_id":digest(b"revision"),"execution":"completed","acceptance":"satisfied",
                "domain_conclusion":"verified","outputs":{"snapshot":{"artifact_type":"af/SourceTree@1","cardinality":"one","artifact_ids":[digest(b"artifact")],"snapshot_id":digest(b"snapshot")}},"evidence":[digest(b"evidence")],"missing_obligations":[]}
        })).unwrap()
        };
        let mut completion: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/scenarios/performance-positive-completion.json"
        ))
        .unwrap();
        completion["candidate_snapshot_id"] = json!(digest(b"snapshot"));
        ProofRequest {
            schema: "af/performance-pr-proof-request@1".into(),
            candidate: receipt("implementation"),
            verification: receipt("verification"),
            report: receipt("report"),
            evidence: Observation {
                subject: digest(b"experiment"),
                revision: digest(b"snapshot"),
                condition: "native_verified".into(),
                satisfied: true,
                evidence: serde_json::to_string(&completion).unwrap(),
            },
            source_commit: "a".repeat(40),
            source_content_digest: digest(b"source"),
        }
    }
    #[test]
    fn native_performance_publication_requires_positive_complete_measured_core_evidence() {
        let request = request();
        request.validate().unwrap();
        valid("performance-pr-proof-request-v1.json", &request);
        for fault in [
            "telemetry",
            "synthetic",
            "regression",
            "inconclusive",
            "coverage",
            "no-improvement",
            "advisory-only",
            "source",
            "scope",
            "old-version",
        ] {
            let mut wrong = request.clone();
            let mut c: serde_json::Value = serde_json::from_str(&wrong.evidence.evidence).unwrap();
            match fault {
                "telemetry" => c["experiment"]["work"] = json!("telemetry"),
                "synthetic" => c["verdict"]["provenance"] = json!("synthetic"),
                "regression" => c["verdict"]["aggregate"]["result"] = json!("fail"),
                "inconclusive" => c["verdict"]["cases"][0]["outcome"] = json!("incomplete"),
                "coverage" => c["verdict"]["aggregate"]["coverage"]["missing_records"] = json!(1),
                "source" => c["candidate_snapshot_id"] = json!(digest(b"foreign")),
                "scope" => c["verdict"]["scope"] = json!("diagnostic"),
                "old-version" => c["schema"] = json!("af.perf-scenario-completion/0"),
                other => {
                    for case in c["verdict"]["cases"].as_array_mut().unwrap() {
                        if other == "no-improvement" {
                            case["statistical_classification"] = json!("within_tolerance");
                        } else {
                            case["exploratory"] = json!(true);
                        }
                    }
                }
            }
            wrong.evidence.evidence = serde_json::to_string(&c).unwrap();
            assert!(wrong.validate().is_err(), "accepted {fault}");
        }
        let mut duplicate = request.clone();
        duplicate.verification = duplicate.candidate.clone();
        assert!(duplicate.validate().is_err());
        let mut old = serde_json::to_value(&request).unwrap();
        old["schema"] = json!("af/performance-pr-proof-request@0");
        assert!(!schema("performance-pr-proof-request-v1.json").is_valid(&old));
        assert!(
            serde_json::from_value::<ProofRequest>(old)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut extra = serde_json::to_value(&request).unwrap();
        extra["trusted"] = json!(true);
        assert!(!schema("performance-pr-proof-request-v1.json").is_valid(&extra));
        assert!(serde_json::from_value::<ProofRequest>(extra).is_err());
    }
    #[test]
    fn inert_activation_and_workspace_examples_share_one_narrow_publication_scope() {
        let activation: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/scenarios/performance-publication.example.json"
        ))
        .unwrap();
        let workspace: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/scenarios/performance-workspace.example.json"
        ))
        .unwrap();
        valid("performance-publication-activation-v1.json", &activation);
        valid("git-workspace-config-v2.json", &workspace);
        let scope: Scope = serde_json::from_value(activation["scope"].clone()).unwrap();
        scope.validate().unwrap();
        for key in [
            "repository",
            "repository_id",
            "repo",
            "base",
            "branch_prefix",
            "source_commit",
            "source_content_digest",
        ] {
            assert_eq!(
                activation["scope"][key], workspace[key],
                "mismatched inert {key}"
            );
        }
        assert_eq!(activation["scope"]["task_state"], workspace["task_store"]);
        assert_eq!(
            workspace["operations"],
            json!(["publish_performance_pr_v1"])
        );
        assert_eq!(activation["adapter"]["operations"], workspace["operations"]);
        assert_eq!(
            workspace["performance"]["destination"],
            activation["scope"]["evidence_destination"]
        );
    }
    #[test]
    fn performance_operation_requires_its_own_machine_scope_and_keeps_generic_decisions() {
        let (mut bundle, mut policy) = crate::tests::simple();
        let scope = Scope {
            repository: "octo/project".into(),
            repository_id: 17,
            repo: "/project".into(),
            task_state: "/private/tasks".into(),
            base: "main".into(),
            branch_prefix: "af-performance".into(),
            source_commit: "a".repeat(40),
            source_content_digest: digest(b"source"),
            evidence_adapter: "lab".into(),
            evidence_destination: "native:claw".into(),
        };
        let adapter = |op, dest: &str| Adapter {
            executable: "/private/adapter".into(),
            digest: digest(b"adapter"),
            operations: [op].into(),
            destinations: [dest.into()].into(),
            max_wall_ms: 30000,
            max_output_bytes: 65536,
        };
        policy.adapters.insert(
            "publisher".into(),
            adapter(Operation::PublishPerformancePrV1, "github:octo/project"),
        );
        policy
            .adapters
            .insert("lab".into(), adapter(Operation::Observe, "native:claw"));
        assert!(policy.validate().is_err());
        policy
            .performance_publications
            .insert("publisher".into(), scope);
        policy.validate().unwrap();
        valid("scenario-policy-v5.json", &policy);
        policy
            .performance_publications
            .get_mut("publisher")
            .unwrap()
            .repository_id = 0;
        assert!(policy.validate().is_err());
        policy
            .performance_publications
            .get_mut("publisher")
            .unwrap()
            .repository_id = 17;
        policy
            .performance_publications
            .get_mut("publisher")
            .unwrap()
            .repository = "foreign/project".into();
        assert!(policy.validate().is_err());
        let definition = bundle.definitions.get_mut("main").unwrap();
        let grant = Grant {
            adapter: "publisher".into(),
            operation: Operation::PublishPr,
            destination: "github:octo/project".into(),
        };
        definition.authority.grants.insert(grant.clone());
        definition.steps[0].operation = Kind::Action {
            grant,
            requires_decision: false,
            required_condition: None,
        };
        assert!(
            bundle
                .validate(&crate::tests::TestHost::default())
                .unwrap_err()
                .contains("exact human decision")
        );
    }
}
