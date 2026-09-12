//! Resource translation for the installed Review frontend. The original Campaign caps
//! remain additional scopes on the common Task ledger; they never become another ledger.

use std::collections::{BTreeMap, BTreeSet};

use review_attempt::task_budget::{NodeAllowance, TaskTokenScope};
use review_core::{CampaignManifestV1, json::SAFE_INTEGER_MAX};
use review_graph::NodeKind;

use super::{LegacyReviewCompilation, ReviewCompileContext};
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
            || manifest
                .check_timeout_seconds
                .is_some_and(|bound| bound != loaded.check_timeout_seconds())
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
                if let Some(execution) = loaded.reviewer_execution().get(node)
                    && review_core::broker_authority_usage(&execution.operations)? > tokens
                {
                    return Err(format!(
                        "Captured Broker authority exceeds Review Worker `{node}` reservation"
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
    compilation: &LegacyReviewCompilation,
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
    if !compilation.graph.allowances.is_empty() {
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
