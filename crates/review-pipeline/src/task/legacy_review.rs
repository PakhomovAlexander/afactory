//! Captured legacy Review inputs and domain operations for the common Task host. Nothing in
//! this module owns an Attempt, retry loop or budget. Recompilation and currentness checks
//! remain required before an execution plan can use these data adapters.

use std::collections::BTreeMap;

use review_config::task::legacy_review::{
    LegacyReviewCompilation, ReviewCompileContext, ReviewNodeMapping, ReviewWorker,
    artifact::ReviewArtifactCodec, resources::ReviewResourcePolicy,
};
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_core::task::{ArtifactInputV1, TaskLimitsV1};
use review_core::{ArtifactEnvelope, PortCardinality, Producer, contract};
use review_graph::{Node, NodeKind};
use review_store::{Cas, EventStore, validate_envelope};

use crate::RoundAuthority;

pub mod plan;

pub struct CapturedLegacyReviewRound {
    authority: RoundAuthority,
}

/// Captured Review data and the common graph before effective Provider admission and Task
/// acceptance are attached. This value alone is not an executable-plan capability.
pub struct CapturedReviewCompilation {
    pub loaded: review_config::Loaded,
    pub compilation: LegacyReviewCompilation,
}

/// A recorded Task's root inputs and the host-selected public contract. These declarations
/// are checked against the actual captured Round before compiling the graph.
pub struct ReviewCompilationRequest {
    pub limits: TaskLimitsV1,
    pub inputs: BTreeMap<String, ArtifactInputV1>,
    pub outputs: BTreeMap<String, review_graph::task::Address>,
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

    /// Historical reconstruction only; execution still checks the current Store Round fence.
    pub fn load_recorded(
        cas: &Cas,
        store: &EventStore,
        campaign: &str,
        round_event: &str,
    ) -> Result<Self, String> {
        Ok(Self {
            authority: RoundAuthority::load_recorded(store, cas, campaign, round_event)?,
        })
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

    /// Compile the actual captured Round without consulting a live authority directory.
    /// The trusted host supplies the new original Task allowance and public output mapping;
    /// captured Review authority determines every Worker/Gate bound and aggregate scope.
    pub fn compile(
        &self,
        cas: &Cas,
        mode: review_config::captured_review::ReviewMode,
        resources: &ReviewResourcePolicy,
        limits: TaskLimitsV1,
        outputs: BTreeMap<String, review_graph::task::Address>,
    ) -> Result<CapturedReviewCompilation, String> {
        self.compile_existing(
            cas,
            mode,
            resources,
            ReviewCompilationRequest {
                limits,
                inputs: self.capture_inputs(cas)?,
                outputs,
            },
        )
    }

    /// Read-only recompilation for plan admission and resume. In particular, missing recorded
    /// wrappers must be refused before capture could recreate their content-addressed bytes.
    pub fn compile_existing(
        &self,
        cas: &Cas,
        mode: review_config::captured_review::ReviewMode,
        resources: &ReviewResourcePolicy,
        request: ReviewCompilationRequest,
    ) -> Result<CapturedReviewCompilation, String> {
        self.validate_inputs(cas, &request.inputs)?;
        let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
            cas.get_json(&self.authority.campaign_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let loaded = review_config::captured_review::load_captured_review(cas, &manifest, mode)?;
        let mut context = ReviewCompileContext {
            finding_identity_policy: manifest.finding_identity_policy.clone(),
            inputs: request.inputs,
            head_input: "head".into(),
            round_input: "round".into(),
            workers: loaded
                .planned()
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, (_, node))| {
                    matches!(node.kind, NodeKind::Reviewer | NodeKind::Scatter)
                })
                .map(|(index, (name, _))| {
                    (
                        name.clone(),
                        ReviewWorker {
                            // A deterministic slot handle, not a substitute for the original
                            // package bytes and exact invocation policy in plan admission.
                            package: format!("af/legacy-review-worker-{index}"),
                            allowance: review_attempt::task_budget::NodeAllowance {
                                tokens_per_attempt: 0,
                                wall_ms_per_attempt: 1,
                                max_attempts: 1,
                                verification_attempts: 0,
                            },
                        },
                    )
                })
                .collect(),
            outputs: request.outputs,
            limits: request.limits.clone(),
            max_parallel: 4,
            gate_wall_ms: 1,
        };
        resources.apply(&loaded, &manifest, &mut context)?;
        let mut compilation =
            review_config::task::legacy_review::compile_legacy_review(&loaded, context)?;
        compilation.graph.token_scopes =
            review_config::task::legacy_review::resources::review_token_scopes(
                &loaded,
                &compilation,
                self.authority.round,
            )?;
        compilation.graph.budget(request.limits)?;
        Ok(CapturedReviewCompilation {
            loaded,
            compilation,
        })
    }

    fn validate_inputs(
        &self,
        cas: &Cas,
        inputs: &BTreeMap<String, ArtifactInputV1>,
    ) -> Result<(), String> {
        if inputs.len() != 2 || !inputs.contains_key("head") || !inputs.contains_key("round") {
            return Err("Captured Review requires exactly its head and Round inputs".into());
        }
        let binding = self.binding();
        for (name, ty) in [
            ("head", contract::SOURCE_SNAPSHOT_V1),
            ("round", LEGACY_REVIEW_ROUND_V1),
        ] {
            let input = &inputs[name];
            input.validate()?;
            if input.artifact_type != ty
                || input.cardinality != PortCardinality::One
                || input.artifact_ids.len() != 1
                || input.snapshot_id.as_deref() != Some(&binding.head_snapshot_id)
            {
                return Err("Captured Review input differs from its exact Round contract".into());
            }
            let wrapper = cas
                .get_artifact(&input.artifact_ids[0])
                .map_err(|error| error.to_string())?;
            if wrapper.artifact_type != ty
                || wrapper.producer != self.producer(None, name)
                || wrapper.subject_snapshot_id != input.snapshot_id
            {
                return Err(
                    "Captured Review input has different type, producer or Snapshot".into(),
                );
            }
            if name == "head" {
                let raw = ReviewArtifactCodec::Flat {
                    artifact_type: ty.into(),
                }
                .restore(cas, &wrapper.artifact_id)?;
                if raw != binding.head_snapshot_id {
                    return Err("Captured Review input names another head".into());
                }
            } else if wrapper.payload
                != serde_json::to_value(&binding).map_err(|error| error.to_string())?
                || wrapper.input_artifacts != binding.artifact_refs()
            {
                return Err("Captured Review input names another Round authority".into());
            }
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
        let raw_outputs = crate::review_domain::generation_outputs(
            &self.authority,
            pipeline_version,
            Some(&self.authority.prior_finding_set_id),
            node,
        )?;
        let mut outputs = BTreeMap::new();
        for (port, output) in &mapping.outputs {
            let original = node
                .outputs
                .iter()
                .find(|original| original.name == output.review_port)
                .ok_or("Unknown original Generation output")?;
            let raw = &raw_outputs[&original.name];
            if raw.is_empty() {
                if !original.optional {
                    return Err("Review genesis requires an optional Finding Set output".into());
                }
                continue;
            }
            let [raw] = raw.as_slice() else {
                return Err(
                    "Review Generation requires exactly one artifact per present output".into(),
                );
            };
            let ids = vec![output.codec.capture(
                cas,
                raw,
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
