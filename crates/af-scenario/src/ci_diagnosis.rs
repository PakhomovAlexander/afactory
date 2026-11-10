//! Native diagnosis settlement. A refused Task remains refused; its deterministic unknown
//! observation is separate from accepted Task output and survives ordinary Task collection.
use crate::contract::*;
use af_contracts::ci_health::{Category, Diagnosis, Evidence, Selection};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "af/scenario-ci-diagnosis@2";
const RATIONALE: &str = "Deterministic unknown: native diagnosis Task output was refused";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    pub attempt_id: String,
    pub diagnostic_id: String,
    /// Bounded prefix; the immutable ID still names the entire original native diagnostic.
    pub error_prefix: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provenance {
    Accepted {
        output: Box<crate::task_output::Record>,
    },
    DeterministicRefusal {
        refusals: Vec<Refusal>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: String,
    pub receipt: TaskReceipt,
    pub snapshot_id: String,
    pub selection: Selection,
    pub diagnosis: Diagnosis,
    pub provenance: Provenance,
}
pub fn unknown(snapshot_id: &str, selection: &Selection) -> Diagnosis {
    Diagnosis {
        schema: af_contracts::ci_health::DIAGNOSIS_SCHEMA.into(),
        snapshot_id: snapshot_id.into(),
        source_artifact_ids: vec![snapshot_id.into()],
        occurrence_id: selection.occurrence_id.clone(),
        signature_id: selection.signature_id.clone(),
        source_sha: selection.source_sha.clone(),
        category: Category::Unknown,
        rationale: RATIONALE.into(),
        evidence: vec![Evidence {
            snapshot_id: snapshot_id.into(),
            source_sha: selection.source_sha.clone(),
            reference: "snapshot".into(),
        }],
    }
}
impl Record {
    pub fn validate(&self) -> Result<()> {
        text(&serde_json::to_string(self).map_err(|e| e.to_string())?)?;
        let r = &self.receipt;
        name(&r.task_id)?;
        require(
            self.schema == SCHEMA
                && [&r.plan_id, &r.revision_id, &r.result_id, &self.snapshot_id]
                    .iter()
                    .all(|s| af_contracts::is_digest(s))
                && r.result.task_revision_id == r.revision_id
                && self.selection.schema == "af.ci-diagnosis-selection/1"
                && self.diagnosis.snapshot_id == self.snapshot_id
                && self.diagnosis.occurrence_id == self.selection.occurrence_id
                && self.diagnosis.signature_id == self.selection.signature_id
                && self.diagnosis.source_sha == self.selection.source_sha,
            "CI diagnosis settlement identity differs",
        )?;
        r.result.validate()?;
        self.diagnosis.validate()?;
        match &self.provenance {
            Provenance::Accepted { output } => {
                output.validate(r)?;
                require(
                    output.artifact.payload
                        == serde_json::to_value(&self.diagnosis).map_err(|e| e.to_string())?,
                    "CI diagnosis differs from accepted output",
                )
            }
            Provenance::DeterministicRefusal { refusals } => {
                require(
                    r.result.acceptance != af_contracts::task::TaskAcceptanceV1::Satisfied
                        && !r.result.outputs.contains_key("validation")
                        && self.diagnosis == unknown(&self.snapshot_id, &self.selection)
                        && !refusals.is_empty()
                        && refusals.len() <= 20,
                    "Deterministic CI refusal cannot supply accepted diagnosis evidence",
                )?;
                let mut seen = std::collections::BTreeSet::new();
                for refusal in refusals {
                    require(
                        refusal.attempt_id.len() == 26
                            && refusal
                                .attempt_id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric())
                            && seen.insert(&refusal.attempt_id)
                            && af_contracts::is_digest(&refusal.diagnostic_id)
                            && !refusal.error_prefix.is_empty()
                            && refusal.error_prefix.chars().count() <= 2048
                            && !refusal.error_prefix.contains('\0'),
                        "CI refusal lacks exact native diagnostic evidence",
                    )?;
                }
                Ok(())
            }
        }
    }
    pub fn observation(&self, repository: &str) -> Result<Observation> {
        self.validate()?;
        Ok(Observation {
            subject: repository.into(),
            revision: self.selection.source_sha.clone(),
            condition: "ci_diagnosis_settled_v1".into(),
            satisfied: true,
            evidence: serde_json::to_string(self).map_err(|e| e.to_string())?,
        })
    }
    pub fn parse(observation: &Observation) -> Result<Self> {
        observation.validate()?;
        require(
            observation.satisfied && observation.condition == "ci_diagnosis_settled_v1",
            "Not a native CI diagnosis settlement",
        )?;
        let record: Self =
            serde_json::from_str(&observation.evidence).map_err(|e| e.to_string())?;
        record.validate()?;
        require(
            observation.revision == record.selection.source_sha,
            "CI diagnosis observation revision differs",
        )?;
        Ok(record)
    }
    pub fn target(&self, target: &TaskTarget) -> Result<()> {
        require(
            self.receipt.task_id == target.task_id
                && self.receipt.plan_id == target.plan_id
                && self.receipt.revision_id == target.revision_id,
            "CI diagnosis receipt belongs to another pinned invocation",
        )
    }
}

/// Adapter observations and workflow text cannot manufacture native settlement authority.
pub fn native(run: &crate::state::Run, observation: &Observation) -> Result<()> {
    for frame in run.frames.values() {
        let definition = &run.pin.bundle.definitions[&frame.scenario];
        for (progress, step) in frame.steps.iter().zip(&definition.steps) {
            if let Kind::CiDiagnosis { target } = &step.operation
                && progress.complete()
                && matches!(progress.outputs.get("result"), Some(Datum::Observation(o)) if o == observation)
            {
                return Record::parse(observation)?.target(target);
            }
        }
    }
    Err("Missing kernel-recorded CI diagnosis settlement".into())
}
