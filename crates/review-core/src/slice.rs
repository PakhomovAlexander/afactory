//! Dynamic Review Slice, shard-result, and semantic-closure contracts.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SliceCoverageV1 {
    Complete,
    ExplicitPartial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloseoutPolicyV1 {
    Required,
    Waived { policy_id: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSliceV1 {
    pub slice_id: String,
    pub runtime_node_id: String,
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlaps: Vec<String>,
}

impl ReviewSliceV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.slice_id)
            || self.runtime_node_id.trim().is_empty()
            || self.paths.is_empty()
            || self
                .paths
                .iter()
                .any(|path| !crate::is_valid_repo_path(path))
            || self.paths.windows(2).any(|pair| pair[0] >= pair[1])
            || self.overlaps.iter().any(|id| !crate::is_digest(id))
            || self.overlaps.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("ReviewSlice@1 has invalid identity, paths, or overlaps".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceSetV1 {
    pub subject_id: String,
    pub coverage: SliceCoverageV1,
    pub max_fanout: u32,
    pub all_shards_required: bool,
    pub closeout: CloseoutPolicyV1,
    pub slices: Vec<ReviewSliceV1>,
}

impl SliceSetV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.subject_id)
            || self.slices.is_empty()
            || self.max_fanout == 0
            || self.slices.len() > self.max_fanout as usize
        {
            return Err("SliceSet@1 has invalid Subject or fan-out bounds".into());
        }
        if !self.all_shards_required && self.coverage == SliceCoverageV1::Complete {
            return Err("complete SliceSet@1 requires every shard by default".into());
        }
        if let CloseoutPolicyV1::Waived { policy_id, reason } = &self.closeout
            && (!crate::is_digest(policy_id) || reason.trim().is_empty())
        {
            return Err("SliceSet@1 has an invalid closeout waiver".into());
        }
        let mut ids = BTreeSet::new();
        let mut runtime_ids = BTreeSet::new();
        let mut by_id = BTreeMap::new();
        for slice in &self.slices {
            if slice.validate().is_err()
                || !ids.insert(slice.slice_id.as_str())
                || !runtime_ids.insert(slice.runtime_node_id.as_str())
            {
                return Err("SliceSet@1 has duplicate or invalid Slice identity".into());
            }
            let mut paths = BTreeSet::new();
            if slice
                .paths
                .iter()
                .any(|path| !crate::is_valid_repo_path(path) || !paths.insert(path))
                || slice.paths.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err("SliceSet@1 has invalid, repeated, or unsorted paths".into());
            }
            let mut overlaps = BTreeSet::new();
            if slice.overlaps.iter().any(|overlap| {
                overlap == &slice.slice_id
                    || !crate::is_digest(overlap)
                    || !overlaps.insert(overlap)
            }) || slice.overlaps.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err("SliceSet@1 has invalid overlap declarations".into());
            }
            by_id.insert(slice.slice_id.as_str(), slice);
        }
        for slice in &self.slices {
            for overlap in &slice.overlaps {
                let peer = by_id
                    .get(overlap.as_str())
                    .ok_or_else(|| "SliceSet@1 overlap names an absent Slice".to_string())?;
                if !peer.overlaps.contains(&slice.slice_id)
                    || !slice.paths.iter().any(|path| peer.paths.contains(path))
                {
                    return Err("SliceSet@1 overlaps are not symmetric and path-backed".into());
                }
            }
            for peer in &self.slices {
                if slice.slice_id < peer.slice_id
                    && slice.paths.iter().any(|path| peer.paths.contains(path))
                    && !slice.overlaps.contains(&peer.slice_id)
                {
                    return Err("SliceSet@1 has an undeclared path overlap".into());
                }
            }
        }
        Ok(())
    }

    pub fn validate_coverage(&self, subject_paths: &[String]) -> Result<(), String> {
        self.validate()?;
        let declared = self
            .slices
            .iter()
            .flat_map(|slice| slice.paths.iter().cloned())
            .collect::<BTreeSet<_>>();
        let expected = subject_paths.iter().cloned().collect::<BTreeSet<_>>();
        if declared.iter().any(|path| !expected.contains(path)) {
            return Err("SliceSet@1 includes a path outside the Subject".into());
        }
        if self.coverage == SliceCoverageV1::Complete && declared != expected {
            return Err("complete SliceSet@1 does not cover the whole Subject path set".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ShardOutcomeV1 {
    Completed { result_artifact_ids: Vec<String> },
    Failed { reason: String },
    Missing { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardReceiptV1 {
    pub slice_id: String,
    pub runtime_node_id: String,
    pub outcome: ShardOutcomeV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardSetV1 {
    pub subject_id: String,
    pub slice_set_id: String,
    pub all_shards_required: bool,
    pub shards: Vec<ShardReceiptV1>,
}

impl ShardSetV1 {
    pub fn validate_shape(&self) -> Result<(), String> {
        if !crate::is_digest(&self.subject_id)
            || !crate::is_digest(&self.slice_set_id)
            || self.shards.is_empty()
        {
            return Err("ShardSet@1 has invalid Subject or Slice Set identity".into());
        }
        let mut observed = BTreeSet::new();
        for shard in &self.shards {
            if !crate::is_digest(&shard.slice_id)
                || shard.runtime_node_id.trim().is_empty()
                || !observed.insert((shard.slice_id.as_str(), shard.runtime_node_id.as_str()))
            {
                return Err("ShardSet@1 has duplicate or invalid shard identity".into());
            }
            match &shard.outcome {
                ShardOutcomeV1::Completed {
                    result_artifact_ids,
                } if result_artifact_ids.is_empty()
                    || result_artifact_ids.iter().any(|id| !crate::is_digest(id)) =>
                {
                    return Err("ShardSet@1 has invalid completed output IDs".into());
                }
                ShardOutcomeV1::Failed { reason } | ShardOutcomeV1::Missing { reason }
                    if reason.trim().is_empty() =>
                {
                    return Err("ShardSet@1 has an empty failure reason".into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn validate_against(&self, slices: &SliceSetV1) -> Result<(), String> {
        self.validate_shape()?;
        slices.validate()?;
        if self.subject_id != slices.subject_id
            || !crate::is_digest(&self.slice_set_id)
            || self.all_shards_required != slices.all_shards_required
            || self.shards.len() != slices.slices.len()
        {
            return Err("ShardSet@1 contradicts its SliceSet@1 authority".into());
        }
        let expected = slices
            .slices
            .iter()
            .map(|slice| (slice.slice_id.as_str(), slice.runtime_node_id.as_str()))
            .collect::<BTreeSet<_>>();
        for shard in &self.shards {
            if !expected.contains(&(shard.slice_id.as_str(), shard.runtime_node_id.as_str())) {
                return Err("ShardSet@1 has duplicate or unknown shard identity".into());
            }
        }
        Ok(())
    }

    pub fn complete(&self) -> bool {
        self.shards
            .iter()
            .all(|shard| matches!(shard.outcome, ShardOutcomeV1::Completed { .. }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticDispositionV1 {
    pub artifact_id: String,
    pub sink: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticClosureV1 {
    pub subject_id: String,
    pub required_artifact_ids: Vec<String>,
    pub dispositions: Vec<SemanticDispositionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closeout_result_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closeout_waiver_policy_id: Option<String>,
}

impl SemanticClosureV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.subject_id)
            || self
                .required_artifact_ids
                .iter()
                .any(|id| !crate::is_digest(id))
            || self
                .closeout_result_id
                .as_deref()
                .is_some_and(|id| !crate::is_digest(id))
            || self
                .closeout_waiver_policy_id
                .as_deref()
                .is_some_and(|id| !crate::is_digest(id))
            || self.closeout_result_id.is_some() == self.closeout_waiver_policy_id.is_some()
        {
            return Err("SemanticClosure@1 has invalid Subject or closeout authority".into());
        }
        let required = self.required_artifact_ids.iter().collect::<BTreeSet<_>>();
        if required.len() != self.required_artifact_ids.len() {
            return Err("SemanticClosure@1 repeats a required artifact".into());
        }
        let mut disposed = BTreeSet::new();
        for disposition in &self.dispositions {
            if !crate::is_digest(&disposition.artifact_id)
                || disposition.sink.trim().is_empty()
                || !disposed.insert(&disposition.artifact_id)
            {
                return Err("SemanticClosure@1 has an invalid or repeated disposition".into());
            }
        }
        if required != disposed {
            return Err("SemanticClosure@1 is missing or invents a semantic disposition".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceSetAcceptedPayloadV1 {
    pub slice_set_id: String,
    pub slice_set_artifact_id: String,
}

impl SliceSetAcceptedPayloadV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::is_digest(&self.slice_set_id) || !crate::is_digest(&self.slice_set_artifact_id) {
            return Err("SliceSetAccepted@1 contains an invalid artifact ID");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedSetPayloadV1 {
    pub artifact_id: String,
    pub record_id: String,
}

impl RecordedSetPayloadV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::is_digest(&self.artifact_id) || !crate::is_digest(&self.record_id) {
            return Err("recorded set contains an invalid artifact ID");
        }
        Ok(())
    }
}
