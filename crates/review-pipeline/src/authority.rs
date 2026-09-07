//! Round authority: the immutable publication boundary every event emitted by one Round
//! execution inherits, and the canonical prior-Finding-Set lineage it is resolved from.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use review_core::{
    CampaignManifestV1, CampaignOpenedPayloadV1, EventType, NodeOutputReceiptPayloadV1,
    RoundStartedPayloadV1, SourceSnapshot, run_report_closes_round,
};
use review_source_git::Manifest;
use review_store::{Cas, EventStore};

/// The immutable publication boundary every event emitted by one Round execution inherits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundAuthority {
    pub(crate) run_id: String,
    pub(crate) round_event_id: String,
    pub(crate) round: u32,
    pub(crate) epoch: u32,
    pub(crate) authority_snapshot_id: String,
    pub(crate) campaign_manifest_id: String,
    pub(crate) pipeline_policy_id: String,
    pub(crate) max_rounds: u32,
    pub(crate) subject_id: String,
    pub(crate) head_snapshot_id: String,
    pub(crate) head_content_digest: String,
    pub(crate) prior_finding_set_id: String,
    pub(crate) prior_reduction_finding_set_id: String,
    pub(crate) prior_demand_set_id: String,
    pub(crate) finding_genesis_id: String,
    pub(crate) demand_genesis_id: String,
    pub(crate) finding_identity_policy: String,
    pub(crate) subject_kind: review_core::SubjectKind,
    pub(crate) change_set_id: Option<String>,
    pub(crate) change_set: Option<Arc<review_store::ResolvedChangeSet>>,
    pub(crate) reviewer_packages: BTreeMap<String, (String, String)>,
    pub(crate) policy_ids: Vec<String>,
}

impl RoundAuthority {
    pub fn authority_snapshot_id(&self) -> &str {
        &self.authority_snapshot_id
    }

    /// The validated Change Set this Round reviews, when the Subject is a Diff.
    pub fn change_set(&self) -> Option<&Arc<review_store::ResolvedChangeSet>> {
        self.change_set.as_ref()
    }

    pub fn finding_identity_policy(&self) -> &str {
        &self.finding_identity_policy
    }

    pub fn campaign_manifest_id(&self) -> &str {
        &self.campaign_manifest_id
    }

    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }

    pub fn head_snapshot_id(&self) -> &str {
        &self.head_snapshot_id
    }

    pub fn load(
        store: &EventStore,
        cas: &Cas,
        run_id: &str,
        round_event_id: &str,
    ) -> Result<Self, String> {
        let opened = store
            .campaign_opened(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("Round authority has no CampaignOpened@1")?;
        let opened: CampaignOpenedPayloadV1 =
            serde_json::from_value(opened.payload).map_err(|error| error.to_string())?;
        let round = store
            .latest_round_started(run_id)
            .map_err(|error| error.to_string())?
            .ok_or("Round authority has no RoundStarted@1")?;
        if round.event_id != round_event_id {
            return Err("requested Round is not the active Round epoch".into());
        }
        let payload: RoundStartedPayloadV1 =
            serde_json::from_value(round.payload.clone()).map_err(|error| error.to_string())?;
        if payload.campaign_manifest_id != opened.campaign_manifest_id {
            return Err("RoundStarted@1 does not reference the opened CampaignManifest".into());
        }
        let campaign_manifest: CampaignManifestV1 = serde_json::from_value(
            cas.get_json(&opened.campaign_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        campaign_manifest.validate()?;
        let prior_reduction_finding_set_id = if campaign_manifest.finding_identity_policy
            == review_core::CANONICAL_FINDING_IDENTITY_POLICY
        {
            canonical_prior_finding_set_id(
                store,
                cas,
                run_id,
                round.sequence,
                payload.round,
                &opened.campaign_manifest_id,
                &campaign_manifest,
            )?
        } else {
            payload.prior_finding_set_id.clone()
        };
        let reviewer_packages = campaign_manifest
            .reviewers
            .iter()
            .map(|reviewer| {
                (
                    reviewer.node.clone(),
                    (
                        reviewer.package_artifact_id.clone(),
                        reviewer.digest.clone(),
                    ),
                )
            })
            .collect();
        let mut policy_ids = campaign_manifest.execution_policy_ids.clone();
        policy_ids.extend(campaign_manifest.project_policy_ids.clone());
        policy_ids.sort();
        policy_ids.dedup();
        let resolved = review_store::resolve_subject(cas, &payload.subject_id)
            .map_err(|error| error.to_string())?;
        let subject = resolved.subject;
        let change_set_id = subject.change_set_id.clone();
        let change_set = match (&change_set_id, resolved.change_set) {
            (Some(artifact_id), Some(change_set)) if change_set.artifact_id() == artifact_id => {
                Some(change_set)
            }
            (None, None) => None,
            _ => return Err("resolved Subject has incomplete Change Set authority".into()),
        };
        let source: SourceSnapshot = serde_json::from_value(
            cas.get_json(&subject.head_snapshot_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let source_manifest_id = source
            .artifact_manifest
            .as_deref()
            .ok_or("Round Subject SourceSnapshot has no artifact manifest")?;
        let source_manifest: Manifest = serde_json::from_value(
            cas.get_json(source_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if source_manifest.content_digest() != source.content_digest {
            return Err("Round Subject manifest contradicts its SourceSnapshot".into());
        }
        let mut required = vec![
            opened.authority_snapshot_id.clone(),
            opened.campaign_manifest_id.clone(),
            payload.subject_id.clone(),
            subject.head_snapshot_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        required.extend(subject.base_snapshot_id.clone());
        required.extend(change_set_id.clone());
        for artifact in required {
            cas.verify(&artifact).map_err(|error| error.to_string())?;
            if !round.artifact_refs.contains(&artifact) {
                return Err(format!(
                    "RoundStarted@1 does not publish required authority artifact `{artifact}`"
                ));
            }
        }
        Ok(Self {
            run_id: run_id.to_string(),
            round_event_id: round.event_id,
            round: payload.round,
            epoch: payload.epoch,
            authority_snapshot_id: opened.authority_snapshot_id,
            campaign_manifest_id: payload.campaign_manifest_id,
            pipeline_policy_id: campaign_manifest.pipeline.artifact_id,
            max_rounds: campaign_manifest.convergence.max_rounds,
            subject_id: payload.subject_id,
            head_snapshot_id: subject.head_snapshot_id,
            head_content_digest: source.content_digest,
            prior_finding_set_id: payload.prior_finding_set_id,
            prior_reduction_finding_set_id,
            prior_demand_set_id: payload.prior_demand_set_id,
            finding_genesis_id: campaign_manifest.finding_genesis_id,
            demand_genesis_id: campaign_manifest.demand_genesis_id,
            finding_identity_policy: campaign_manifest.finding_identity_policy,
            subject_kind: subject.kind,
            change_set_id,
            change_set,
            reviewer_packages,
            policy_ids,
        })
    }

    pub fn round_event_id(&self) -> &str {
        &self.round_event_id
    }

    pub fn round(&self) -> u32 {
        self.round
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub(crate) fn artifact_refs(&self) -> Vec<String> {
        let refs = vec![
            self.authority_snapshot_id.clone(),
            self.campaign_manifest_id.clone(),
            self.subject_id.clone(),
            self.head_snapshot_id.clone(),
        ];
        refs
    }
}

fn canonical_prior_finding_set_id(
    store: &EventStore,
    cas: &Cas,
    run_id: &str,
    round_sequence: u64,
    round: u32,
    campaign_manifest_id: &str,
    campaign: &CampaignManifestV1,
) -> Result<String, String> {
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    canonical_prior_finding_set_id_from_events(
        cas,
        &events,
        round_sequence,
        round,
        campaign_manifest_id,
        campaign,
    )
}

fn canonical_prior_finding_set_id_from_events(
    cas: &Cas,
    events: &[review_core::RunEvent],
    active_sequence: u64,
    round: u32,
    campaign_manifest_id: &str,
    campaign: &CampaignManifestV1,
) -> Result<String, String> {
    let pipeline = cas
        .get(&campaign.pipeline.artifact_id)
        .map_err(|error| format!("canonical pipeline authority is unreadable: {error}"))?;
    let pipeline = std::str::from_utf8(&pipeline)
        .map_err(|error| format!("canonical pipeline authority is not UTF-8: {error}"))?;
    let definition = review_config::Definition::from_toml(pipeline)
        .map_err(|error| format!("canonical pipeline authority is invalid: {error}"))?;
    let ledger_nodes: BTreeSet<_> = definition
        .nodes
        .iter()
        .filter(|node| node.kind == review_config::NodeKindSpec::Ledger)
        .map(|node| node.id.as_str())
        .collect();
    if ledger_nodes.is_empty() {
        return Err("canonical pipeline authority has no ledger node".into());
    }
    let prior_rounds: Vec<_> = events
        .iter()
        .rev()
        .filter_map(|event| {
            if event.event_type != EventType::RoundStartedV1 {
                return None;
            }
            let payload: RoundStartedPayloadV1 =
                serde_json::from_value(event.payload.clone()).ok()?;
            (payload.campaign_manifest_id == campaign_manifest_id
                && (payload.round < round
                    || (payload.round == round && event.sequence < active_sequence)))
                .then_some((event.event_id.as_str(), payload.round))
        })
        .collect();
    for (prior_round_event_id, prior_round) in prior_rounds {
        let closed = events.iter().try_fold(false, |closed, event| {
            if event.causation_id.as_deref() != Some(prior_round_event_id)
                || !event.event_type.is_run_report()
            {
                return Ok(closed);
            }
            run_report_closes_round(event)
                .map(|value| closed || value.unwrap_or(false))
                .map_err(|error| error.to_string())
        })?;
        if prior_round < round && !closed {
            continue;
        }
        let mut ids = Vec::new();
        let mut saw_ledger_receipt = false;
        for event in events {
            if event.event_type != EventType::NodeOutputReceiptV1
                || event.causation_id.as_deref() != Some(prior_round_event_id)
            {
                continue;
            }
            let receipt: NodeOutputReceiptPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
            if !ledger_nodes.contains(receipt.node.as_str()) {
                continue;
            }
            saw_ledger_receipt = true;
            for port in receipt.outputs {
                if port.artifact_type == review_core::contract::FINDING_SET_V1 {
                    ids.extend(port.artifact_ids);
                    continue;
                }
                for id in port.artifact_ids {
                    let value = cas.get_json(&id).map_err(|error| {
                        format!("prior ledger output {id} is unreadable: {error}")
                    })?;
                    let Ok(envelope) =
                        serde_json::from_value::<review_core::ArtifactEnvelope>(value)
                    else {
                        continue;
                    };
                    if envelope.artifact_type == review_core::contract::FINDING_SET_V1 {
                        ids.push(id);
                    }
                }
            }
        }
        if ids.is_empty() {
            if saw_ledger_receipt {
                return Err(format!(
                    "canonical Finding Set lineage round {prior_round} has a ledger receipt but no FindingSet@1 output"
                ));
            }
            continue;
        }
        if ids.len() != 1 {
            return Err(format!(
                "canonical Finding Set lineage round {prior_round} has {} ledger outputs",
                ids.len()
            ));
        }
        let id = ids.pop().expect("exactly one canonical Finding Set output");
        let envelope: review_core::ArtifactEnvelope = serde_json::from_value(
            cas.get_json(&id).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("prior Finding Set {id} is not an artifact envelope: {error}"))?;
        review_store::validate_envelope(&envelope).map_err(|error| error.to_string())?;
        if envelope.artifact_type != review_core::contract::FINDING_SET_V1 {
            return Err(format!("prior ledger output {id} is not FindingSet@1"));
        }
        let set: review_core::FindingSetV1 = serde_json::from_value(envelope.payload)
            .map_err(|error| format!("prior Finding Set {id} is invalid: {error}"))?;
        set.validate()?;
        if set.round != prior_round
            || set.round > round
            || set.identity_policy != review_core::CANONICAL_FINDING_IDENTITY_POLICY
        {
            return Err(format!(
                "prior Finding Set {id} disagrees with canonical round {prior_round}"
            ));
        }
        return Ok(id);
    }
    cas.verify(&campaign.finding_genesis_id)
        .map_err(|error| error.to_string())?;
    Ok(campaign.finding_genesis_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::{
        PortArtifactsV1, RunFailureReasonV3, RunNodeOutcomeV2, RunNodeReportV2, RunReportPayloadV3,
        RunVerdictV3, SnapshotAffinity,
    };

    #[test]
    fn canonical_lineage_falls_back_to_genesis_when_a_closed_round_emitted_no_set() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let digest = cas.put(b"genesis").unwrap();
        let pipeline_id = cas
            .put(
                br#"version = 2
[[nodes]]
id = "ledger"
kind = "ledger"
outputs = ["findings"]
"#,
            )
            .unwrap();
        let campaign_manifest_id = format!("sha256:{}", "a".repeat(64));
        let campaign = CampaignManifestV1 {
            authority_snapshot_id: digest.clone(),
            subject_kind: review_core::SubjectKind::WholeTree,
            base_snapshot_id: None,
            pipeline: review_core::AuthorityFileV1 {
                path: "review.toml".into(),
                artifact_id: pipeline_id,
            },
            reviewer_lock: review_core::AuthorityFileV1 {
                path: "review.lock".into(),
                artifact_id: digest.clone(),
            },
            reviewers: Vec::new(),
            execution_policy_ids: vec![digest.clone()],
            project_policy_ids: Vec::new(),
            convergence: review_core::CampaignConvergenceV1 {
                clean_rounds: 1,
                max_rounds: 2,
                gate: "major".into(),
            },
            reviewer_timeout_seconds: 60,
            check_timeout_seconds: Some(3600),
            git_timeout_seconds: Some(300),
            budgets: None,
            focus: None,
            finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            finding_genesis_id: digest.clone(),
            demand_genesis_id: digest,
        };
        let round = review_core::RunEvent {
            event_id: "round-1".into(),
            run_id: "run".into(),
            sequence: 0,
            event_type: EventType::RoundStartedV1,
            occurred_at: "2026-08-26T00:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(RoundStartedPayloadV1 {
                round: 1,
                epoch: 1,
                campaign_manifest_id: campaign_manifest_id.clone(),
                subject_id: format!("sha256:{}", "b".repeat(64)),
                prior_finding_set_id: format!("sha256:{}", "c".repeat(64)),
                prior_demand_set_id: format!("sha256:{}", "d".repeat(64)),
            })
            .unwrap(),
        };
        let terminal = review_core::RunEvent {
            event_id: "report-1".into(),
            run_id: "run".into(),
            sequence: 1,
            event_type: EventType::RunReportV3,
            occurred_at: "2026-08-26T00:00:01Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: Some(round.event_id.clone()),
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(RunReportPayloadV3 {
                outcomes: vec![RunNodeReportV2 {
                    node: "reviewer".into(),
                    outcome: RunNodeOutcomeV2::Failed {
                        error: "run budget exhausted".into(),
                    },
                }],
                blocked_gates: Vec::new(),
                verdict: RunVerdictV3::Fail {
                    reason: RunFailureReasonV3::Exhausted,
                },
                spent_tokens: Some(1_000_000),
            })
            .unwrap(),
        };

        assert_eq!(
            canonical_prior_finding_set_id_from_events(
                &cas,
                &[round, terminal],
                2,
                2,
                &campaign_manifest_id,
                &campaign,
            )
            .unwrap(),
            campaign.finding_genesis_id
        );
    }

    #[test]
    fn canonical_lineage_anchors_a_superseded_epoch_of_the_active_round() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let genesis = cas.put(b"genesis").unwrap();
        let subject = cas.put(b"subject").unwrap();
        let pipeline_id = cas
            .put(
                br#"version = 2
[[nodes]]
id = "ledger"
kind = "ledger"
outputs = [{ name = "set", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
"#,
            )
            .unwrap();
        let campaign_manifest_id = format!("sha256:{}", "a".repeat(64));
        let campaign = CampaignManifestV1 {
            authority_snapshot_id: genesis.clone(),
            subject_kind: review_core::SubjectKind::WholeTree,
            base_snapshot_id: None,
            pipeline: review_core::AuthorityFileV1 {
                path: "review.toml".into(),
                artifact_id: pipeline_id,
            },
            reviewer_lock: review_core::AuthorityFileV1 {
                path: "review.lock".into(),
                artifact_id: genesis.clone(),
            },
            reviewers: Vec::new(),
            execution_policy_ids: vec![genesis.clone()],
            project_policy_ids: Vec::new(),
            convergence: review_core::CampaignConvergenceV1 {
                clean_rounds: 1,
                max_rounds: 2,
                gate: "major".into(),
            },
            reviewer_timeout_seconds: 60,
            check_timeout_seconds: Some(3600),
            git_timeout_seconds: Some(300),
            budgets: None,
            focus: None,
            finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            finding_genesis_id: genesis.clone(),
            demand_genesis_id: genesis.clone(),
        };
        let set = review_core::FindingSetV1 {
            subject_id: subject.clone(),
            round: 1,
            prior_finding_set_id: genesis.clone(),
            reducer_version: review_core::FINDING_REDUCER_VERSION.into(),
            identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            selected_report_ids: Vec::new(),
            relation_ids: Vec::new(),
            resolution_ids: Vec::new(),
            findings: Vec::new(),
        };
        let (set_id, _) = cas
            .put_artifact(
                review_core::contract::FINDING_SET_V1,
                review_core::Producer::KernelOperation {
                    run_id: "run".into(),
                    node_id: Some("ledger".into()),
                    operation_id: "superseded-epoch-test".into(),
                },
                vec![genesis.clone()],
                Some(subject.clone()),
                serde_json::to_value(set).unwrap(),
            )
            .unwrap();
        let round_payload = |epoch| RoundStartedPayloadV1 {
            round: 1,
            epoch,
            campaign_manifest_id: campaign_manifest_id.clone(),
            subject_id: subject.clone(),
            prior_finding_set_id: genesis.clone(),
            prior_demand_set_id: genesis.clone(),
        };
        let superseded_round = review_core::RunEvent {
            event_id: "round-1-epoch-1".into(),
            run_id: "run".into(),
            sequence: 0,
            event_type: EventType::RoundStartedV1,
            occurred_at: "2026-08-26T00:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(round_payload(1)).unwrap(),
        };
        let receipt = review_core::RunEvent {
            event_id: "receipt-1-epoch-1".into(),
            run_id: "run".into(),
            sequence: 1,
            event_type: EventType::NodeOutputReceiptV1,
            occurred_at: "2026-08-26T00:00:01Z".into(),
            node_id: Some("ledger".into()),
            attempt_id: None,
            causation_id: Some(superseded_round.event_id.clone()),
            correlation_id: None,
            artifact_refs: vec![set_id.clone()],
            payload: serde_json::to_value(NodeOutputReceiptPayloadV1 {
                node: "ledger".into(),
                outputs: vec![PortArtifactsV1 {
                    port: "set".into(),
                    artifact_type: review_core::contract::FINDING_SET_V1.into(),
                    cardinality: review_core::PortCardinality::One,
                    optional: false,
                    snapshot_affinity: SnapshotAffinity::Any,
                    artifact_ids: vec![set_id.clone()],
                    subject_snapshot_id: None,
                }],
            })
            .unwrap(),
        };
        let active_round = review_core::RunEvent {
            event_id: "round-1-epoch-2".into(),
            run_id: "run".into(),
            sequence: 2,
            event_type: EventType::RoundStartedV1,
            occurred_at: "2026-08-26T00:00:02Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(round_payload(2)).unwrap(),
        };

        assert_eq!(
            canonical_prior_finding_set_id_from_events(
                &cas,
                &[superseded_round, receipt, active_round],
                2,
                1,
                &campaign_manifest_id,
                &campaign,
            )
            .unwrap(),
            set_id
        );
    }

    #[test]
    fn canonical_lineage_refuses_an_unreadable_untyped_ledger_output() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let genesis = cas.put(b"genesis").unwrap();
        let pipeline_id = cas
            .put(
                br#"version = 2
[[nodes]]
id = "ledger"
kind = "ledger"
outputs = ["findings"]
"#,
            )
            .unwrap();
        let campaign_manifest_id = format!("sha256:{}", "a".repeat(64));
        let campaign = CampaignManifestV1 {
            authority_snapshot_id: genesis.clone(),
            subject_kind: review_core::SubjectKind::WholeTree,
            base_snapshot_id: None,
            pipeline: review_core::AuthorityFileV1 {
                path: "review.toml".into(),
                artifact_id: pipeline_id,
            },
            reviewer_lock: review_core::AuthorityFileV1 {
                path: "review.lock".into(),
                artifact_id: genesis.clone(),
            },
            reviewers: Vec::new(),
            execution_policy_ids: vec![genesis.clone()],
            project_policy_ids: Vec::new(),
            convergence: review_core::CampaignConvergenceV1 {
                clean_rounds: 1,
                max_rounds: 2,
                gate: "major".into(),
            },
            reviewer_timeout_seconds: 60,
            check_timeout_seconds: Some(3600),
            git_timeout_seconds: Some(300),
            budgets: None,
            focus: None,
            finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
            finding_genesis_id: genesis.clone(),
            demand_genesis_id: genesis,
        };
        let round = review_core::RunEvent {
            event_id: "round-1".into(),
            run_id: "run".into(),
            sequence: 0,
            event_type: EventType::RoundStartedV1,
            occurred_at: "2026-08-26T00:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(RoundStartedPayloadV1 {
                round: 1,
                epoch: 1,
                campaign_manifest_id: campaign_manifest_id.clone(),
                subject_id: format!("sha256:{}", "b".repeat(64)),
                prior_finding_set_id: format!("sha256:{}", "c".repeat(64)),
                prior_demand_set_id: format!("sha256:{}", "d".repeat(64)),
            })
            .unwrap(),
        };
        let unreadable_set_id = format!("sha256:{}", "f".repeat(64));
        let hex = unreadable_set_id.strip_prefix("sha256:").unwrap();
        let object = directory
            .path()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]);
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(object, b"corrupt").unwrap();
        let receipt = review_core::RunEvent {
            event_id: "receipt-1".into(),
            run_id: "run".into(),
            sequence: 1,
            event_type: EventType::NodeOutputReceiptV1,
            occurred_at: "2026-08-26T00:00:01Z".into(),
            node_id: Some("ledger".into()),
            attempt_id: None,
            causation_id: Some(round.event_id.clone()),
            correlation_id: None,
            artifact_refs: vec![unreadable_set_id.clone()],
            payload: serde_json::to_value(NodeOutputReceiptPayloadV1 {
                node: "ledger".into(),
                outputs: vec![PortArtifactsV1 {
                    port: "set".into(),
                    artifact_type: review_core::contract::OPAQUE_V1.into(),
                    cardinality: review_core::PortCardinality::One,
                    optional: false,
                    snapshot_affinity: SnapshotAffinity::Any,
                    artifact_ids: vec![unreadable_set_id],
                    subject_snapshot_id: None,
                }],
            })
            .unwrap(),
        };
        let terminal = review_core::RunEvent {
            event_id: "report-1".into(),
            run_id: "run".into(),
            sequence: 2,
            event_type: EventType::RunReportV3,
            occurred_at: "2026-08-26T00:00:02Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: Some(round.event_id.clone()),
            correlation_id: None,
            artifact_refs: Vec::new(),
            payload: serde_json::to_value(RunReportPayloadV3 {
                outcomes: vec![RunNodeReportV2 {
                    node: "ledger".into(),
                    outcome: RunNodeOutcomeV2::Failed {
                        error: "campaign exhausted".into(),
                    },
                }],
                blocked_gates: Vec::new(),
                verdict: RunVerdictV3::Fail {
                    reason: RunFailureReasonV3::Exhausted,
                },
                spent_tokens: Some(1),
            })
            .unwrap(),
        };

        let error = canonical_prior_finding_set_id_from_events(
            &cas,
            &[round, receipt, terminal],
            3,
            2,
            &campaign_manifest_id,
            &campaign,
        )
        .unwrap_err();
        assert!(error.contains("prior ledger output"), "{error}");
        assert!(error.contains("unreadable"), "{error}");
    }
}
