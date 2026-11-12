//! Kernel-owned copies of admitted structured output. Receipt projections must keep replaying
//! after ordinary Task collection; only native Task settlement can write these records.
use crate::contract::*;
use crate::state::{Progress, Run};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "af/scenario-task-output-field@1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: String,
    pub task_id: String,
    pub plan_id: String,
    pub revision_id: String,
    pub result_id: String,
    pub port: String,
    pub artifact_id: String,
    #[serde(deserialize_with = "unbound_artifact")]
    pub artifact: af_contracts::ArtifactEnvelope,
}
fn unbound_artifact<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<af_contracts::ArtifactEnvelope, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    if value.get("subject_snapshot_id").is_some() {
        return Err(serde::de::Error::custom("Cached diagnosis must be unbound"));
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
impl Record {
    pub fn validate(&self, receipt: &TaskReceipt) -> Result<()> {
        require(
            self.schema == SCHEMA
                && self.task_id == receipt.task_id
                && self.plan_id == receipt.plan_id
                && self.revision_id == receipt.revision_id
                && self.result_id == receipt.result_id,
            "Cached Task output receipt identity differs",
        )?;
        name(&self.port)?;
        let output = receipt
            .result
            .outputs
            .get(&self.port)
            .ok_or("Cached Task output port absent")?;
        require(
            output.artifact_ids == [self.artifact_id.clone()],
            "Cached Task output artifact identity differs",
        )?;
        af_store::validate_envelope(&self.artifact)?;
        require(
            self.artifact_id == self.artifact.artifact_id
                && self.artifact.artifact_type == af_contracts::ci_health::DIAGNOSIS_TYPE
                && self.artifact.subject_snapshot_id.is_none(),
            "Cached Task envelope identity or affinity differs",
        )?;
        Projection::TaskOutputField {
            port: self.port.clone(),
            schema: af_contracts::ci_health::DIAGNOSIS_SCHEMA.into(),
            field: "category".into(),
        }
        .task_field(receipt, self.artifact.payload.clone())?;
        Ok(())
    }
}
fn target<'a>(run: &'a Run, receipt: &TaskReceipt) -> Result<&'a TaskTarget> {
    let mut targets = run
        .pin
        .bundle
        .definitions
        .values()
        .flat_map(|d| &d.steps)
        .filter_map(|s| match &s.operation {
            Kind::Task { target } => Some(target),
            _ => None,
        })
        .chain(run.fresh_tasks.values())
        .filter(|t| {
            t.task_id == receipt.task_id
                && t.plan_id == receipt.plan_id
                && t.revision_id == receipt.revision_id
        });
    let target = targets
        .next()
        .ok_or("Projected Task has no pinned native target")?;
    require(
        targets.all(|other| other == target),
        "Ambiguous native Task output origin",
    )?;
    Ok(target)
}
/// Called inside the same journal transaction as native Task completion. The Host validates
/// the current accepted native output and its captured snapshot before returning any payload.
pub fn capture(host: &dyn crate::Host, run: &Run, receipt: &TaskReceipt) -> Result<Vec<String>> {
    let mut records = Vec::new();
    for (port, output) in &receipt.result.outputs {
        if output.artifact_type != af_contracts::ci_health::DIAGNOSIS_TYPE {
            continue;
        }
        let artifact = host.task_output(target(run, receipt)?, receipt, port)?;
        let record = Record {
            schema: SCHEMA.into(),
            task_id: receipt.task_id.clone(),
            plan_id: receipt.plan_id.clone(),
            revision_id: receipt.revision_id.clone(),
            result_id: receipt.result_id.clone(),
            port: port.clone(),
            artifact_id: output
                .artifact_ids
                .first()
                .ok_or("Cached output has no artifact")?
                .clone(),
            artifact,
        };
        record.validate(receipt)?;
        let encoded = serde_json::to_string(&record).map_err(|e| e.to_string())?;
        text(&encoded)?;
        records.push(encoded);
    }
    Ok(records)
}
fn records(progress: &Progress) -> Result<Vec<Record>> {
    let mut records = Vec::new();
    for encoded in progress.intents.iter().flat_map(|i| &i.evidence) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(encoded) else {
            continue;
        };
        if value
            .get("schema")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("af/scenario-task-output-field@"))
        {
            let record: Record = serde_json::from_value(value)
                .map_err(|e| format!("Malformed cached Task output: {e}"))?;
            require(
                record.schema == SCHEMA,
                "Unsupported cached Task output version",
            )?;
            records.push(record);
        }
    }
    Ok(records)
}
pub fn validate(progress: &Progress, receipt: &TaskReceipt) -> Result<()> {
    let mut ports = std::collections::BTreeSet::new();
    for record in records(progress)? {
        record.validate(receipt)?;
        require(
            ports.insert(record.port),
            "Duplicate cached Task output port",
        )?;
    }
    let expected = receipt
        .result
        .outputs
        .iter()
        .filter(|(_, o)| o.artifact_type == af_contracts::ci_health::DIAGNOSIS_TYPE)
        .map(|(p, _)| p.clone())
        .collect();
    require(
        ports == expected,
        "Missing or extra cached Task output evidence",
    )
}
pub fn read(run: &Run, receipt: &TaskReceipt, port: &str) -> Result<serde_json::Value> {
    for frame in run.frames.values() {
        let def = &run.pin.bundle.definitions[&frame.scenario];
        for (progress, step) in frame.steps.iter().zip(&def.steps) {
            if !matches!(step.operation, Kind::Task { .. } | Kind::FreshTask { .. }) {
                continue;
            }
            if !progress
                .outputs
                .values()
                .any(|d| matches!(d,Datum::Task(t) if t==receipt))
            {
                continue;
            }
            validate(progress, receipt)?;
            if let Some(record) = records(progress)?.into_iter().find(|r| r.port == port) {
                return Ok(record.artifact.payload);
            }
        }
    }
    Err("Missing kernel-recorded Task output evidence".into())
}
