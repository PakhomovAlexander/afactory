use super::{assert_invalid, assert_valid};
use serde_json::json;

#[test]
fn native_review_conclusion_has_exact_wide_cumulative_charge_and_a_task_prefix() {
    use af_contracts::task::review::*;
    let receipt = TaskReviewConclusionV1 {
        task_revision_id: id('1'),
        plan_id: id('2'),
        task_report_id: id('3'),
        through_sequence: 42,
        cumulative_chargeable_tokens: u128::MAX.into(),
        begun_attempts: 4,
        round_ids: std::collections::BTreeSet::from([id('4')]),
        conclusion: ReviewConclusionV1::Pass,
    };
    receipt.validate().unwrap();
    let value = serde_json::to_value(&receipt).unwrap();
    assert_valid("task-review-conclusion-v1.json", &value);
    assert_eq!(
        serde_json::from_value::<TaskReviewConclusionV1>(value.clone()).unwrap(),
        receipt
    );
    for (field, bad) in [
        ("cumulative_chargeable_tokens", json!(42)),
        ("cumulative_chargeable_tokens", json!("01")),
        (
            "cumulative_chargeable_tokens",
            json!("340282366920938463463374607431768211456"),
        ),
        ("through_sequence", json!(0)),
        ("round_ids", json!([])),
        ("round_ids", json!([id('4'), id('4')])),
    ] {
        let mut changed = value.clone();
        changed[field] = bad;
        assert_invalid("task-review-conclusion-v1.json", &changed, field);
        assert!(
            serde_json::from_value::<TaskReviewConclusionV1>(changed)
                .map_or(true, |v| v.validate().is_err())
        );
    }
}

#[test]
fn native_proposals_integrations_and_commits_have_closed_task_only_shapes() {
    use af_contracts::task::execution::TaskInvocationV1;
    use af_contracts::task::native_review::*;
    use af_contracts::task::pipeline::ReceiptOutcomeV1;
    use std::collections::BTreeMap;
    let invocation = TaskInvocationV1 {
        plan_id: id('1'),
        node: "root.nodes.integrate".into(),
        inputs: BTreeMap::new(),
    };
    let declaration = TaskReviewProposalV1 {
        result_id: id('1'),
        paths: vec!["lib.rs".into()],
        report_indices: vec![0],
        evidence_ids: vec![id('2')],
        auto_apply_nominated: true,
    };
    declaration.validate().unwrap();
    let value = serde_json::to_value(&declaration).unwrap();
    assert_valid("task-review-proposal-v1.json", &value);
    for (field, bad) in [
        ("paths", json!([])),
        ("paths", json!(["lib.rs", "lib.rs"])),
        ("report_indices", json!([])),
        ("report_indices", json!([0, 0])),
        ("evidence_ids", json!(["unknown"])),
    ] {
        let mut changed = value.clone();
        changed[field] = bad;
        assert_invalid("task-review-proposal-v1.json", &changed, field);
        assert!(
            serde_json::from_value::<TaskReviewProposalV1>(changed)
                .map_or(true, |v| v.validate().is_err())
        );
    }
    let selected = TaskReviewSelectedProposalV1 {
        result_id: id('2'),
        declaration_id: id('3'),
        candidate_id: id('4'),
        claims: BTreeMap::from([(id('5'), id('6'))]),
    };
    for (outcome, selection) in [
        (
            ReceiptOutcomeV1::Inconclusive,
            TaskReviewIntegrationSelection::Empty {},
        ),
        (
            ReceiptOutcomeV1::Failed,
            TaskReviewIntegrationSelection::Conflict {
                paths: vec!["lib.rs".into()],
                reason: "overlapping sealed proposals".into(),
            },
        ),
        (
            ReceiptOutcomeV1::Passed,
            TaskReviewIntegrationSelection::Prepared {
                snapshot_id: id('7'),
                manifest_id: id('8'),
                proposals: BTreeMap::from([("correctness".into(), selected)]),
            },
        ),
    ] {
        let receipt = TaskReviewIntegrationV1 {
            invocation: invocation.clone(),
            policy_id: id('9'),
            review_id: id('a'),
            base_snapshot_id: id('b'),
            outcome,
            selection,
        };
        receipt.validate().unwrap();
        let value = serde_json::to_value(&receipt).unwrap();
        assert_valid("task-review-integration-v1.json", &value);
        assert_eq!(
            serde_json::from_value::<TaskReviewIntegrationV1>(value.clone()).unwrap(),
            receipt
        );
        let mut changed = value.clone();
        changed["campaign_id"] = json!("ambient-history");
        assert_invalid(
            "task-review-integration-v1.json",
            &changed,
            "no Campaign authority",
        );
        assert!(serde_json::from_value::<TaskReviewIntegrationV1>(changed).is_err());
        let mut changed = value;
        changed["outcome"] = json!(if outcome == ReceiptOutcomeV1::Passed {
            "failed"
        } else {
            "passed"
        });
        assert_invalid(
            "task-review-integration-v1.json",
            &changed,
            "selection/outcome mismatch",
        );
        assert!(
            serde_json::from_value::<TaskReviewIntegrationV1>(changed)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let commit = TaskReviewCommitV1 {
        invocation,
        integration_id: id('a'),
        snapshot_id: id('b'),
        prior_history_id: id('c'),
        pending_claims: BTreeMap::from([(id('5'), id('6'))]),
        outcome: ReceiptOutcomeV1::Passed,
    };
    commit.validate().unwrap();
    let value = serde_json::to_value(&commit).unwrap();
    assert_valid("task-review-commit-v1.json", &value);
    let mut changed = value;
    changed["pending_claims"] = json!({});
    assert_invalid(
        "task-review-commit-v1.json",
        &changed,
        "promotion retains original claims pending verification",
    );
    assert!(
        serde_json::from_value::<TaskReviewCommitV1>(changed)
            .unwrap()
            .validate()
            .is_err()
    );
}
fn id(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}
