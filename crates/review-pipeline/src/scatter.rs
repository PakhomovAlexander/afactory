//! Deterministic M8 slicing and semantic-output closure.

use std::collections::{BTreeMap, BTreeSet};

use review_core::{
    CloseoutPolicyV1, ReviewSliceV1, SemanticClosureV1, SemanticDispositionV1, ShardOutcomeV1,
    ShardSetV1, SliceCoverageV1, SliceSetV1,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticSlicePolicy {
    pub scatter_node: String,
    pub max_paths_per_slice: usize,
    pub max_fanout: u32,
    pub coverage: SliceCoverageV1,
    pub all_shards_required: bool,
    pub closeout: CloseoutPolicyV1,
}

impl StaticSlicePolicy {
    pub fn plan(
        &self,
        subject_id: &str,
        subject_paths: &[String],
        static_node_ids: &BTreeSet<String>,
    ) -> Result<SliceSetV1, String> {
        if self.scatter_node.trim().is_empty()
            || self.max_paths_per_slice == 0
            || self.max_fanout == 0
        {
            return Err("static Slice policy has a zero bound or empty Scatter identity".into());
        }
        let mut paths = subject_paths.to_vec();
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return Err("dynamic slicing requires at least one Subject path".into());
        }
        let needed = paths.len().div_ceil(self.max_paths_per_slice);
        if needed > self.max_fanout as usize {
            return Err(format!(
                "Subject requires {needed} Slices but policy permits {}",
                self.max_fanout
            ));
        }
        let slices = paths
            .chunks(self.max_paths_per_slice)
            .enumerate()
            .map(|(ordinal, paths)| {
                let slice_id = review_store::content_id(&serde_json::json!({
                    "domain": "review.kernel/slice-id@1",
                    "subject_id": subject_id,
                    "paths": paths,
                }))
                .map_err(|error| error.to_string())?;
                let runtime_node_id = format!(
                    "{}#slice:{}:{}",
                    self.scatter_node,
                    ordinal + 1,
                    &slice_id[7..23]
                );
                if static_node_ids.contains(&runtime_node_id) {
                    return Err(format!(
                        "runtime Slice node `{runtime_node_id}` collides with a static node"
                    ));
                }
                Ok(ReviewSliceV1 {
                    slice_id,
                    runtime_node_id,
                    paths: paths.to_vec(),
                    overlaps: vec![],
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let set = SliceSetV1 {
            subject_id: subject_id.to_string(),
            coverage: self.coverage,
            max_fanout: self.max_fanout,
            all_shards_required: self.all_shards_required,
            closeout: self.closeout.clone(),
            slices,
        };
        set.validate_coverage(&paths)?;
        Ok(set)
    }
}

/// Whether a completed Scatter examined none of the prior Findings assigned to it: not one shard
/// reached `Completed`, so no reviewer under it returned a disposition. A Slice policy with
/// `all_shards_required = false` lets exactly this Scatter finish `Ok` — it is no missing node,
/// the verdict sees nothing wrong, and the Round would close with the Scatter's prior Findings
/// holding no disposition at all. The kernel routes those rows to the receiving nodes still to
/// be delivered theirs instead.
pub fn scatter_left_prior_findings_unexamined(shard_set: &ShardSetV1) -> bool {
    !shard_set
        .shards
        .iter()
        .any(|shard| matches!(shard.outcome, ShardOutcomeV1::Completed { .. }))
}

/// Prove actual semantic-output routing after scatter and reduction. The caller supplies every
/// selected semantic artifact by kind; this function deliberately flattens kinds only after each
/// ID has acquired a named authoritative sink.
pub fn prove_semantic_closure(
    slice_set: &SliceSetV1,
    shard_set: &ShardSetV1,
    selected_outputs: &BTreeMap<String, Vec<String>>,
    sinks: &BTreeMap<String, String>,
    closeout_result_id: Option<String>,
) -> Result<SemanticClosureV1, String> {
    slice_set.validate()?;
    shard_set.validate_against(slice_set)?;
    if slice_set.all_shards_required && !shard_set.complete() {
        return Err("required shard failed or is missing; semantic closure is incomplete".into());
    }
    let shard_outputs = shard_set
        .shards
        .iter()
        .flat_map(|shard| match &shard.outcome {
            ShardOutcomeV1::Completed {
                result_artifact_ids,
            } => result_artifact_ids.clone(),
            ShardOutcomeV1::Failed { .. } | ShardOutcomeV1::Missing { .. } => vec![],
        })
        .collect::<BTreeSet<_>>();
    let selected = selected_outputs
        .values()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    if !selected.is_subset(&shard_outputs) {
        return Err("semantic selection names output absent from the lossless Shard Set".into());
    }
    let mut required_artifact_ids = selected.into_iter().collect::<Vec<_>>();
    required_artifact_ids.sort();
    let dispositions = required_artifact_ids
        .iter()
        .map(|artifact_id| {
            sinks
                .get(artifact_id)
                .filter(|sink| !sink.trim().is_empty())
                .map(|sink| SemanticDispositionV1 {
                    artifact_id: artifact_id.clone(),
                    sink: sink.clone(),
                })
                .ok_or_else(|| format!("semantic output {artifact_id} has no authoritative sink"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (closeout_result_id, closeout_waiver_policy_id) = match &slice_set.closeout {
        CloseoutPolicyV1::Required => (
            Some(closeout_result_id.ok_or("whole-Subject closeout result is missing")?),
            None,
        ),
        CloseoutPolicyV1::Waived { policy_id, .. } => {
            if closeout_result_id.is_some() {
                return Err("waived closeout cannot also claim an executed result".into());
            }
            (None, Some(policy_id.clone()))
        }
    };
    let closure = SemanticClosureV1 {
        subject_id: slice_set.subject_id.clone(),
        required_artifact_ids,
        dispositions,
        closeout_result_id,
        closeout_waiver_policy_id,
    };
    closure.validate()?;
    Ok(closure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::{ShardReceiptV1, ShardSetV1};

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    /// A Scatter that completes without one `Completed` shard examined nothing; one that
    /// completed a single shard, however many others failed or went missing, did.
    #[test]
    fn a_scatter_with_no_completed_shard_examined_no_prior_finding() {
        let receipt = |slice: &str, outcome: ShardOutcomeV1| ShardReceiptV1 {
            slice_id: slice.into(),
            runtime_node_id: format!("scatter#slice:{slice}"),
            outcome,
        };
        let set = |shards: Vec<ShardReceiptV1>| ShardSetV1 {
            subject_id: digest('a'),
            slice_set_id: digest('b'),
            all_shards_required: false,
            shards,
        };

        assert!(scatter_left_prior_findings_unexamined(&set(vec![
            receipt(
                "1",
                ShardOutcomeV1::Missing {
                    reason: "the fan-out cap refused it".into()
                }
            ),
            receipt(
                "2",
                ShardOutcomeV1::Failed {
                    reason: "every attempt failed".into()
                }
            ),
        ])));
        assert!(scatter_left_prior_findings_unexamined(&set(Vec::new())));
        assert!(!scatter_left_prior_findings_unexamined(&set(vec![
            receipt(
                "1",
                ShardOutcomeV1::Completed {
                    result_artifact_ids: vec![digest('c')]
                }
            ),
            receipt(
                "2",
                ShardOutcomeV1::Failed {
                    reason: "every attempt failed".into()
                }
            ),
        ])));
    }

    #[test]
    fn static_partition_is_stable_and_bounded() {
        let policy = StaticSlicePolicy {
            scatter_node: "correctness".into(),
            max_paths_per_slice: 2,
            max_fanout: 2,
            coverage: SliceCoverageV1::Complete,
            all_shards_required: true,
            closeout: CloseoutPolicyV1::Required,
        };
        let paths = vec!["c.rs".into(), "a.rs".into(), "b.rs".into()];
        let first = policy.plan(&digest('a'), &paths, &BTreeSet::new()).unwrap();
        let second = policy.plan(&digest('a'), &paths, &BTreeSet::new()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.slices[0].paths, ["a.rs", "b.rs"]);
        assert_eq!(first.slices[1].paths, ["c.rs"]);
    }

    #[test]
    fn two_clean_slices_cannot_close_without_whole_subject_closeout() {
        let policy = StaticSlicePolicy {
            scatter_node: "correctness".into(),
            max_paths_per_slice: 1,
            max_fanout: 2,
            coverage: SliceCoverageV1::Complete,
            all_shards_required: true,
            closeout: CloseoutPolicyV1::Required,
        };
        let set = policy
            .plan(
                &digest('a'),
                &["left.rs".into(), "right.rs".into()],
                &BTreeSet::new(),
            )
            .unwrap();
        let shard_set = ShardSetV1 {
            subject_id: set.subject_id.clone(),
            slice_set_id: digest('b'),
            all_shards_required: true,
            shards: set
                .slices
                .iter()
                .enumerate()
                .map(|(index, slice)| ShardReceiptV1 {
                    slice_id: slice.slice_id.clone(),
                    runtime_node_id: slice.runtime_node_id.clone(),
                    outcome: ShardOutcomeV1::Completed {
                        result_artifact_ids: vec![digest(if index == 0 { 'c' } else { 'd' })],
                    },
                })
                .collect(),
        };
        let selected = BTreeMap::from([("reports".into(), vec![digest('c'), digest('d')])]);
        let sinks = BTreeMap::from([
            (digest('c'), "ledger".into()),
            (digest('d'), "ledger".into()),
        ]);
        assert!(
            prove_semantic_closure(&set, &shard_set, &selected, &sinks, None)
                .unwrap_err()
                .contains("closeout")
        );
    }
}
