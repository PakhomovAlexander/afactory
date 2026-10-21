//! A whole-tree Campaign with its first Round started, in a Store on disk, whose reviewer
//! results the Store's canonical reduction admits: Findings, a Demand, its Evidence and its
//! satisfaction, and Finding dispositions. Adapted from the Store's own integration fixtures.

use review_core::{
    AuthorityFileV1, CANONICAL_FINDING_IDENTITY_POLICY, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, EventType, Producer, ReviewerStageOutput, RoundStartedPayloadV1,
    SubjectKind, SubjectV1,
};
use review_store::{CanonicalStage, Cas, EventStore, Ingest, NewEvent};

pub const RUN: &str = "campaign-telemetry";

pub struct Round {
    pub subject: String,
    pub head: String,
    pub round_event_id: String,
}

/// Open the Campaign and its first Round.
pub fn opened_round(store: &mut EventStore, cas: &Cas) -> Round {
    let authority = cas.put(b"authority").unwrap();
    let pipeline = cas
        .put(
            br#"version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = [{ name = "out", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "any" }]
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
            RUN,
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
            RUN,
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
                authority,
                manifest,
                subject.clone(),
                head.clone(),
                findings,
                demands,
            ]),
        )
        .unwrap();
    Round {
        subject,
        head,
        round_event_id: round.event_id,
    }
}

pub fn stage(json: &str) -> ReviewerStageOutput {
    serde_json::from_str(json).unwrap()
}

/// Admit flat reviewer answers, `(source, attempt_id, output)`, through the canonical
/// reduction the Campaign runs at its barrier.
pub fn admit(
    store: &mut EventStore,
    cas: &Cas,
    round: &Round,
    results: &[(&str, &str, &ReviewerStageOutput)],
) {
    let result_ids: Vec<String> = results
        .iter()
        .map(|(source, attempt_id, output)| {
            cas.put_artifact(
                review_core::contract::REVIEWER_RESULT_V2,
                Producer::Attempt {
                    run_id: RUN.into(),
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
        })
        .collect();
    let mut ingest = Ingest::new(store, cas, RUN)
        .unwrap()
        .under_round(&round.round_event_id);
    ingest.add_canonical_stage_outputs(&stages).unwrap();
}

/// A bare-status `FindingResolved@1` under the active Round, as the reducer writes a contested
/// claim and the Store's fixtures stage other dispositions.
pub fn resolve(store: &mut EventStore, cas: &Cas, round: &Round, key: &str, status: &str) {
    let ledger_round = review_store::LedgerProjection::rebuild(store, cas, RUN)
        .unwrap()
        .ledger()
        .round;
    store
        .append(
            RUN,
            cas,
            NewEvent::new(
                EventType::FindingResolvedV1,
                serde_json::json!({
                    "key": key,
                    "status": status,
                    "note": "fixture disposition",
                    "round": ledger_round,
                }),
            )
            .caused_by(round.round_event_id.clone())
            .correlating(key.to_string()),
        )
        .unwrap();
}

/// The Campaign's Findings by title and its Demands, as the Ledger holds them now.
pub fn ledger(store: &EventStore, cas: &Cas) -> review_store::Ledger {
    review_store::LedgerProjection::rebuild(store, cas, RUN)
        .unwrap()
        .into_ledger()
}
