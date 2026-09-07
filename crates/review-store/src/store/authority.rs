//! The pinned pipeline as replay reads it: one Campaign's authority plan, indexed by node, plus
//! the dynamic shard authority a persisted Slice Set adds beneath a Scatter.

use review_core::definition::{
    CacheKindSpec, IntegrationSpec, NodeKindSpec, NodeSpec, PipelineDefinition, PortContractSpec,
    ReviewerExecutionSpec, TypedPortSpec,
};
use rusqlite::params;

use crate::cas::Cas;
use crate::store::StoreError;

/// The pinned pipeline as replay reads it: the one shared shape
/// ([`PipelineDefinition`]), admitted by exactly the rules the loader applied when the Campaign
/// opened, then indexed by node for the checks below. Nothing here re-describes that shape; a
/// pipeline field is added in `review_core::definition` and reaches replay from there.
pub(crate) struct AuthorityPlan {
    pub(crate) version: u32,
    pub(crate) pipeline_policy_id: String,
    pub(crate) nodes: std::collections::BTreeMap<String, NodeSpec>,
    pub(crate) budgeted: bool,
    pub(crate) gate_bound: bool,
    pub(crate) gate_nodes: std::collections::BTreeSet<String>,
    pub(crate) cache_kinds: std::collections::BTreeSet<CacheKindSpec>,
    pub(crate) reviewer_execution: std::collections::BTreeMap<String, ReviewerExecutionSpec>,
    pub(crate) integration: Option<IntegrationSpec>,
    pub(crate) check_names: Vec<String>,
}

impl AuthorityPlan {
    pub(crate) fn reviewer_execution_for(&self, node: &str) -> Option<&ReviewerExecutionSpec> {
        self.reviewer_execution.get(node).or_else(|| {
            let (owner, _) = node.split_once("#slice:")?;
            (self.nodes.get(owner)?.kind == NodeKindSpec::Scatter)
                .then(|| self.reviewer_execution.get(owner))
                .flatten()
        })
    }
}

pub(crate) struct DynamicNodeAuthority {
    pub(crate) slice: review_core::ReviewSliceV1,
    pub(crate) inputs: Vec<PortContractSpec>,
    pub(crate) outputs: Vec<PortContractSpec>,
}

pub(crate) fn dynamic_node_authority(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
    round_event_id: &str,
    plan: &AuthorityPlan,
    runtime_node: &str,
) -> Result<Option<DynamicNodeAuthority>, StoreError> {
    if plan.nodes.contains_key(runtime_node) {
        return Ok(None);
    }
    let mut statement = tx.prepare(
        "SELECT node_id, payload FROM events
         WHERE run_id = ?1 AND causation_id = ?2 AND type = 'SliceSetAccepted@1'
         ORDER BY sequence",
    )?;
    let rows = statement.query_map(params![run_id, round_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut resolved = None;
    for row in rows {
        let (slicer_id, payload) = row?;
        let payload: review_core::SliceSetAcceptedPayloadV1 = serde_json::from_str(&payload)?;
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&payload.slice_set_artifact_id)
                .map_err(|error| StoreError::Conflict(error.to_string()))?,
        )?;
        crate::validate_envelope(&envelope).map_err(StoreError::Conflict)?;
        if envelope.artifact_type != review_core::contract::SLICE_SET_V1
            || envelope.artifact_id != payload.slice_set_id
        {
            return Err(StoreError::Conflict(
                "durable SliceSet authority contradicts its accepted payload".into(),
            ));
        }
        let set: review_core::SliceSetV1 = serde_json::from_value(envelope.payload)?;
        set.validate().map_err(StoreError::Conflict)?;
        let Some(slice) = set
            .slices
            .iter()
            .find(|slice| slice.runtime_node_id == runtime_node)
        else {
            continue;
        };
        if resolved.is_some() {
            return Err(StoreError::Conflict(format!(
                "runtime node `{runtime_node}` is authorized by multiple Slice Sets"
            )));
        }
        let slicer = plan.nodes.get(&slicer_id).ok_or_else(|| {
            StoreError::Conflict("SliceSetAccepted@1 names a non-plan Slicer".into())
        })?;
        if slicer.kind != NodeKindSpec::Slicer {
            return Err(StoreError::Conflict(
                "SliceSetAccepted@1 producer is not a pinned Slicer".into(),
            ));
        }
        let owner = slicer
            .slicing
            .as_ref()
            .map(|policy| policy.scatter.clone())
            .ok_or_else(|| StoreError::Conflict("pinned Slicer has no Scatter owner".into()))?;
        let scatter = plan.nodes.get(&owner).ok_or_else(|| {
            StoreError::Conflict("pinned Slicer names an absent Scatter owner".into())
        })?;
        if scatter.kind != NodeKindSpec::Scatter
            || !runtime_node.starts_with(&format!("{owner}#slice:"))
        {
            return Err(StoreError::Conflict(
                "runtime node identity disagrees with its pinned Scatter".into(),
            ));
        }
        let mut inputs = scatter
            .inputs
            .iter()
            .filter(|port| port.artifact_type() != review_core::contract::SLICE_SET_V1)
            .cloned()
            .collect::<Vec<_>>();
        inputs.push(PortContractSpec::Typed(TypedPortSpec {
            name: "slice".into(),
            artifact_type: review_core::contract::REVIEW_SLICE_V1.into(),
            cardinality: review_core::PortCardinality::One,
            optional: false,
            snapshot_affinity: review_core::SnapshotAffinity::SameSubject,
        }));
        let result_type = if inputs
            .iter()
            .any(|port| port.artifact_type() == review_core::contract::FINDING_SET_V1)
        {
            review_core::contract::REVIEWER_RESULT_V2
        } else {
            review_core::contract::REVIEWER_RESULT_V1
        };
        resolved = Some(DynamicNodeAuthority {
            slice: slice.clone(),
            inputs,
            outputs: vec![PortContractSpec::Typed(TypedPortSpec {
                name: "out".into(),
                artifact_type: result_type.into(),
                cardinality: review_core::PortCardinality::One,
                optional: false,
                snapshot_affinity: review_core::SnapshotAffinity::SameSubject,
            })],
        });
    }
    Ok(resolved)
}

pub(crate) fn load_authority_plan(
    tx: &rusqlite::Transaction<'_>,
    cas: &Cas,
    run_id: &str,
) -> Result<AuthorityPlan, StoreError> {
    let raw: String = tx.query_row(
        "SELECT payload FROM events
         WHERE run_id = ?1 AND type = 'CampaignOpened@1'
         ORDER BY sequence LIMIT 1",
        params![run_id],
        |row| row.get(0),
    )?;
    let opened: review_core::CampaignOpenedPayloadV1 = serde_json::from_str(&raw)?;
    load_authority_plan_id(
        cas,
        &opened.campaign_manifest_id,
        &opened.authority_snapshot_id,
    )
}

pub(crate) fn load_authority_plan_id(
    cas: &Cas,
    manifest_id: &str,
    authority_snapshot_id: &str,
) -> Result<AuthorityPlan, StoreError> {
    let manifest = cas
        .get_json(manifest_id)
        .map_err(|error| StoreError::Conflict(format!("unreadable CampaignManifest: {error}")))?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(manifest)?;
    manifest.validate().map_err(StoreError::Conflict)?;
    if manifest.authority_snapshot_id != authority_snapshot_id {
        return Err(StoreError::Conflict(
            "CampaignManifest authority does not match CampaignOpened@1".into(),
        ));
    }
    let budgeted = manifest.budgets.is_some();
    let pipeline = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let pipeline = std::str::from_utf8(&pipeline)
        .map_err(|error| StoreError::Conflict(format!("pinned pipeline is not UTF-8: {error}")))?;
    // The one shape the loader admitted when the Campaign opened, judged by the same rules: a
    // pinned pipeline that fails here was never valid authority, whichever reader sees it first.
    let definition = PipelineDefinition::from_toml(pipeline)
        .and_then(|definition| definition.validate().map(|()| definition))
        .map_err(|error| StoreError::Conflict(format!("pinned pipeline is invalid: {error}")))?;
    let check_names = definition
        .checks
        .iter()
        .map(|check| check.name.clone())
        .collect();
    let gate_bound = definition.gate.is_some();
    let cache_kinds = definition
        .gate
        .iter()
        .flat_map(|gate| gate.caches.iter().copied())
        .collect();
    // Node IDs are unique and non-empty by the shape's rules, so indexing loses nothing.
    let nodes: std::collections::BTreeMap<String, NodeSpec> = definition
        .nodes
        .into_iter()
        .map(|node| (node.id.clone(), node))
        .collect();
    let gate_nodes = nodes
        .values()
        .filter(|node| node.kind == NodeKindSpec::Gate)
        .map(|node| node.id.clone())
        .collect();
    // Only a reviewer-capable node of a v4/v5 pipeline carries an Execution Binding, and every
    // such node must: the shape refused anything else above.
    let reviewer_execution = nodes
        .values()
        .filter_map(|node| {
            node.execution
                .clone()
                .map(|execution| (node.id.clone(), execution))
        })
        .collect();
    Ok(AuthorityPlan {
        version: definition.version,
        pipeline_policy_id: manifest.pipeline.artifact_id,
        nodes,
        budgeted,
        gate_bound,
        gate_nodes,
        cache_kinds,
        reviewer_execution,
        integration: definition.integration,
        check_names,
    })
}

/// The pinned cache kind a run-time cache report names. Both enums are closed: a new package
/// manager adds a variant to each and one line here.
pub(crate) fn pinned_cache_kind(kind: review_core::RunCacheKindV5) -> CacheKindSpec {
    match kind {
        review_core::RunCacheKindV5::Cargo => CacheKindSpec::Cargo,
    }
}

pub(crate) fn cache_kind_name(kind: CacheKindSpec) -> &'static str {
    match kind {
        CacheKindSpec::Cargo => "cargo",
    }
}
