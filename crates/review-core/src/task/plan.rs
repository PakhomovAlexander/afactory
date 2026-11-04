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
pub struct EffectiveWorkerBindingV3 {
    pub package_digest: String,
    pub package_artifact_id: String,
    pub execution: WorkerExecutionV3,
    /// Content identity of the effective isolation, environment and context policy.
    pub invocation_policy_id: String,
}

/// Captured Pi SDK authority. All fields are non-secret; account identity is the Model
/// binding's opaque principal. Native model endpoints/configuration are pinned by digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiIdentityModeV1 {
    Account,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiAuthorityV2 {
    pub identity_mode: PiIdentityModeV1,
    pub backend: String,
    pub auth_method: String,
    pub sdk_version: String,
    pub sdk_entrypoint: String,
    pub node_program: String,
    pub auth_context_id: String,
    pub model_config_id: String,
    pub adapter_policy_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerExecutionV3 {
    Command {},
    Model {
        /// Machine-local binding label; it never proves Provider diversity.
        provider: String,
        /// Canonical Provider implementation family supplied by trusted admission.
        provider_kind: String,
        #[serde(deserialize_with = "Option::deserialize")]
        principal_id: Option<String>,
        /// Canonical resolved model identity, not a mutable selector alias.
        model: String,
        effort: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "super::present_option"
        )]
        pi: Option<Box<PiAuthorityV2>>,
    },
}

impl EffectiveWorkerBindingV3 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.package_digest) && is_digest(&self.package_artifact_id),
            "Worker binding needs exact package identities",
        )?;
        require(
            is_digest(&self.invocation_policy_id),
            "Worker binding needs an exact invocation policy",
        )?;
        self.execution.validate()
    }
}

impl WorkerExecutionV3 {
    pub fn validate(&self) -> Result<(), String> {
        if let WorkerExecutionV3::Model {
            provider,
            provider_kind,
            principal_id,
            model,
            effort,
            pi,
        } = self
        {
            require(
                is_name(provider) && is_name(provider_kind),
                "Worker binding needs an admitted Provider principal",
            )?;
            if provider_kind == "pi" {
                let identity = pi
                    .as_ref()
                    .ok_or("Pi binding requires captured SDK authority")?;
                require(
                    match identity.identity_mode {
                        PiIdentityModeV1::Account => {
                            identity.backend == "openai-codex"
                                && principal_id.as_deref().is_some_and(is_digest)
                        }
                        PiIdentityModeV1::Unavailable => {
                            identity.backend == "kimi-coding"
                                && principal_id.is_none()
                                && model == "k3"
                                && matches!(effort.as_str(), "low" | "high" | "max")
                        }
                    },
                    "Pi binding requires an honest supported identity mode and exact model settings",
                )?;
                require(
                    matches!(identity.backend.as_str(), "openai-codex" | "kimi-coding")
                        && identity.auth_method == "oauth"
                        && identity.sdk_version == "1.4.0"
                        && std::path::Path::new(&identity.node_program).is_absolute()
                        && std::path::Path::new(&identity.sdk_entrypoint).is_absolute()
                        && [
                            &identity.auth_context_id,
                            &identity.model_config_id,
                            &identity.adapter_policy_id,
                        ]
                        .into_iter()
                        .all(|id| is_digest(id)),
                    "Pi binding requires exact supported SDK, backend, auth and configuration identities",
                )?;
            } else {
                require(pi.is_none(), "Only a Pi binding carries Pi SDK authority")?;
                require(
                    principal_id
                        .as_ref()
                        .is_some_and(|id| !id.trim().is_empty()),
                    "Worker binding needs an admitted Provider principal",
                )?;
            }
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
    /// Deterministic Command slots can establish independence by distinct packages. This
    /// never relaxes the principal rule between two Model slots or explicit model diversity.
    pub command_workers_by_package: bool,
    pub distinct_principals: bool,
    pub distinct_providers: bool,
    pub distinct_models: bool,
}

impl Default for IndependencePolicyV1 {
    fn default() -> Self {
        Self {
            command_workers_by_package: true,
            distinct_principals: true,
            distinct_providers: false,
            distinct_models: false,
        }
    }
}

/// Static binding predicate. Fresh isolated sessions and role-scoped context are additional
/// mandatory runtime checks; matching this predicate never permits session reuse.
pub fn validate_independent_bindings(
    a: &EffectiveWorkerBindingV3,
    b: &EffectiveWorkerBindingV3,
    policy: IndependencePolicyV1,
) -> Result<(), String> {
    a.validate()?;
    b.validate()?;
    if policy.distinct_principals {
        for binding in [a, b] {
            require(
                !matches!(
                    &binding.execution,
                    WorkerExecutionV3::Model {
                        principal_id: None,
                        ..
                    }
                ),
                "Principal independence requires known account identities",
            )?;
        }
    }
    require(
        a.package_digest != b.package_digest,
        "Independent slots require distinct effective Worker packages",
    )?;
    match (&a.execution, &b.execution) {
        (
            WorkerExecutionV3::Model {
                provider_kind: ap,
                principal_id: ai,
                model: am,
                ..
            },
            WorkerExecutionV3::Model {
                provider_kind: bp,
                principal_id: bi,
                model: bm,
                ..
            },
        ) => {
            require(
                !policy.distinct_principals || (ai.is_some() && bi.is_some() && ai != bi),
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
            policy.command_workers_by_package
                && !policy.distinct_providers
                && !policy.distinct_models,
            "Trusted policy does not permit Command package independence for these slots",
        ),
    }
}

/// A fixed preparation plan can propose execution, but cannot satisfy business acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanPreparationV1 {
    Planning {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlanV3 {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub preparation: Option<PlanPreparationV1>,
    pub task_revision_id: String,
    pub engine_id: String,
    pub pipeline_id: String,
    pub compiled_graph_id: String,
    pub authority: TaskAuthorityV1,
    pub limits: TaskLimitsV1,
    pub inputs: BTreeMap<String, ArtifactInputV1>,
    pub bindings: BTreeMap<String, EffectiveWorkerBindingV3>,
    pub dependencies: BTreeMap<String, PlanDependencyV1>,
    pub generated_origins: Vec<GeneratedOriginV1>,
    /// Every Task obligation names its compiled evidence producers (qualified node.port).
    #[serde(deserialize_with = "super::unique_set_map")]
    pub acceptance: BTreeMap<String, BTreeSet<String>>,
}

impl ExecutionPlanV3 {
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
            if self.preparation.is_some() {
                self.acceptance.is_empty() && self.generated_origins.is_empty()
            } else {
                !self.acceptance.is_empty()
            },
            "Preparation must be fixed and cannot claim business acceptance",
        )?;
        require(
            self.acceptance.iter().all(|(name, producers)| {
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
    pub fn approves(&self, plan_id: &str, plan: &ExecutionPlanV3) -> bool {
        self.decision == PlanDecisionKindV1::Approved
            && self.plan_id == plan_id
            && self.task_revision_id == plan.task_revision_id
            && self.policy_id == plan.authority.policy_id
    }
}
