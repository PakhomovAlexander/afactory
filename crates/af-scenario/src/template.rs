//! Narrow fresh-Task authority: captured bytes and native Task planning, never a shell template.
use super::contract::*;
use super::state::Dispatch;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub schema: String,
    pub repo: PathBuf,
    pub state: PathBuf,
    pub authority_commit: String,
    pub repository_id: String,
    pub source_content_digest: String,
    pub authority_id: String,
    pub document: serde_json::Value,
    pub bindings: Option<serde_json::Value>,
    pub max_tokens: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predecessor {
    pub port: String,
    pub artifact_type: String,
}
impl Template {
    pub fn validate(&self, host: &dyn crate::Host) -> Result<()> {
        require(
            self.schema == "af/scenario-task-template@1",
            "Unsupported Task template",
        )?;
        require(
            self.repo.is_absolute()
                && self.state.is_absolute()
                && !self.state.starts_with(&self.repo),
            "Template requires an absolute repository and external Task Store",
        )?;
        require(
            [&self.source_content_digest, &self.authority_id]
                .iter()
                .all(|s| af_contracts::is_digest(s)),
            "Invalid template authority/source identity",
        )?;
        text(&self.repository_id)?;
        require(
            self.authority_commit.len() == 40
                && self.authority_commit.bytes().all(|b| b.is_ascii_hexdigit()),
            "Template authority must name an exact commit",
        )?;
        require(
            self.max_tokens > 0 && self.max_tokens <= af_contracts::json::SAFE_INTEGER_MAX as u64,
            "Invalid template limit",
        )?;
        host.validate_template(&self.document, self.bindings.as_ref(), self.max_tokens)
    }
    pub fn id(&self) -> Result<String> {
        hash(self)
    }
}
pub fn task_id(intent: &str) -> String {
    format!(
        "scenario-{}",
        intent.strip_prefix("sha256:").unwrap_or(intent)
    )
}
pub fn charge_key(d: &Dispatch) -> Result<String> {
    match &d.definition.operation {
        Kind::Task { target } => hash(&(&target.state, &target.task_id, &target.revision_id)),
        Kind::FreshTask { template, .. } => hash(&(&template.state, task_id(&d.intent.id))),
        _ => Err("Non-Task has no charge identity".into()),
    }
}
pub fn instantiate(
    host: &dyn crate::Host,
    d: &Dispatch,
    goal: Option<&super::roadmap::Goal>,
) -> Result<TaskTarget> {
    let Kind::FreshTask { template, bindings } = &d.definition.operation else {
        return Err("Not a fresh Task".into());
    };
    template.validate(host)?;
    let mut document = template.document.clone();
    document["task_id"] = task_id(&d.intent.id).into();
    let mut references = serde_json::Map::new();
    let mut predecessors = BTreeMap::new();
    for (input, binding) in bindings {
        let Some(Datum::Task(receipt)) = d.inputs.get(input) else {
            return Err("Fresh Task requires a predecessor Task receipt".into());
        };
        Datum::Task(receipt.clone()).validate()?;
        host.check_predecessor(
            template,
            receipt,
            input,
            &binding.port,
            &binding.artifact_type,
        )?;
        predecessors.insert(input.clone(), (receipt.clone(), binding.port.clone()));
        references.insert(
            input.clone(),
            serde_json::json!({"task":receipt.task_id,"port":binding.port}),
        );
    }
    if !references.is_empty() {
        document["inputs"] = references.into();
    }
    if let Some(goal) = goal {
        let map = document
            .as_object_mut()
            .ok_or("Template document must be an object")?;
        let requirements = map
            .entry("requirements")
            .or_insert_with(|| serde_json::json!({}));
        let requirements = requirements
            .as_object_mut()
            .ok_or("Template requirements must be an object")?;
        require(
            !requirements.contains_key("scenario_goal"),
            "Template cannot prefill runtime goal",
        )?;
        requirements.insert(
            "scenario_goal".into(),
            serde_json::to_value(goal).map_err(|e| e.to_string())?,
        );
    }
    if d.inputs.contains_key("publication") {
        require(
            matches!(d.inputs.get("scenario_evidence"), Some(Datum::Observation(o)) if o.condition == "checks_completed"),
            "Repair publication requires completed-check evidence",
        )?;
    }
    if let Some(Datum::Observation(observation)) = d.inputs.get("scenario_evidence") {
        observation.validate()?;
        require(
            observation.satisfied
                && matches!(
                    observation.condition.as_str(),
                    "bounded_evidence" | "native_verified" | "checks_completed"
                ),
            "Fresh Task requires satisfied bounded observation evidence",
        )?;
        if observation.condition == "checks_completed" {
            let outcome = super::checks::Outcome::parse(observation)?;
            require(
                outcome.state == "repairable",
                "Repair requires completed failing checks",
            )?;
            let Some(Datum::Action(publication)) = d.inputs.get("publication") else {
                return Err("Check repair requires exact prepared publication".into());
            };
            let prepared = super::workspace::Prepared::parse(publication)?;
            let (source, port) = predecessors
                .get("source")
                .ok_or("Check repair requires an accepted source predecessor")?;
            require(
                prepared.task_id == source.task_id
                    && prepared.result_id == source.result_id
                    && source
                        .result
                        .outputs
                        .get(port)
                        .and_then(|o| o.snapshot_id.as_ref())
                        == Some(&prepared.snapshot_id)
                    && observation.subject == prepared.subject()?
                    && observation.revision == prepared.commit,
                "Check repair differs from accepted published source",
            )?;
        }
        if observation.condition == "native_verified" {
            let (source, port) = predecessors
                .get("source")
                .ok_or("Native experiment evidence requires an accepted source predecessor")?;
            host.check_experiment_source(template, source, port, &observation.revision)?;
        }
        let requirements = document
            .as_object_mut()
            .ok_or("Template document must be an object")?
            .entry("requirements")
            .or_insert_with(|| serde_json::json!({}));
        let requirements = requirements
            .as_object_mut()
            .ok_or("Template requirements must be an object")?;
        require(
            !requirements.contains_key("scenario_evidence"),
            "Template cannot prefill runtime evidence",
        )?;
        requirements.insert(
            "scenario_evidence".into(),
            serde_json::to_value(observation).map_err(|e| e.to_string())?,
        );
    }
    host.plan_template(template, document, goal.is_some(), &predecessors)
}

pub fn targets(
    run: &super::state::Run,
    p: &super::state::Progress,
    step: &Step,
) -> Option<TaskTarget> {
    match &step.operation {
        Kind::Task { target } => Some(target.clone()),
        Kind::FreshTask { .. } => p
            .intents
            .last()
            .and_then(|i| run.fresh_tasks.get(&i.id))
            .cloned(),
        _ => None,
    }
}

pub fn validate_bindings(bindings: &BTreeMap<String, Predecessor>) -> Result<()> {
    require(bindings.len() <= 32, "Too many predecessor bindings")?;
    for (input, b) in bindings {
        name(input)?;
        name(&b.port)?;
        text(&b.artifact_type)?;
        require(
            !matches!(
                input.as_str(),
                "requirements" | "base" | "continuation" | "findings" | "scenario_evidence"
            ),
            "Reserved native input port",
        )?;
    }
    Ok(())
}
