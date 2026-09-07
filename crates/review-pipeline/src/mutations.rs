//! Bounded summaries of sandbox mutation sets, shared by the review and Task paths.
//!
//! A mutation set can be enormous — a Worker that built to verify a claim leaves a whole
//! `target/` behind, ~10k paths on this workspace. The complete list is durable exactly once, in
//! the CAS; anything that travels further (a provenance record, a Worker prompt) carries only
//! the counts, a fixed-size sorted sample, and the artifact ID that names the rest.

use review_sandbox::MutationSet;

/// Paths quoted inline from a mutation set.
pub const MUTATION_SAMPLE: usize = 20;

/// The bounded shape: counts per kind, the first [`MUTATION_SAMPLE`] paths in sorted order across
/// all three kinds, whether that truncated anything, and the artifact holding the full lists.
pub fn mutation_summary(mutations: &MutationSet, full_artifact: &str) -> serde_json::Value {
    let groups = [&mutations.added, &mutations.modified, &mutations.deleted];
    let mut positions = [0_usize; 3];
    let mut sample = Vec::new();
    while sample.len() < MUTATION_SAMPLE {
        let next = (0..groups.len())
            .filter(|index| positions[*index] < groups[*index].len())
            .min_by_key(|index| groups[*index][positions[*index]].as_str());
        let Some(index) = next else { break };
        sample.push(&groups[index][positions[index]]);
        positions[index] += 1;
    }
    let count = groups.iter().map(|group| group.len()).sum::<usize>();
    serde_json::json!({
        "count": count,
        "added": mutations.added.len(),
        "modified": mutations.modified.len(),
        "deleted": mutations.deleted.len(),
        "sample": sample,
        "truncated": count > MUTATION_SAMPLE,
        "artifact": full_artifact,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_is_bounded_and_sorted_across_kinds() {
        let mutations = MutationSet {
            added: (0..5_000)
                .map(|index| format!("target/debug/{index:05}.o"))
                .collect(),
            modified: vec!["Cargo.lock".into(), "src/lib.rs".into()],
            deleted: vec!["docs/old.md".into()],
        };
        let summary = mutation_summary(&mutations, "sha256:full");
        let sample = summary["sample"].as_array().unwrap();
        assert_eq!(sample.len(), MUTATION_SAMPLE);
        assert_eq!(sample[0], "Cargo.lock");
        assert_eq!(sample[1], "docs/old.md");
        assert_eq!(sample[2], "src/lib.rs");
        assert_eq!(sample[3], "target/debug/00000.o");
        assert_eq!(summary["count"], 5_003);
        assert_eq!(summary["added"], 5_000);
        assert_eq!(summary["truncated"], true);
        assert_eq!(summary["artifact"], "sha256:full");
        assert!(serde_json::to_string(&summary).unwrap().len() < 1_024);
    }
}
