//! Resource translation for the installed Review frontend. The original Campaign caps
//! remain additional scopes on the common Task ledger; they never become another ledger.

use std::collections::{BTreeMap, BTreeSet};

use review_attempt::task_budget::{NodeAllowance, TaskTokenScope};
use review_core::{CampaignManifestV1, json::SAFE_INTEGER_MAX};
use review_graph::NodeKind;

use super::{CampaignReviewCompilation, ReviewCompileContext};
use crate::Loaded;

/// New bounded execution policy supplied by the trusted Task host. An old uncapped
/// Campaign did not capture this value: it must be recorded with the new Task's policy.
/// Existing Campaign caps always take precedence over this fallback.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewResourcePolicy {
    pub uncapped_attempt_tokens: u64,
}

impl ReviewResourcePolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.uncapped_attempt_tokens > SAFE_INTEGER_MAX as u64 {
            return Err("Review fallback reservation exceeds the Task wire range".into());
        }
        Ok(())
    }

    /// Original lifetime envelope for an installed Review Task. Capture this once, before
    /// execution; a later Round or input epoch must reuse it rather than call this again.
    /// Sequential wall bounds include every permitted retry, shard, check and Provider probe.
    /// Existing per-Round token scopes still constrain admission inside this outer envelope.
    pub fn task_limits(
        &self,
        loaded: &Loaded,
        manifest: &CampaignManifestV1,
        mode: crate::captured_review::ReviewMode,
        executions: &BTreeMap<String, review_core::task::plan::WorkerExecutionV1>,
        provider: &review_graph::task::OperatorAttemptCost,
        now_unix_ms: u64,
    ) -> Result<review_core::task::TaskLimitsV1, String> {
        use review_core::task::plan::WorkerExecutionV1;
        self.validate()?;
        manifest.validate()?;
        let convergence = mode.convergence(loaded.convergence());
        let budgets = loaded.budgets().map(|v| review_core::CampaignBudgetV1 {
            attempt_tokens: v.attempt,
            run_tokens: v.run,
        });
        if manifest.budgets != budgets
            || manifest.convergence.clean_rounds != convergence.clean_rounds
            || manifest.convergence.max_rounds != convergence.max_rounds
            || manifest.convergence.gate != format!("{:?}", convergence.gate).to_lowercase()
            || manifest.check_timeout_seconds != loaded.check_timeout_seconds()
            || now_unix_ms == 0
            || provider.tokens == 0
            || provider.wall_ms == 0
            || provider.tokens > SAFE_INTEGER_MAX as u64
            || provider.wall_ms > SAFE_INTEGER_MAX as u64
        {
            return Err(
                "Review Task envelope differs from captured authority or bounded host policy"
                    .into(),
            );
        }
        let expected: BTreeSet<_> = loaded
            .planned()
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::Reviewer | NodeKind::Scatter))
            .map(|(name, _)| name)
            .collect();
        if expected != executions.keys().collect() {
            return Err(
                "Review Task envelope requires every exact Worker execution binding".into(),
            );
        }
        let worker_wall = milliseconds(manifest.reviewer_timeout_seconds)?;
        let check_wall = milliseconds(loaded.check_timeout_seconds())?;
        let gate_wall = bounded_mul(check_wall, loaded.checks().len().max(1) as u64)?;
        let mut attempts = 0;
        let mut tokens = 0;
        let mut wall = 0;
        for (name, node) in &loaded.planned().nodes {
            match node.kind {
                NodeKind::Gate => {
                    attempts = bounded_add(attempts, 1)?;
                    wall = bounded_add(wall, gate_wall)?;
                }
                NodeKind::Reviewer | NodeKind::Scatter => {
                    let count = if node.kind == NodeKind::Scatter {
                        let policies: Vec<_> = loaded
                            .slicing()
                            .values()
                            .filter(|policy| policy.scatter == *name)
                            .collect();
                        if policies.len() != 1 {
                            return Err("Review Scatter needs one captured fan-out bound".into());
                        }
                        u64::from(policies[0].max_fanout)
                    } else {
                        1
                    };
                    let count = bounded_mul(count, 2)?;
                    attempts = bounded_add(attempts, count)?;
                    tokens = bounded_add(
                        tokens,
                        bounded_mul(
                            count,
                            loaded
                                .attempt_cap_for(name)
                                .unwrap_or(self.uncapped_attempt_tokens),
                        )?,
                    )?;
                    wall = bounded_add(wall, bounded_mul(count, worker_wall)?)?;
                    executions[name].validate()?;
                    if matches!(executions[name], WorkerExecutionV1::Model { .. }) {
                        // The compiler may share an identical probe across slots. Counting
                        // each configured slot is an upper bound, not permission to duplicate it.
                        attempts = bounded_add(attempts, 1)?;
                        tokens = bounded_add(tokens, provider.tokens)?;
                        wall = bounded_add(wall, provider.wall_ms)?;
                    }
                }
                _ => {}
            }
        }
        let rounds = u64::from(convergence.max_rounds);
        let mut attempts = bounded_mul(attempts, rounds)?;
        let tokens = bounded_mul(budgets.as_ref().map_or(tokens, |v| v.run_tokens), rounds)?;
        let mut wall = bounded_mul(wall, rounds)?;
        if mode == crate::captured_review::ReviewMode::Heavy
            && let Some(integration) = loaded.integration()
        {
            // A promoted head needs another complete Round, so no check phase can start
            // after the final permitted Round. Each sequence is one common Attempt.
            let phases = rounds.saturating_sub(1);
            attempts = bounded_add(attempts, phases)?;
            wall = bounded_add(
                wall,
                bounded_mul(
                    phases,
                    bounded_mul(check_wall, integration.post_apply_checks.len() as u64)?,
                )?,
            )?;
        }
        let limits = review_core::task::TaskLimitsV1 {
            tokens,
            max_attempts: u32::try_from(attempts.max(1))
                .map_err(|_| "Review Task Attempt envelope exceeds its wire range")?,
            deadline_unix_ms: bounded_add(now_unix_ms, wall.max(1))?,
            verification: review_core::task::VerificationReserveV1 {
                tokens: 0,
                attempts: 0,
                wall_ms: 0,
            },
        };
        limits.validate()?;
        Ok(limits)
    }

    /// Replace all caller-supplied execution bounds with captured Review authority.
    /// The caller retains the original whole-Task limits and exact package/input mapping.
    pub fn apply(
        &self,
        loaded: &Loaded,
        manifest: &CampaignManifestV1,
        context: &mut ReviewCompileContext,
    ) -> Result<(), String> {
        self.validate()?;
        manifest.validate()?;
        context.limits.validate()?;
        let worker_wall = milliseconds(manifest.reviewer_timeout_seconds)?;
        let check_wall = milliseconds(loaded.check_timeout_seconds())?;
        let checks = u64::try_from(loaded.checks().len().max(1))
            .map_err(|_| "Too many captured Review checks")?;
        let gate_wall = check_wall
            .checked_mul(checks)
            .filter(|wall| *wall <= SAFE_INTEGER_MAX as u64)
            .ok_or("Captured Review Gate sequence exceeds the Task wall-time range")?;
        let expected = loaded.budgets().map(|caps| review_core::CampaignBudgetV1 {
            attempt_tokens: caps.attempt,
            run_tokens: caps.run,
        });
        if manifest.budgets != expected
            || manifest.check_timeout_seconds != loaded.check_timeout_seconds()
        {
            return Err("Review resource authority differs from its captured definition".into());
        }
        let expected_workers: BTreeSet<_> = loaded
            .planned()
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::Reviewer | NodeKind::Scatter))
            .map(|(name, _)| name)
            .collect();
        if expected_workers != context.workers.keys().collect() {
            return Err("Review resource bindings differ from captured Worker nodes".into());
        }
        // Validate the complete replacement before mutating even this preparation object.
        let allowances: BTreeMap<_, _> = context
            .workers
            .keys()
            .map(|node| {
                let tokens = loaded
                    .attempt_cap_for(node)
                    .unwrap_or(self.uncapped_attempt_tokens);
                if tokens > context.limits.tokens || tokens > SAFE_INTEGER_MAX as u64 {
                    return Err(format!(
                        "Task cannot reserve captured Review Worker `{node}`"
                    ));
                }
                Ok((
                    node.clone(),
                    NodeAllowance {
                        tokens_per_attempt: tokens,
                        wall_ms_per_attempt: worker_wall,
                        // One initial Attempt and the existing bounded contract/timeout retry.
                        // Durable failure classification still decides whether retry is legal.
                        max_attempts: 2,
                        verification_attempts: 0,
                    },
                ))
            })
            .collect::<Result<_, String>>()?;
        for (node, allowance) in allowances {
            context.workers.get_mut(&node).unwrap().allowance = allowance;
        }
        context.max_parallel = 4;
        context.gate_wall_ms = gate_wall;
        Ok(())
    }
}

fn bounded_add(left: u64, right: u64) -> Result<u64, String> {
    left.checked_add(right)
        .filter(|v| *v <= SAFE_INTEGER_MAX as u64)
        .ok_or_else(|| "Review Task resource envelope exceeds its wire range".into())
}

fn bounded_mul(left: u64, right: u64) -> Result<u64, String> {
    left.checked_mul(right)
        .filter(|v| *v <= SAFE_INTEGER_MAX as u64)
        .ok_or_else(|| "Review Task resource envelope exceeds its wire range".into())
}

fn milliseconds(seconds: u64) -> Result<u64, String> {
    seconds
        .checked_mul(1000)
        .filter(|wall| *wall > 0 && *wall <= SAFE_INTEGER_MAX as u64)
        .ok_or_else(|| "Captured Review timeout exceeds the Task wall-time range".into())
}

/// Derive the original per-Round scopes after Provider admission has been installed, so its
/// paid operations share the same Round cap. Input-epoch changes reuse these scope identities;
/// a later numeric Round gets new scopes while the Task retains all earlier charges.
pub fn review_token_scopes(
    loaded: &Loaded,
    compilation: &CampaignReviewCompilation,
    round: u32,
) -> Result<BTreeMap<String, TaskTokenScope>, String> {
    if round == 0 {
        return Err("Review resource scopes require a numeric Round".into());
    }
    let Some(budget) = loaded.budgets() else {
        return Ok(BTreeMap::new());
    };
    let scope_root = format!("review.round{round}");
    let mut scopes = BTreeMap::new();
    if !compilation.graph.allowances.is_empty() || !compilation.graph.owned_children.is_empty() {
        scopes.insert(
            scope_root.clone(),
            TaskTokenScope {
                tokens: budget.run,
                members: BTreeSet::from(["root".into()]),
            },
        );
    }
    for (review_node, node) in &loaded.planned().nodes {
        let mapping = compilation
            .nodes
            .get(review_node)
            .ok_or("Review resource scope has no compiled node mapping")?;
        if node.kind == NodeKind::Scatter {
            // Each shard's reservation uses the Scatter's Attempt cap. Its aggregate scope
            // is only the captured FanOut cap, never the static parent Node cap.
            scopes.insert(
                format!("{scope_root}.fanout.{}", mapping.task_node),
                TaskTokenScope {
                    tokens: budget.fan_out.unwrap_or(budget.run),
                    members: BTreeSet::from([mapping.task_node.clone()]),
                },
            );
        } else if let Some(tokens) = loaded.node_attempt_caps().get(review_node) {
            scopes.insert(
                format!("{scope_root}.node.{}", mapping.task_node),
                TaskTokenScope {
                    tokens: *tokens,
                    members: BTreeSet::from([mapping.task_node.clone()]),
                },
            );
        }
    }
    Ok(scopes)
}
