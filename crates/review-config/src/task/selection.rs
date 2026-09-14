//! Deterministic selection over captured facts and public contracts. Only semantic no-fit
//! can request generation; missing information, broken authority and unavailable resources
//! remain explicit refusals. This module never invokes a Planner or downloads a package.
use review_core::task::pipeline::PipelineDefinitionV1;
use review_core::task::{PipelineChoiceV1, PipelineFallbackV1, TaskRevisionV1};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoMatchPolicy {
    #[default]
    Refuse,
    Generate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateState {
    Fit {},
    Compatible {},
    NoFit { reasons: Vec<String> },
    UnknownFacts { facts: BTreeSet<String> },
    Unavailable { reason: String },
    Infeasible { reason: String },
    Invalid { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAssessment {
    pub pipeline: String,
    pub preferred: bool,
    pub priority: u32,
    pub state: CandidateState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SelectionDecision {
    Selected { pipeline: String },
    Ambiguous { pipelines: BTreeSet<String> },
    NeedsFacts { facts: BTreeSet<String> },
    Unavailable {},
    Infeasible {},
    Invalid {},
    Refused {},
    NeedsGeneration {},
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineSelection {
    pub schema: String,
    pub requested: Option<PipelineChoiceV1>,
    pub no_match: NoMatchPolicy,
    pub candidates: Vec<CandidateAssessment>,
    pub decision: SelectionDecision,
}

pub fn semantic_fit(task: &TaskRevisionV1, pipeline: &PipelineDefinitionV1) -> CandidateState {
    let mut reasons = Vec::new();
    let mut unknown = BTreeSet::new();
    if !pipeline.accepts.kinds.contains(&task.kind) {
        reasons.push("Task kind is outside the root Pipeline's declared applicability".into());
    }
    for (name, expected) in &pipeline.accepts.required_facts {
        match task.facts.get(name) {
            None => {
                unknown.insert(name.clone());
            }
            Some(actual) if actual != expected => {
                reasons.push(format!("Known fact {name} contradicts applicability"))
            }
            _ => (),
        }
    }
    for (name, input) in &task.inputs {
        if pipeline.contract.inputs.get(name).is_none_or(|p| {
            p.artifact_type != input.artifact_type || p.cardinality != input.cardinality
        }) {
            reasons.push(format!("Input {name} has no compatible public port"));
        }
    }
    for (name, port) in &pipeline.contract.inputs {
        if !port.optional && port.root_default.is_none() && !task.inputs.contains_key(name) {
            reasons.push(format!(
                "Required input {name} is absent and has no admitted constructor"
            ));
        }
    }
    for (name, output) in &task.required_outputs {
        if pipeline.contract.outputs.get(name).is_none_or(|p| {
            p.optional
                || p.artifact_type != output.artifact_type
                || p.cardinality != output.cardinality
        }) {
            reasons.push(format!(
                "Required output {name} has no compatible public guarantee"
            ));
        }
    }
    for (name, obligation) in &task.acceptance {
        if !pipeline.contract.outputs.values().any(|p| {
            !p.optional && p.covers.contains(name) && p.artifact_type == obligation.evidence_type
        }) {
            reasons.push(format!(
                "Acceptance obligation {name} has no compatible public coverage"
            ));
        }
    }
    if !reasons.is_empty() {
        CandidateState::NoFit { reasons }
    } else if !unknown.is_empty() {
        CandidateState::UnknownFacts { facts: unknown }
    } else {
        CandidateState::Fit {}
    }
}

/// Admission is supplied by the trusted host: exact structural compilation, captured
/// capability checks and resource feasibility. It cannot turn a semantic no-fit into a fit.
pub fn select(
    task: &TaskRevisionV1,
    requested: &PipelineChoiceV1,
    pipelines: &BTreeMap<String, PipelineDefinitionV1>,
    priorities: &BTreeMap<String, u32>,
    admit: impl FnMut(&str) -> CandidateState,
) -> Result<PipelineSelection, String> {
    select_with_policy(
        task,
        Some(requested),
        pipelines,
        priorities,
        NoMatchPolicy::Refuse,
        admit,
    )
}

pub fn select_with_policy(
    task: &TaskRevisionV1,
    requested: Option<&PipelineChoiceV1>,
    pipelines: &BTreeMap<String, PipelineDefinitionV1>,
    priorities: &BTreeMap<String, u32>,
    no_match: NoMatchPolicy,
    mut admit: impl FnMut(&str) -> CandidateState,
) -> Result<PipelineSelection, String> {
    task.validate()?;
    if requested.is_some_and(|choice| !review_core::task::is_package_name(&choice.name)) {
        return Err("Invalid requested Pipeline name".into());
    }
    if pipelines.len() > 128
        || priorities.len() > 128
        || priorities.keys().any(|name| !pipelines.contains_key(name))
    {
        return Err(
            "Selection requires bounded captured Pipelines and valid trusted priorities".into(),
        );
    }
    let fallback = requested
        .map(|choice| choice.fallback)
        .unwrap_or(match no_match {
            NoMatchPolicy::Refuse => PipelineFallbackV1::Select,
            NoMatchPolicy::Generate => PipelineFallbackV1::Generate,
        });
    let mut candidates = Vec::new();
    let mut names: BTreeSet<_> = if fallback == PipelineFallbackV1::Refuse {
        BTreeSet::new()
    } else {
        pipelines.keys().cloned().collect()
    };
    if let Some(choice) = requested {
        names.insert(choice.name.clone());
    }
    for name in names {
        let state = match pipelines.get(&name) {
            None => CandidateState::Unavailable {
                reason: "Requested Pipeline package is not captured".into(),
            },
            Some(pipeline) => {
                pipeline.validate()?;
                match semantic_fit(task, pipeline) {
                    CandidateState::Fit {} => CandidateState::Compatible {},
                    state => state,
                }
            }
        };
        candidates.push(CandidateAssessment {
            preferred: requested.is_some_and(|choice| name == choice.name),
            priority: priorities.get(&name).copied().unwrap_or(u32::MAX),
            pipeline: name,
            state,
        });
    }
    let mut groups: BTreeMap<(bool, u32), Vec<usize>> = BTreeMap::new();
    for (index, candidate) in candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.state, CandidateState::Compatible {}))
    {
        groups
            .entry((!candidate.preferred, candidate.priority))
            .or_default()
            .push(index);
    }
    for group in groups.values() {
        let mut found = false;
        for index in group {
            let candidate = &mut candidates[*index];
            candidate.state = match admit(&candidate.pipeline) {
                CandidateState::Fit {} => {
                    found = true;
                    CandidateState::Fit {}
                }
                state @ (CandidateState::Unavailable { .. }
                | CandidateState::Infeasible { .. }
                | CandidateState::Invalid { .. }) => state,
                _ => {
                    return Err("Capability admission cannot rewrite semantic applicability".into());
                }
            };
        }
        if found {
            break;
        }
    }
    let best = candidates
        .iter()
        .filter(|c| matches!(c.state, CandidateState::Fit {}))
        .map(|c| (!c.preferred, c.priority))
        .min();
    let decision = if let Some(best) = best {
        let winners: BTreeSet<_> = candidates
            .iter()
            .filter(|c| {
                matches!(c.state, CandidateState::Fit {}) && (!c.preferred, c.priority) == best
            })
            .map(|c| c.pipeline.clone())
            .collect();
        if winners.len() == 1 {
            SelectionDecision::Selected {
                pipeline: winners.into_iter().next().expect("one fitting Pipeline"),
            }
        } else {
            SelectionDecision::Ambiguous { pipelines: winners }
        }
    } else if candidates
        .iter()
        .any(|c| matches!(c.state, CandidateState::Invalid { .. }))
    {
        SelectionDecision::Invalid {}
    } else if candidates
        .iter()
        .any(|c| matches!(c.state, CandidateState::UnknownFacts { .. }))
    {
        SelectionDecision::NeedsFacts {
            facts: candidates
                .iter()
                .filter_map(|c| match &c.state {
                    CandidateState::UnknownFacts { facts } => Some(facts),
                    _ => None,
                })
                .flatten()
                .cloned()
                .collect(),
        }
    } else if candidates
        .iter()
        .any(|c| matches!(c.state, CandidateState::Unavailable { .. }))
    {
        SelectionDecision::Unavailable {}
    } else if candidates
        .iter()
        .any(|c| matches!(c.state, CandidateState::Infeasible { .. }))
    {
        SelectionDecision::Infeasible {}
    } else if fallback == PipelineFallbackV1::Generate
        && candidates
            .iter()
            .all(|c| matches!(c.state, CandidateState::NoFit { .. }))
    {
        SelectionDecision::NeedsGeneration {}
    } else {
        SelectionDecision::Refused {}
    };
    Ok(PipelineSelection {
        schema: "af.pipeline-selection/1".into(),
        requested: requested.cloned(),
        no_match,
        candidates,
        decision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixtures() -> (TaskRevisionV1, BTreeMap<String, PipelineDefinitionV1>) {
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/task-contracts/v1");
        let task = serde_json::from_slice(&std::fs::read(root.join("task-revision.json")).unwrap())
            .unwrap();
        let mut small: PipelineDefinitionV1 =
            serde_json::from_slice(&std::fs::read(root.join("pipeline-definition.json")).unwrap())
                .unwrap();
        small.name = "project/small".into();
        let mut heavy = small.clone();
        heavy.name = "project/heavy".into();
        heavy.accepts.required_facts.clear();
        (
            task,
            BTreeMap::from([(small.name.clone(), small), (heavy.name.clone(), heavy)]),
        )
    }
    fn request(fallback: PipelineFallbackV1) -> PipelineChoiceV1 {
        PipelineChoiceV1 {
            name: "project/small".into(),
            fallback,
        }
    }
    #[test]
    fn existing_heavy_pipeline_wins_before_generation_and_only_needed_capabilities_are_checked() {
        let (mut task, pipelines) = fixtures();
        task.facts.insert(
            "small".into(),
            serde_json::from_value(serde_json::json!(false)).unwrap(),
        );
        let mut checked = Vec::new();
        let result = select(
            &task,
            &request(PipelineFallbackV1::Generate),
            &pipelines,
            &BTreeMap::new(),
            |name| {
                checked.push(name.to_string());
                CandidateState::Fit {}
            },
        )
        .unwrap();
        assert_eq!(
            result.decision,
            SelectionDecision::Selected {
                pipeline: "project/heavy".into()
            }
        );
        assert_eq!(checked, ["project/heavy"]);
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.pipeline == "project/small"
                    && matches!(c.state, CandidateState::NoFit { .. }))
        );
        task.facts.insert(
            "small".into(),
            serde_json::from_value(serde_json::json!(true)).unwrap(),
        );
        checked.clear();
        let result = select(
            &task,
            &request(PipelineFallbackV1::Generate),
            &pipelines,
            &BTreeMap::new(),
            |name| {
                checked.push(name.to_string());
                CandidateState::Fit {}
            },
        )
        .unwrap();
        assert_eq!(
            result.decision,
            SelectionDecision::Selected {
                pipeline: "project/small".into()
            }
        );
        assert_eq!(
            checked,
            ["project/small"],
            "An explicit fitting Pipeline never probes unrelated Workers"
        );
    }
    #[test]
    fn unknown_facts_and_unresolved_ties_cannot_request_generation() {
        let (mut task, mut pipelines) = fixtures();
        pipelines.remove("project/heavy");
        task.facts.clear();
        let result = select(
            &task,
            &request(PipelineFallbackV1::Generate),
            &pipelines,
            &BTreeMap::new(),
            |_| panic!("Unknown facts do not admit capabilities"),
        )
        .unwrap();
        assert_eq!(
            result.decision,
            SelectionDecision::NeedsFacts {
                facts: BTreeSet::from(["small".into()])
            }
        );
        let (mut task, mut pipelines) = fixtures();
        task.facts.insert(
            "small".into(),
            serde_json::from_value(serde_json::json!(false)).unwrap(),
        );
        let mut other = pipelines["project/heavy"].clone();
        other.name = "project/other".into();
        pipelines.insert(other.name.clone(), other);
        let result = select(
            &task,
            &request(PipelineFallbackV1::Generate),
            &pipelines,
            &BTreeMap::new(),
            |_| CandidateState::Fit {},
        )
        .unwrap();
        assert_eq!(
            result.decision,
            SelectionDecision::Ambiguous {
                pipelines: BTreeSet::from(["project/heavy".into(), "project/other".into()])
            }
        );
        let mut checked = Vec::new();
        let result = select(
            &task,
            &request(PipelineFallbackV1::Select),
            &pipelines,
            &BTreeMap::from([("project/heavy".into(), 1), ("project/other".into(), 2)]),
            |name| {
                checked.push(name.to_string());
                CandidateState::Fit {}
            },
        )
        .unwrap();
        assert_eq!(
            result.decision,
            SelectionDecision::Selected {
                pipeline: "project/heavy".into()
            }
        );
        assert_eq!(checked, ["project/heavy"]);
    }
    #[test]
    fn capability_budget_and_invalid_definition_failures_are_distinct_from_semantic_no_fit() {
        let (task, mut pipelines) = fixtures();
        let result = select(
            &task,
            &request(PipelineFallbackV1::Generate),
            &pipelines,
            &BTreeMap::new(),
            |name| {
                if name == "project/small" {
                    CandidateState::Infeasible {
                        reason: "Protected verifier exceeds remaining allowance".into(),
                    }
                } else {
                    CandidateState::Fit {}
                }
            },
        )
        .unwrap();
        assert_eq!(
            result.decision,
            SelectionDecision::Selected {
                pipeline: "project/heavy".into()
            }
        );
        pipelines.remove("project/heavy");
        for (state, decision) in [
            (
                CandidateState::Infeasible {
                    reason: "Attempt allowance".into(),
                },
                SelectionDecision::Infeasible {},
            ),
            (
                CandidateState::Unavailable {
                    reason: "No admitted Provider".into(),
                },
                SelectionDecision::Unavailable {},
            ),
            (
                CandidateState::Invalid {
                    reason: "Invalid dependency contract".into(),
                },
                SelectionDecision::Invalid {},
            ),
        ] {
            let result = select(
                &task,
                &request(PipelineFallbackV1::Generate),
                &pipelines,
                &BTreeMap::new(),
                |_| state.clone(),
            )
            .unwrap();
            assert_eq!(result.decision, decision);
        }
    }
    #[test]
    fn only_permitted_semantic_no_fit_requests_generation() {
        let (mut task, mut pipelines) = fixtures();
        task.kind = "unknown/business-kind".into();
        for (fallback, expected) in [
            (PipelineFallbackV1::Refuse, SelectionDecision::Refused {}),
            (PipelineFallbackV1::Select, SelectionDecision::Refused {}),
            (
                PipelineFallbackV1::Generate,
                SelectionDecision::NeedsGeneration {},
            ),
        ] {
            let result = select(
                &task,
                &request(fallback),
                &pipelines,
                &BTreeMap::new(),
                |_| panic!("Semantic no-fit must be token free"),
            )
            .unwrap();
            assert_eq!(result.decision, expected);
        }
        pipelines.remove("project/small");
        let result = select(
            &task,
            &request(PipelineFallbackV1::Generate),
            &pipelines,
            &BTreeMap::new(),
            |_| panic!("Missing package is not an admitted Pipeline"),
        )
        .unwrap();
        assert_eq!(
            result.decision,
            SelectionDecision::Unavailable {},
            "A missing configured package cannot be silently regenerated"
        );
    }
}
