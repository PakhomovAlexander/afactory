//! Captured legacy Review inputs and domain operations for the common Task host. Nothing in
//! this module owns an Attempt, retry loop or budget. Recompilation and currentness checks
//! remain required before an execution plan can use these data adapters.

use std::collections::BTreeMap;

use review_config::task::legacy_review::{ReviewNodeMapping, artifact::ReviewArtifactCodec};
use review_core::task::ArtifactInputV1;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_core::{ArtifactEnvelope, PortCardinality, Producer, contract};
use review_graph::{Node, NodeKind};
use review_store::{Cas, EventStore, validate_envelope};

use crate::RoundAuthority;

pub struct CapturedLegacyReviewRound {
    authority: RoundAuthority,
}

impl CapturedLegacyReviewRound {
    pub fn load(
        cas: &Cas,
        store: &EventStore,
        campaign: &str,
        round_event: &str,
    ) -> Result<Self, String> {
        Ok(Self {
            authority: RoundAuthority::load(store, cas, campaign, round_event)?,
        })
    }

    pub fn authority(&self) -> &RoundAuthority {
        &self.authority
    }

    pub fn binding(&self) -> LegacyReviewRoundV1 {
        LegacyReviewRoundV1 {
            campaign_id: self.authority.run_id.clone(),
            round_event_id: self.authority.round_event_id.clone(),
            campaign_manifest_id: self.authority.campaign_manifest_id.clone(),
            subject_id: self.authority.subject_id.clone(),
            head_snapshot_id: self.authority.head_snapshot_id.clone(),
            round: self.authority.round,
            epoch: self.authority.epoch,
        }
    }

    pub fn check_current(&self, cas: &Cas, store: &EventStore) -> Result<(), String> {
        let current = Self::load(
            cas,
            store,
            &self.authority.run_id,
            &self.authority.round_event_id,
        )?;
        if current.binding() != self.binding() {
            return Err("Captured Review Round authority changed".into());
        }
        Ok(())
    }

    fn producer(&self, node: Option<String>, operation: &str) -> Producer {
        Producer::KernelOperation {
            run_id: self.authority.run_id.clone(),
            node_id: node,
            operation_id: format!(
                "legacy-review-{operation}@1:{}",
                self.authority.round_event_id
            ),
        }
    }

    /// Capture only immutable Round authority and its head. The original Snapshot remains a
    /// raw canonical Review artifact; the Task wrapper retains that exact content reference.
    pub fn capture_inputs(&self, cas: &Cas) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        let binding = self.binding();
        binding.validate()?;
        let codec = ReviewArtifactCodec::Flat {
            artifact_type: contract::SOURCE_SNAPSHOT_V1.into(),
        };
        let head = codec.capture(
            cas,
            &binding.head_snapshot_id,
            self.producer(None, "head"),
            Some(binding.head_snapshot_id.clone()),
        )?;
        let round = cas
            .put_artifact(
                LEGACY_REVIEW_ROUND_V1,
                self.producer(None, "round"),
                binding
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                Some(binding.head_snapshot_id.clone()),
                serde_json::to_value(binding).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        Ok(BTreeMap::from([
            (
                "head".into(),
                artifact_input(
                    cas,
                    contract::SOURCE_SNAPSHOT_V1,
                    vec![head],
                    PortCardinality::One,
                )?,
            ),
            (
                "round".into(),
                artifact_input(
                    cas,
                    LEGACY_REVIEW_ROUND_V1,
                    vec![round],
                    PortCardinality::One,
                )?,
            ),
        ]))
    }

    /// The existing Generation semantics over the exact Round, with compiled output codecs.
    /// Historical Finding Sets are forwarded unchanged; Campaign genesis remains optional
    /// absence rather than a freshly stamped, invented empty reduction.
    pub fn generation_outputs(
        &self,
        cas: &Cas,
        pipeline_version: u32,
        node: &Node,
        mapping: &ReviewNodeMapping,
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        if node.kind != NodeKind::Generation || mapping.outputs.len() != node.outputs.len() {
            return Err("Review Generation differs from its compiled outputs".into());
        }
        let mut outputs = BTreeMap::new();
        for (port, output) in &mapping.outputs {
            let original = node
                .outputs
                .iter()
                .find(|original| original.name == output.review_port)
                .ok_or("Unknown original Generation output")?;
            let raw = if crate::is_generation_prior_findings_output(original, pipeline_version) {
                Some(self.authority.prior_finding_set_id.clone())
            } else if crate::is_generation_finding_set_output(original) {
                (self.authority.prior_reduction_finding_set_id != self.authority.finding_genesis_id)
                    .then(|| self.authority.prior_reduction_finding_set_id.clone())
            } else if crate::is_change_set_port(original, pipeline_version) {
                Some(
                    self.authority
                        .change_set_id
                        .clone()
                        .ok_or("Whole-tree Review has no Change Set")?,
                )
            } else {
                return Err("Unsupported Review Generation output".into());
            };
            let Some(raw) = raw else {
                if !original.optional {
                    return Err("Review genesis requires an optional Finding Set output".into());
                }
                continue;
            };
            let ids = vec![output.codec.capture(
                cas,
                &raw,
                self.producer(Some(node.id.clone()), &format!("generation-{port}")),
                Some(self.authority.head_snapshot_id.clone()),
            )?];
            outputs.insert(
                port.clone(),
                artifact_input(cas, output.codec.artifact_type(), ids, original.cardinality)?,
            );
        }
        Ok(outputs)
    }
}

fn artifact_input(
    cas: &Cas,
    ty: &str,
    ids: Vec<String>,
    cardinality: PortCardinality,
) -> Result<ArtifactInputV1, String> {
    let mut snapshot_id = None;
    for (index, id) in ids.iter().enumerate() {
        let envelope: ArtifactEnvelope =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        validate_envelope(&envelope)?;
        if envelope.artifact_id != *id || envelope.artifact_type != ty {
            return Err("Review output differs from its compiled artifact contract".into());
        }
        if index == 0 {
            snapshot_id = envelope.subject_snapshot_id;
        } else if snapshot_id != envelope.subject_snapshot_id {
            return Err("Review output combines different Snapshot affinities".into());
        }
    }
    let output = ArtifactInputV1 {
        artifact_ids: ids,
        artifact_type: ty.into(),
        cardinality,
        snapshot_id,
    };
    output.validate()?;
    Ok(output)
}
