//! Exact plan and developer decision payloads. A decision payload is not authentication.

use super::{ArtifactInputV1, TaskAuthorityV1, TaskLimitsV1, is_name, is_package_name, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDependencyV1 {
    pub name: String,
    pub content_digest: String,
    pub artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedOriginV1 {
    pub pipeline_id: String,
    pub proposal_id: String,
    pub bootstrap_plan_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveWorkerBindingV1 {
    pub package_digest: String,
    pub package_artifact_id: String,
    pub execution: WorkerExecutionV1,
    /// Content identity of the effective isolation, environment and context policy.
    pub invocation_policy_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerExecutionV1 {
    Command {},
    Model {
        /// Machine-local binding label; it never proves Provider diversity.
        provider: String,
        /// Canonical Provider implementation family supplied by trusted admission.
        provider_kind: String,
        principal_id: String,
        /// Canonical resolved model identity, not a mutable selector alias.
        model: String,
        effort: String,
    },
}

impl EffectiveWorkerBindingV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.package_digest) && is_digest(&self.package_artifact_id),
            "Worker binding needs exact package identities",
        )?;
        require(
            is_digest(&self.invocation_policy_id),
            "Worker binding needs an exact invocation policy",
        )?;
        if let WorkerExecutionV1::Model {
            provider,
            provider_kind,
            principal_id,
            model,
            effort,
        } = &self.execution
        {
            require(
                is_name(provider) && is_name(provider_kind) && !principal_id.trim().is_empty(),
                "Worker binding needs an admitted Provider principal",
            )?;
            require(
                !model.trim().is_empty() && is_name(effort),
                "Worker binding needs explicit model and effort settings",
            )?;
        }
        Ok(())
    }
}

/// Loaded from exact trusted project policy, never from a local Worker override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndependencePolicyV1 {
    pub distinct_principals: bool,
    pub distinct_providers: bool,
    pub distinct_models: bool,
}

impl Default for IndependencePolicyV1 {
    fn default() -> Self {
        Self {
            distinct_principals: true,
            distinct_providers: false,
            distinct_models: false,
        }
    }
}

/// Static binding predicate. Fresh isolated sessions and role-scoped context are additional
/// mandatory runtime checks; matching this predicate never permits session reuse.
pub fn validate_independent_bindings(
    a: &EffectiveWorkerBindingV1,
    b: &EffectiveWorkerBindingV1,
    policy: IndependencePolicyV1,
) -> Result<(), String> {
    a.validate()?;
    b.validate()?;
    require(
        a.package_digest != b.package_digest,
        "Independent slots require distinct effective Worker packages",
    )?;
    match (&a.execution, &b.execution) {
        (
            WorkerExecutionV1::Model {
                provider_kind: ap,
                principal_id: ai,
                model: am,
                ..
            },
            WorkerExecutionV1::Model {
                provider_kind: bp,
                principal_id: bi,
                model: bm,
                ..
            },
        ) => {
            require(
                !policy.distinct_principals || ai != bi,
                "Provider aliases do not establish principal independence",
            )?;
            require(
                !policy.distinct_providers || ap != bp,
                "Trusted policy requires distinct Providers",
            )?;
            require(
                !policy.distinct_models || am != bm,
                "Trusted policy requires distinct models",
            )
        }
        _ => require(
            !policy.distinct_principals && !policy.distinct_providers && !policy.distinct_models,
            "Command Workers cannot establish model Provider diversity",
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlanV1 {
    pub task_revision_id: String,
    pub engine_id: String,
    pub pipeline_id: String,
    pub compiled_graph_id: String,
    pub authority: TaskAuthorityV1,
    pub limits: TaskLimitsV1,
    pub inputs: BTreeMap<String, ArtifactInputV1>,
    pub bindings: BTreeMap<String, EffectiveWorkerBindingV1>,
    pub dependencies: BTreeMap<String, PlanDependencyV1>,
    pub generated_origins: Vec<GeneratedOriginV1>,
    /// Every Task obligation names its compiled evidence producers (qualified node.port).
    #[serde(deserialize_with = "super::unique_set_map")]
    pub acceptance: BTreeMap<String, BTreeSet<String>>,
}

impl ExecutionPlanV1 {
    /// The compiler derives this closure from verified package provenance. A caller cannot
    /// make a plan trusted by deleting generated_origins: admission rechecks the closure.
    pub fn requires_developer_approval(&self) -> bool {
        !self.generated_origins.is_empty()
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.task_revision_id,
                &self.engine_id,
                &self.pipeline_id,
                &self.compiled_graph_id,
            ]
            .into_iter()
            .all(|id| is_digest(id)),
            "Execution Plan requires exact revision, engine, Pipeline and graph identities",
        )?;
        self.authority.validate()?;
        self.limits.validate()?;
        for (name, input) in &self.inputs {
            require(is_name(name), "Invalid plan input name")?;
            input.validate()?;
        }
        for (slot, binding) in &self.bindings {
            require(
                slot.split('.').all(is_name),
                "Invalid qualified Worker slot",
            )?;
            binding.validate()?;
        }
        for (key, dependency) in &self.dependencies {
            require(
                key == &dependency.name
                    && is_package_name(key)
                    && is_digest(&dependency.content_digest)
                    && is_digest(&dependency.artifact_id),
                "Invalid exact dependency entry",
            )?;
        }
        let mut generated = BTreeSet::new();
        let closure: BTreeSet<_> = self
            .dependencies
            .values()
            .map(|d| d.artifact_id.as_str())
            .collect();
        require(
            closure.contains(self.pipeline_id.as_str()),
            "Plan closure must include its root Pipeline",
        )?;
        for origin in &self.generated_origins {
            require(
                [
                    &origin.pipeline_id,
                    &origin.proposal_id,
                    &origin.bootstrap_plan_id,
                ]
                .into_iter()
                .all(|id| is_digest(id))
                    && generated.insert(&origin.pipeline_id),
                "Invalid or duplicate generated Pipeline origin",
            )?;
            require(
                closure.contains(origin.pipeline_id.as_str()),
                "Generated origin must occur in the dependency closure",
            )?;
        }
        require(
            !self.acceptance.is_empty()
                && self.acceptance.iter().all(|(name, producers)| {
                    is_name(name)
                        && !producers.is_empty()
                        && producers
                            .iter()
                            .all(|p| p.split('.').count() >= 2 && p.split('.').all(is_name))
                }),
            "Plan requires named acceptance producers",
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanDecisionKindV1 {
    Approved,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDecisionV1 {
    pub task_revision_id: String,
    /// Exact artifact-envelope identity of the persisted Execution Plan, not a selector.
    pub plan_id: String,
    pub policy_id: String,
    pub developer: String,
    pub authorization_id: String,
    pub decision: PlanDecisionKindV1,
    pub reason: String,
}

impl PlanDecisionV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            [
                &self.task_revision_id,
                &self.plan_id,
                &self.policy_id,
                &self.authorization_id,
            ]
            .into_iter()
            .all(|id| is_digest(id)),
            "Plan decision needs exact revision, plan, policy and authorization IDs",
        )?;
        require(
            !self.developer.trim().is_empty() && !self.reason.trim().is_empty(),
            "Plan decision needs developer identity and reason",
        )
    }

    /// Structural matching only. The trusted Store entry point must separately authenticate
    /// the developer, validate authorization, check revocation and enforce transition order.
    pub fn approves(&self, plan_id: &str, plan: &ExecutionPlanV1) -> bool {
        self.decision == PlanDecisionKindV1::Approved
            && self.plan_id == plan_id
            && self.task_revision_id == plan.task_revision_id
            && self.policy_id == plan.authority.policy_id
    }
}
