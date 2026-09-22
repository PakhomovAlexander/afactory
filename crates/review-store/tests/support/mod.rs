//! A durable whole-tree Campaign with its first Round started, shared by the store's
//! integration tests. Each test binary uses a subset of it.
#![allow(dead_code)]

use review_core::{
    AuthorityFileV1, CANONICAL_FINDING_IDENTITY_POLICY, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, EventType, LEGACY_FINDING_IDENTITY_POLICY, RoundStartedPayloadV1,
    SubjectKind, SubjectV1,
};
use review_store::{Cas, EventStore, NewEvent};

pub struct Authority {
    pub authority: String,
    pub manifest: String,
    pub subject: String,
    pub head: String,
    pub findings: String,
    pub demands: String,
    pub round_event_id: String,
}

/// A Campaign on the canonical, report-derived Finding identity policy.
pub fn opened_round(store: &mut EventStore, cas: &Cas, run_id: &str) -> Authority {
    opened_round_with_policy(store, cas, run_id, CANONICAL_FINDING_IDENTITY_POLICY)
}

/// A Campaign on the path/title Finding identity policy, which live flat reviewer results use.
pub fn opened_legacy_round(store: &mut EventStore, cas: &Cas, run_id: &str) -> Authority {
    opened_round_with_policy(store, cas, run_id, LEGACY_FINDING_IDENTITY_POLICY)
}

fn opened_round_with_policy(
    store: &mut EventStore,
    cas: &Cas,
    run_id: &str,
    finding_identity_policy: &str,
) -> Authority {
    let authority = cas.put(b"authority").unwrap();
    let pipeline = cas
        .put(
            br#"version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = [{ name = "out", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
runner = { program = "/bin/true" }
"#,
        )
        .unwrap();
    let lock = cas.put(b"lock").unwrap();
    let findings = cas.put(b"finding genesis").unwrap();
    let demands = cas.put(b"demand genesis").unwrap();
    let manifest = cas
        .put_json(
            &serde_json::to_value(CampaignManifestV1 {
                authority_snapshot_id: authority.clone(),
                subject_kind: SubjectKind::WholeTree,
                base_snapshot_id: None,
                pipeline: AuthorityFileV1 {
                    path: "review.toml".into(),
                    artifact_id: pipeline.clone(),
                },
                reviewer_lock: AuthorityFileV1 {
                    path: "review.lock".into(),
                    artifact_id: lock,
                },
                reviewers: vec![],
                execution_policy_ids: vec![pipeline],
                project_policy_ids: vec![],
                convergence: CampaignConvergenceV1 {
                    clean_rounds: 1,
                    max_rounds: 2,
                    gate: "major".into(),
                },
                reviewer_timeout_seconds: 60,
                check_timeout_seconds: 3600,
                git_timeout_seconds: 300,
                budgets: None,
                focus: None,
                finding_identity_policy: finding_identity_policy.into(),
                finding_genesis_id: findings.clone(),
                demand_genesis_id: demands.clone(),
            })
            .unwrap(),
        )
        .unwrap();
    let head = cas.put(b"head snapshot").unwrap();
    let subject = cas
        .put_json(&serde_json::to_value(SubjectV1::whole_tree(&head)).unwrap())
        .unwrap();
    let opened = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                serde_json::to_value(CampaignOpenedPayloadV1 {
                    campaign_manifest_id: manifest.clone(),
                    authority_snapshot_id: authority.clone(),
                })
                .unwrap(),
            )
            .referencing(vec![authority.clone(), manifest.clone()]),
        )
        .unwrap();
    let round = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(RoundStartedPayloadV1 {
                    round: 1,
                    epoch: 1,
                    campaign_manifest_id: manifest.clone(),
                    subject_id: subject.clone(),
                    prior_finding_set_id: findings.clone(),
                    prior_demand_set_id: demands.clone(),
                })
                .unwrap(),
            )
            .caused_by(opened.event_id)
            .referencing(vec![
                authority.clone(),
                manifest.clone(),
                subject.clone(),
                head.clone(),
                findings.clone(),
                demands.clone(),
            ]),
        )
        .unwrap();
    Authority {
        authority,
        manifest,
        subject,
        head,
        findings,
        demands,
        round_event_id: round.event_id,
    }
}

/// Append a bare-status `FindingResolved@1` under the active Round, the event the reducer
/// itself writes when a reviewer contests a claim.
pub fn resolve(
    store: &mut EventStore,
    cas: &Cas,
    run_id: &str,
    round: &Authority,
    key: &str,
    status: review_store::Status,
    note: &str,
) {
    let ledger_round = review_store::LedgerProjection::rebuild(store, cas, run_id)
        .unwrap()
        .ledger()
        .round;
    store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::FindingResolvedV1,
                serde_json::json!({
                    "key": key,
                    "status": status.as_str(),
                    "note": note,
                    "round": ledger_round,
                }),
            )
            .caused_by(round.round_event_id.clone())
            .correlating(key.to_string()),
        )
        .unwrap();
}
