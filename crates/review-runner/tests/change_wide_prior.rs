use review_core::ReviewerResultContract;
use review_runner::{RESULT_CONTRACT, ReviewerInputs};

#[test]
fn prompt_states_the_change_wide_encoding_used_by_prior_rows() {
    let rendered = ReviewerInputs {
        prior_findings: Some(serde_json::json!([{
            "key": "claim",
            "file": null,
            "title": "change-wide claim"
        }])),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(RESULT_CONTRACT.contains("An empty `file` means the claim is change-wide"));
    assert!(rendered.contains("row's `file` is null and `location_unrecorded` is absent or false"));
    assert!(rendered.contains("When `location_unrecorded` is true"));
    assert!(rendered.contains("re-locate a surviving claim"));
    assert!(rendered.contains("use an empty `file` to report it change-wide"));
    assert!(rendered.contains("re-report it with the same title"));
    assert!(!rendered.contains("confirm it in `disputes`"));
}

#[test]
fn canonical_prior_claims_use_explicit_confirmation() {
    let rendered = ReviewerInputs {
        prior_findings: Some(serde_json::json!([{
            "key": "sha256:claim",
            "file": "src/lib.rs",
            "title": "claim"
        }])),
        finding_identity_policy: Some(review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string()),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(rendered.contains("confirm it in `disputes`"));
    assert!(!rendered.contains("re-report it with the same title"));
}

#[test]
fn v2_prior_findings_require_dispositions_without_duplicate_reports() {
    let rendered = ReviewerInputs {
        result_contract: ReviewerResultContract::V2,
        prior_findings: Some(serde_json::json!({
            "findings": [{
                "finding_id": "sha256:claim",
                "location_unrecorded": true,
                "title": "claim"
            }]
        })),
        finding_identity_policy: Some(review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string()),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(rendered.contains("do not emit a second flat report"));
    assert!(rendered.contains("use its `corroborate` disposition"));
    assert!(!rendered.contains("confirming it only in `disputes`"));
}

/// The prompt must ask for exactly the coverage the kernel requires — the node's own partition —
/// while still saying a peer's wrong claim may be disputed. Asking for the whole union again
/// would restore the R×N disposition cost the partition exists to remove.
#[test]
fn v2_guidance_names_the_required_set_and_permits_disputing_a_peer_finding() {
    let mine = format!("sha256:{}", "a".repeat(64));
    let peer = format!("sha256:{}", "b".repeat(64));
    let rendered = ReviewerInputs {
        result_contract: ReviewerResultContract::V2,
        prior_findings: Some(serde_json::json!({
            "findings": [
                {"finding_id": mine, "title": "mine", "source": "architecture"},
                {"finding_id": peer, "title": "peer", "source": "performance"},
            ]
        })),
        required_finding_ids: Some(vec![mine.clone()]),
        finding_identity_policy: Some(review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string()),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    // The union is still delivered: a Dispute needs the peer's row in front of the reviewer.
    assert!(rendered.contains(&peer));
    assert!(rendered.contains("The Findings listed under `required_dispositions` below are yours"));
    assert!(rendered.contains("Another reviewer owes the rest of this Set"));
    assert!(rendered.contains("you may add a `dispute` entry for one whose claim you find wrong"));
    assert!(!rendered.contains("Every Finding in this exact Set is assigned to you"));
    assert!(rendered.contains(&format!(
        "`required_dispositions`:\n\n```json\n[\"{mine}\"]\n```"
    )));
}

/// No partition supplied means no partition asserted: the reviewer is told it owes everything,
/// which is the conservative obligation the kernel falls back to.
#[test]
fn v2_guidance_without_a_partition_keeps_the_whole_delivered_set_required() {
    let rendered = ReviewerInputs {
        result_contract: ReviewerResultContract::V2,
        prior_findings: Some(serde_json::json!({
            "findings": [{"finding_id": "sha256:claim", "title": "claim"}]
        })),
        finding_identity_policy: Some(review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string()),
        ..ReviewerInputs::default()
    }
    .render()
    .unwrap();

    assert!(rendered.contains("Every Finding in this exact Set is assigned to you"));
    assert!(!rendered.contains("required_dispositions"));
}
