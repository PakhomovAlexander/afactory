//! A durable whole-tree Campaign with its first Round started, shared by the store's
//! integration tests. Each test binary uses a subset of it.
#![allow(dead_code)]

use review_core::{
    AuthorityFileV1, CANONICAL_FINDING_IDENTITY_POLICY, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, EventType, LegacyStageOutput, Producer, RoundStartedPayloadV1,
    SubjectKind, SubjectV1,
};
use review_store::{
    CanonicalReduction, CanonicalStage, Cas, EventStore, Ingest, NewEvent, StoreError,
};

pub struct Authority {
    pub authority: String,
    pub manifest: String,
    pub subject: String,
    pub head: String,
    pub findings: String,
    pub demands: String,
    pub round_event_id: String,
}

/// A Campaign on the canonical, report-derived Finding identity policy, the only policy this
/// release records.
pub fn opened_round(store: &mut EventStore, cas: &Cas, run_id: &str) -> Authority {
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
                finding_identity_policy: CANONICAL_FINDING_IDENTITY_POLICY.into(),
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

/// Admit flat `ReviewerResult@1` answers, given as `(source, attempt_id, output)`, through the
/// canonical reduction a Campaign runs at its barrier. Each answer is first published as its
/// Attempt's `ReviewerResult@1` envelope, so every Report carries that Attempt's provenance.
pub fn add_flat_results(
    ingest: &mut Ingest<'_>,
    cas: &Cas,
    run_id: &str,
    round: &Authority,
    results: &[(&str, &str, &LegacyStageOutput)],
) -> Result<CanonicalReduction, StoreError> {
    let result_ids: Vec<String> = results
        .iter()
        .map(|(source, attempt_id, output)| {
            cas.put_artifact(
                review_core::contract::REVIEWER_RESULT_V1,
                Producer::Attempt {
                    run_id: run_id.into(),
                    node_id: (*source).into(),
                    attempt_id: (*attempt_id).into(),
                },
                Vec::new(),
                Some(round.head.clone()),
                serde_json::to_value(output).unwrap(),
            )
            .unwrap()
            .0
        })
        .collect();
    let stages: Vec<CanonicalStage<'_>> = results
        .iter()
        .zip(&result_ids)
        .map(|((source, attempt_id, output), result_id)| CanonicalStage {
            source,
            demand_requirement: review_core::DemandRequirement::Required,
            stage: output,
            attempt_id,
            result_artifact_id: result_id,
            input_artifacts: &[],
            subject_snapshot_id: &round.head,
            subject_id: &round.subject,
            result_contract: review_core::ReviewerResultContract::V1,
        })
        .collect();
    ingest.add_canonical_stage_outputs(&stages)
}

/// Append a bare-status `FindingResolved@1` under the active Round. It sets `status` directly,
/// which is why tests use it to stage a Finding. On the live path the reducer writes this shape
/// only as `contested`, when a reviewer disputes a claim; operator decisions are typed
/// Resolutions (`FindingResolutionRecorded@1`, `FixVerified@1`) recorded through [`Ingest`].
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
