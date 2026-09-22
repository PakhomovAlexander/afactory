use super::*;
pub(super) mod round;
use review_core::task::execution::*;
use review_core::task::review_compat::*;
use review_core::{PortArtifactsV1, PortCardinality, ReviewerResultContract, SnapshotAffinity};

fn fixture() -> Fixture {
    let mut f = Fixture::new(true);
    let result_type = review_core::contract::REVIEWER_RESULT_V1;
    f.revision
        .required_outputs
        .get_mut("document")
        .unwrap()
        .artifact_type = result_type.into();
    f.revision
        .acceptance
        .get_mut("checked")
        .unwrap()
        .evidence_type = result_type.into();
    f.revision_id = f
        .cas
        .put_artifact(
            task::TASK_REVISION_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&f.revision).unwrap(),
        )
        .unwrap()
        .0;
    let mut pipeline: task::pipeline::PipelineDefinitionV1 =
        payload(&f.cas, &f.plan.pipeline_id, PIPELINE_V1).unwrap();
    pipeline
        .contract
        .outputs
        .get_mut("document")
        .unwrap()
        .artifact_type = result_type.into();
    pipeline.slots.get_mut("author").unwrap().output_type = result_type.into();
    let mut side = pipeline.contract.outputs["document"].clone();
    side.artifact_type = TASK_REVIEW_RESULT_METADATA_V1.into();
    side.covers.clear();
    let (id, artifact) = f
        .cas
        .put_artifact(
            PIPELINE_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(pipeline).unwrap(),
        )
        .unwrap();
    f.plan.pipeline_id = id.clone();
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan
        .dependencies
        .get_mut("builtin/document")
        .unwrap()
        .artifact_id = id.clone();
    f.plan
        .dependencies
        .get_mut("builtin/document")
        .unwrap()
        .content_digest = artifact.content_id;
    f.plan.generated_origins[0].pipeline_id = id;
    f.authority.generated = f.plan.generated_origins.clone();
    f.with_execution_graph_outputs(BTreeMap::from([("metadata".into(), side)]))
}

pub(super) fn canonical_context(
    f: &mut Fixture,
    invocation: &str,
    attempt: &str,
) -> TaskReviewContextV1 {
    canonical_context_for_contract(
        f,
        invocation,
        attempt,
        review_core::contract::REVIEWER_RESULT_V1,
    )
}

fn canonical_context_for_contract(
    f: &mut Fixture,
    invocation: &str,
    attempt: &str,
    contract: &str,
) -> TaskReviewContextV1 {
    canonical_context_with_source(f, invocation, attempt, contract, false)
}
pub(in crate::store::task::tests) fn canonical_context_with_source(
    f: &mut Fixture,
    invocation: &str,
    attempt: &str,
    contract: &str,
    real_source: bool,
) -> TaskReviewContextV1 {
    let definition = r#"version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = [{ name = "out", type = "review.kernel/ReviewerResult@1", cardinality = "one", optional = false, snapshot_affinity = "any" }]
runner = { program = "/bin/true" }
"#;
    let definition = if contract == review_core::contract::OPAQUE_V1 {
        definition.replace("[{ name = \"out\", type = \"review.kernel/ReviewerResult@1\", cardinality = \"one\", optional = false, snapshot_affinity = \"any\" }]", "[\"out\"]")
    } else {
        definition.replace(review_core::contract::REVIEWER_RESULT_V1, contract)
    };
    let pipeline = f.cas.put(definition.as_bytes()).unwrap();
    let (authority, head) = if real_source {
        let manifest = f.cas.put_json(&json!({"entries":[]})).unwrap();
        let source = review_core::SourceSnapshot {
            repository_id: "review-source-fixture".into(),
            vcs: review_core::snapshot::Vcs::Git,
            capture: review_core::Capture::Committed {
                tree_id: "a".repeat(40),
            },
            content_digest: manifest.clone(),
            parent_snapshot_id: None,
            source_revision: Some("b".repeat(40)),
            artifact_manifest: Some(manifest),
            submodules: vec![],
        };
        let id = f
            .cas
            .put_json(&serde_json::to_value(source).unwrap())
            .unwrap();
        (id.clone(), id)
    } else {
        (
            f.cas.put(b"review authority").unwrap(),
            f.cas.put(b"review head").unwrap(),
        )
    };
    let facts = f.cas.put_json(&json!({"fixture":"review roots"})).unwrap();
    let subject = f
        .cas
        .put_json(&serde_json::to_value(review_core::SubjectV1::whole_tree(&head)).unwrap())
        .unwrap();
    let manifest = f
        .cas
        .put_json(
            &serde_json::to_value(review_core::CampaignManifestV1 {
                authority_snapshot_id: authority.clone(),
                subject_kind: review_core::SubjectKind::WholeTree,
                base_snapshot_id: None,
                pipeline: review_core::AuthorityFileV1 {
                    path: "pipeline.toml".into(),
                    artifact_id: pipeline.clone(),
                },
                reviewer_lock: review_core::AuthorityFileV1 {
                    path: "reviewers.lock".into(),
                    artifact_id: facts.clone(),
                },
                reviewers: vec![],
                execution_policy_ids: vec![pipeline],
                project_policy_ids: vec![],
                convergence: review_core::CampaignConvergenceV1 {
                    clean_rounds: 1,
                    max_rounds: 1,
                    gate: "major".into(),
                },
                reviewer_timeout_seconds: 60,
                check_timeout_seconds: 3600,
                git_timeout_seconds: 300,
                budgets: None,
                focus: None,
                finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.into(),
                finding_genesis_id: facts.clone(),
                demand_genesis_id: facts.clone(),
            })
            .unwrap(),
        )
        .unwrap();
    let opened = f
        .store
        .append(
            "review-task",
            &f.cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                json!({"authority_snapshot_id":authority,"campaign_manifest_id":manifest}),
            )
            .referencing(vec![authority.clone(), manifest.clone()]),
        )
        .unwrap();
    let round = f
        .store
        .append(
            "review-task",
            &f.cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                json!({"round":1,"epoch":1,"subject_id":subject,"campaign_manifest_id":manifest,
            "prior_finding_set_id":facts,"prior_demand_set_id":facts}),
            )
            .caused_by(opened.event_id)
            .referencing(vec![
                authority,
                manifest.clone(),
                subject.clone(),
                head,
                facts.clone(),
            ]),
        )
        .unwrap();
    let review_invocation = f
        .store
        .append(
            "review-task",
            &f.cas,
            NewEvent::new(
                EventType::NodeInvocationV1,
                json!({"node":"reviewer","inputs":[]}),
            )
            .node("reviewer")
            .caused_by(&round.event_id),
        )
        .unwrap();
    TaskReviewContextV1 {
        campaign_id: "review-task".into(),
        round_event_id: round.event_id,
        invocation_event_id: review_invocation.event_id,
        review_node: "reviewer".into(),
        subject_id: subject,
        campaign_manifest_id: manifest,
        task_invocation_id: invocation.into(),
        attempt_id: attempt.into(),
        reviewer_inputs_id: facts.clone(),
        rendered_input_id: facts.clone(),
        context_manifest_id: facts,
    }
}

pub(super) fn review_output(
    f: &Fixture,
    invocation: &str,
    attempt: &str,
    wrong_metadata: bool,
    context_id: &str,
) -> String {
    review_output_with_proposal(
        f,
        invocation,
        attempt,
        wrong_metadata,
        TaskReviewProposalV1::None {},
        context_id,
    )
}

fn review_output_with_proposal(
    f: &Fixture,
    invocation: &str,
    attempt: &str,
    wrong_metadata: bool,
    proposal: TaskReviewProposalV1,
    context_id: &str,
) -> String {
    review_output_with_provenance(
        f,
        invocation,
        attempt,
        wrong_metadata,
        proposal,
        Some((context_id, 0, "legacy_valid")),
    )
}

fn review_output_with_provenance(
    f: &Fixture,
    invocation: &str,
    attempt: &str,
    wrong_metadata: bool,
    proposal: TaskReviewProposalV1,
    task_context: Option<(&str, u64, &str)>,
) -> String {
    let author = Producer::Attempt {
        run_id: task_run_id("task-1").unwrap(),
        node_id: payload::<TaskInvocationV1>(&f.cas, invocation, TASK_INVOCATION_V1)
            .unwrap()
            .node,
        attempt_id: attempt.into(),
    };
    let result = json!({"verdict":"approve","summary":"checked","reports":[],"benchmark_demands":[],"disputes":[]});
    let result_id = f.cas.put_json(&result).unwrap();
    let result_envelope = f
        .cas
        .put_artifact(
            review_core::contract::REVIEWER_RESULT_V1,
            author.clone(),
            vec![],
            None,
            result,
        )
        .unwrap()
        .0;
    let (context_id, _, corruption) = task_context.unwrap();
    let context: TaskReviewContextV1 = payload(&f.cas, context_id, TASK_REVIEW_CONTEXT_V1).unwrap();
    let raw = f.cas.put(b"historical raw observation").unwrap();
    let mutations = f
        .cas
        .put_json(&json!({"added":[],"modified":[],"deleted":[]}))
        .unwrap();
    let mut legacy = json!({"node":context.review_node,"attempt":attempt,"result_artifact":result_id,
        "cost_tokens":7,"usage":{"chargeable_tokens":7},
        "context_manifest":f.cas.get_json(&context.context_manifest_id).unwrap(),"raw":raw,
        "sandbox_mutations":{"count":0,"added":0,"modified":0,"deleted":0,"sample":[],"truncated":false,"artifact":mutations}});
    match corruption {
        "legacy_null" => legacy = json!(null),
        "legacy_empty" => legacy = json!({}),
        "legacy_attempt" => legacy["attempt"] = json!("z".repeat(26)),
        "legacy_node" => legacy["node"] = json!("different-node"),
        "legacy_result" => legacy["result_artifact"] = json!(raw),
        "legacy_context" => legacy["context_manifest"] = json!({}),
        "legacy_usage" => legacy["usage"]["chargeable_tokens"] = json!(6),
        "legacy_type_removed" => legacy = json!({"context_id":context_id,"charged_tokens":"7"}),
        _ => {}
    }
    let provenance = f.cas.put_json(&legacy).unwrap();
    let provenance = if let Some((context_id, reserved_tokens, corruption)) =
        task_context.filter(|(_, _, corruption)| !corruption.starts_with("legacy_"))
    {
        use review_core::task::usage::{TASK_TOKEN_USAGE_V1, TaskTokenUsageV1};
        let context: TaskReviewContextV1 =
            payload(&f.cas, context_id, TASK_REVIEW_CONTEXT_V1).unwrap();
        let subject: review_core::SubjectV1 =
            serde_json::from_value(f.cas.get_json(&context.subject_id).unwrap()).unwrap();
        let unknown = matches!(corruption, "unknown" | "unknown_charge");
        let usage_id = (!unknown).then(|| {
            f.cas
                .put_artifact(
                    TASK_TOKEN_USAGE_V1,
                    if corruption == "usage_producer" {
                        producer()
                    } else {
                        author.clone()
                    },
                    vec![context_id.into()],
                    None,
                    serde_json::to_value(TaskTokenUsageV1 {
                        input_tokens: Some(u64::MAX.into()),
                        chargeable_tokens: if corruption == "usage_tokens" {
                            8.into()
                        } else {
                            7.into()
                        },
                        ..Default::default()
                    })
                    .unwrap(),
                )
                .unwrap()
                .0
        });
        let mut value = TaskReviewAttemptProvenanceV1 {
            context_id: context_id.into(),
            task_invocation_id: invocation.into(),
            attempt_id: attempt.into(),
            review_node: context.review_node,
            result_artifact_id: result_id.clone(),
            mutations_artifact_id: provenance.clone(),
            raw_artifact_id: provenance.clone(),
            charged_tokens: if unknown {
                reserved_tokens.into()
            } else {
                7.into()
            },
            usage_id,
        };
        match corruption {
            "context" => value.context_id = provenance.clone(),
            "invocation" => value.task_invocation_id = provenance.clone(),
            "attempt" => value.attempt_id = "z".repeat(26),
            "node" => value.review_node = "another-reviewer".into(),
            "result" => value.result_artifact_id = provenance.clone(),
            "unknown_charge" => value.charged_tokens = (reserved_tokens - 1).into(),
            _ => (),
        }
        f.cas
            .put_artifact(
                TASK_REVIEW_ATTEMPT_PROVENANCE_V1,
                if corruption == "producer" {
                    producer()
                } else {
                    author.clone()
                },
                if corruption == "references" {
                    vec![]
                } else {
                    value
                        .artifact_refs()
                        .into_iter()
                        .map(str::to_owned)
                        .collect()
                },
                Some(if corruption == "snapshot" {
                    provenance
                } else {
                    subject.head_snapshot_id
                }),
                serde_json::to_value(value).unwrap(),
            )
            .unwrap()
            .0
    } else {
        provenance
    };
    let metadata = TaskReviewResultMetadataV1 {
        result_contract: ReviewerResultContract::V1,
        result_artifact_id: if wrong_metadata {
            provenance.clone()
        } else {
            result_id
        },
        provenance_artifact_id: provenance,
        proposal,
    };
    let metadata = f
        .cas
        .put_artifact(
            TASK_REVIEW_RESULT_METADATA_V1,
            author.clone(),
            metadata
                .artifact_refs()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            None,
            serde_json::to_value(metadata).unwrap(),
        )
        .unwrap()
        .0;
    let port = |id, artifact_type: &str| task::ArtifactInputV1 {
        artifact_ids: vec![id],
        artifact_type: artifact_type.into(),
        cardinality: PortCardinality::One,
        snapshot_id: None,
    };
    f.cas
        .put_artifact(
            TASK_OUTPUT_V1,
            author,
            vec![invocation.into(), result_envelope.clone(), metadata.clone()],
            None,
            serde_json::to_value(TaskOutputV1 {
                invocation_id: invocation.into(),
                outputs: BTreeMap::from([
                    (
                        "output".into(),
                        port(result_envelope, review_core::contract::REVIEWER_RESULT_V1),
                    ),
                    (
                        "metadata".into(),
                        port(metadata, TASK_REVIEW_RESULT_METADATA_V1),
                    ),
                ]),
            })
            .unwrap(),
        )
        .unwrap()
        .0
}

fn settle(f: &mut Fixture, lease: &TaskLease, attempt: &str, output: &str) {
    settle_charged(f, lease, attempt, output, 7);
}

fn settle_charged(
    f: &mut Fixture,
    lease: &TaskLease,
    attempt: &str,
    output: &str,
    charged_tokens: u64,
) {
    f.store
        .settle_task_attempt(
            &f.cas,
            lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.into(),
                charged_tokens: u128::from(charged_tokens),
                result: TaskAttemptResultV1::Succeeded {
                    output_id: output.into(),
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
}

#[test]
fn typed_review_provenance_binds_exact_attempt_usage_and_unknown_reservation() {
    for corruption in [
        "valid",
        "undercharge",
        "legacy_valid",
        "legacy_null",
        "legacy_empty",
        "legacy_attempt",
        "legacy_node",
        "legacy_result",
        "legacy_context",
        "legacy_usage",
        "legacy_type_removed",
        "unknown",
        "context",
        "invocation",
        "attempt",
        "node",
        "result",
        "producer",
        "usage_producer",
        "usage_tokens",
        "unknown_charge",
        "snapshot",
        "references",
    ] {
        let mut f = fixture();
        let lease = f.open();
        f.propose(&lease);
        f.decide(&lease, PlanDecisionKindV1::Approved);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        let invocation = f.record_execution_inputs(&lease);
        let mut context = canonical_context(&mut f, &invocation, &"a".repeat(26));
        let reserved = f
            .store
            .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
            .unwrap();
        context.attempt_id = reserved.id().into();
        let context_id = f
            .cas
            .put_artifact(
                TASK_REVIEW_CONTEXT_V1,
                producer(),
                context
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(context).unwrap(),
            )
            .unwrap()
            .0;
        let attempt = f
            .store
            .bind_task_attempt_context(&f.cas, &lease, &reserved, &context_id, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let output = review_output_with_provenance(
            &f,
            &invocation,
            attempt.id(),
            false,
            TaskReviewProposalV1::None {},
            Some((&context_id, reserved.reservation().tokens, corruption)),
        );
        let charge = if matches!(corruption, "unknown" | "unknown_charge") {
            reserved.reservation().tokens
        } else if corruption == "undercharge" {
            6
        } else {
            7
        };
        settle_charged(&mut f, &lease, attempt.id(), &output, charge);
        f.store
            .publish_task_output(&f.cas, &lease, &output, Some(attempt.id()), &f.authority)
            .unwrap();
        let selected = f
            .store
            .publish_task_review_result(&f.cas, &lease, &output, &f.authority);
        assert_eq!(
            selected.is_ok(),
            matches!(corruption, "valid" | "unknown" | "legacy_valid"),
            "{corruption}: {selected:?}"
        );
    }
}

#[test]
fn legacy_opaque_review_selection_is_only_the_frozen_v1_contract() {
    for contract in [
        review_core::contract::OPAQUE_V1,
        review_core::contract::REVIEWER_RESULT_V2,
        review_core::contract::FINDING_SET_V1,
    ] {
        let mut f = fixture();
        let lease = f.open();
        f.propose(&lease);
        f.decide(&lease, PlanDecisionKindV1::Approved);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        let invocation = f.record_execution_inputs(&lease);
        let mut context =
            canonical_context_for_contract(&mut f, &invocation, &"a".repeat(26), contract);
        let reserved = f
            .store
            .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
            .unwrap();
        context.attempt_id = reserved.id().into();
        let context_id = f
            .cas
            .put_artifact(
                TASK_REVIEW_CONTEXT_V1,
                producer(),
                context
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(context).unwrap(),
            )
            .unwrap()
            .0;
        let attempt = f
            .store
            .bind_task_attempt_context(&f.cas, &lease, &reserved, &context_id, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let output = review_output(&f, &invocation, attempt.id(), false, &context_id);
        settle(&mut f, &lease, attempt.id(), &output);
        f.store
            .publish_task_output(&f.cas, &lease, &output, Some(attempt.id()), &f.authority)
            .unwrap();
        let selected = f
            .store
            .publish_task_review_result(&f.cas, &lease, &output, &f.authority);
        assert_eq!(
            selected.is_ok(),
            contract == review_core::contract::OPAQUE_V1,
            "{contract}: {selected:?}"
        );
    }
}

#[test]
fn review_selection_requires_common_publication_and_replays_without_legacy_attempts() {
    for has_refusal in [false, true] {
        let mut f = fixture();
        let lease = f.open();
        f.propose(&lease);
        f.decide(&lease, PlanDecisionKindV1::Approved);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        let invocation = f.record_execution_inputs(&lease);
        // Set up canonical evidence before reserving the one-second fixture Attempt.
        let mut context = canonical_context(&mut f, &invocation, &"a".repeat(26));
        let reserved = f
            .store
            .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
            .unwrap();
        context.attempt_id = reserved.id().into();
        let context_id = f
            .cas
            .put_artifact(
                TASK_REVIEW_CONTEXT_V1,
                producer(),
                context
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(&context).unwrap(),
            )
            .unwrap()
            .0;
        let attempt = f
            .store
            .bind_task_attempt_context(&f.cas, &lease, &reserved, &context_id, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let proposal = if has_refusal {
            TaskReviewProposalV1::Refused {
                reason: review_core::ProposalRefusalReasonV1::PatchMismatch,
            }
        } else {
            TaskReviewProposalV1::None {}
        };
        let output = review_output_with_proposal(
            &f,
            &invocation,
            attempt.id(),
            false,
            proposal,
            &context_id,
        );
        assert!(
            f.store
                .publish_task_review_result(&f.cas, &lease, &output, &f.authority)
                .is_err()
        );
        settle(&mut f, &lease, attempt.id(), &output);
        assert!(
            f.store
                .publish_task_review_result(&f.cas, &lease, &output, &f.authority)
                .is_err()
        );
        f.store
            .publish_task_output(&f.cas, &lease, &output, Some(attempt.id()), &f.authority)
            .unwrap();
        f.authority.current = false;
        assert!(
            f.store
                .publish_task_review_result(&f.cas, &lease, &output, &f.authority)
                .is_err()
        );
        f.authority.current = true;
        f.store
            .publish_task_review_result(&f.cas, &lease, &output, &f.authority)
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        f.store
            .publish_task_review_result(&f.cas, &lease, &output, &f.authority)
            .unwrap();
        let events = f.store.replay("review-task").unwrap();
        assert_eq!(events.len(), 4);
        let selected = events.last().unwrap();
        assert_eq!(selected.event_type, EventType::TaskReviewResultSelectedV1);
        assert_eq!(selected.attempt_id.as_deref(), Some(attempt.id()));
        let receipt: TaskReviewResultSelectedV1 =
            serde_json::from_value(selected.payload.clone()).unwrap();
        assert_eq!(receipt.output_id, output);
        assert_eq!(f.state().execution.unwrap().budget.committed_tokens(), 7);
        let before = f.store.len("review-task").unwrap();
        // JSON copied from a real checked event still cannot manufacture append authority.
        assert!(
            f.store
                .append(
                    "review-task",
                    &f.cas,
                    NewEvent::new(selected.event_type, selected.payload.clone())
                        .node("reviewer")
                        .attempt(attempt.id())
                        .caused_by(&context.round_event_id)
                        .referencing(selected.artifact_refs.clone())
                )
                .is_err()
        );
        assert_eq!(f.store.len("review-task").unwrap(), before);
        let opened = f.store.campaign_opened("review-task").unwrap().unwrap();
        let subject: review_core::SubjectV1 =
            serde_json::from_value(f.cas.get_json(&context.subject_id).unwrap()).unwrap();
        let refusal = |reason| {
            NewEvent::new(
                EventType::ProposalRefusedV1,
                serde_json::to_value(review_core::ProposalRefusedPayloadV1 {
                    reason,
                    result_artifact_id: receipt.result_artifact_id.clone(),
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(attempt.id())
            .caused_by(&context.round_event_id)
            .referencing(vec![
                opened.payload["authority_snapshot_id"]
                    .as_str()
                    .unwrap()
                    .into(),
                context.campaign_manifest_id.clone(),
                context.subject_id.clone(),
                subject.head_snapshot_id.clone(),
                receipt.result_artifact_id.clone(),
            ])
        };
        let wrong_disposition = f
            .store
            .append(
                "review-task",
                &f.cas,
                refusal(review_core::ProposalRefusalReasonV1::PathMismatch),
            )
            .unwrap_err();
        assert!(wrong_disposition.to_string().contains("disposition"));
        if has_refusal {
            f.store
                .append(
                    "review-task",
                    &f.cas,
                    refusal(review_core::ProposalRefusalReasonV1::PatchMismatch),
                )
                .unwrap();
        }
        let receipt_event = |id: String| {
            NewEvent::new(
                EventType::NodeOutputReceiptV1,
                serde_json::to_value(review_core::NodeOutputReceiptPayloadV1 {
                    node: "reviewer".into(),
                    outputs: vec![PortArtifactsV1 {
                        port: "out".into(),
                        artifact_type: review_core::contract::REVIEWER_RESULT_V1.into(),
                        cardinality: PortCardinality::One,
                        optional: false,
                        snapshot_affinity: SnapshotAffinity::Any,
                        artifact_ids: vec![id.clone()],
                        subject_snapshot_id: None,
                    }],
                })
                .unwrap(),
            )
            .node("reviewer")
            .attempt(attempt.id())
            .caused_by(&context.round_event_id)
            .referencing(vec![id])
        };
        let wrong = f
        .cas
        .put_json(&json!({"verdict":"approve","summary":"different valid result","reports":[],"benchmark_demands":[],"disputes":[]}))
        .unwrap();
        assert!(
            f.store
                .append("review-task", &f.cas, receipt_event(wrong))
                .is_err()
        );
        f.store
            .append(
                "review-task",
                &f.cas,
                receipt_event(receipt.result_artifact_id),
            )
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        f.store
            .publish_task_review_result(&f.cas, &lease, &output, &f.authority)
            .unwrap();
        assert_eq!(
            f.store.len("review-task").unwrap(),
            before + 1 + u64::from(has_refusal)
        );
        let first = f.store.len("review-task").unwrap();
        let candidate = NewEvent::new(selected.event_type, selected.payload.clone())
            .node("reviewer")
            .attempt(attempt.id())
            .caused_by(&context.round_event_id)
            .referencing(selected.artifact_refs.clone());
        let permit = execution::review::WritePermit::for_checked_selection(
            &f.state(),
            &f.plan,
            "review-task",
            first,
            candidate.clone(),
        )
        .unwrap();
        // The private compare step sees the same common state as the selected-result check.
        {
            let tx = f
                .store
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            permit
                .validate(&tx, "review-task", first as i64, &[candidate.clone()])
                .unwrap();
            assert!(
                permit
                    .validate(&tx, "review-task", first as i64 + 1, &[candidate.clone()])
                    .is_err()
            );
            let mut changed = candidate.clone();
            changed.attempt_id = Some("z".repeat(26));
            assert!(
                permit
                    .validate(&tx, "review-task", first as i64, &[changed])
                    .is_err()
            );
        }
        let mut second = EventStore::open(&f.path).unwrap();
        second.renew_task_lease(&f.cas, &lease, 1_000_000).unwrap();
        {
            let tx = f
                .store
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            assert!(
                permit
                    .validate(&tx, "review-task", first as i64, &[candidate.clone()])
                    .is_err()
            );
        }
        let mut expired = f.plan.clone();
        expired.limits.deadline_unix_ms = 0;
        let expired = execution::review::WritePermit::for_checked_selection(
            &f.state(),
            &expired,
            "review-task",
            first,
            candidate.clone(),
        )
        .unwrap();
        {
            let tx = f
                .store
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            assert!(
                expired
                    .validate(&tx, "review-task", first as i64, &[candidate])
                    .is_err()
            );
        }
        f.store
            .publish_task_review_result(&f.cas, &lease, &output, &f.authority)
            .unwrap();
    }
}

#[test]
fn review_selection_refuses_mismatched_side_metadata_and_context() {
    for corruption in [
        "attempt",
        "metadata",
        "subject",
        "invocation",
        "round",
        "node",
    ] {
        let corrupt_metadata = corruption == "metadata";
        let mut f = fixture();
        let lease = f.open();
        f.propose(&lease);
        f.decide(&lease, PlanDecisionKindV1::Approved);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        let invocation = f.record_execution_inputs(&lease);
        let mut context = canonical_context(&mut f, &invocation, &"a".repeat(26));
        let reserved = f
            .store
            .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
            .unwrap();
        context.attempt_id = reserved.id().into();
        match corruption {
            "attempt" => context.attempt_id = "z".repeat(26),
            "subject" => context.subject_id = context.reviewer_inputs_id.clone(),
            "invocation" => context.invocation_event_id = "z".repeat(26),
            "round" => context.round_event_id = "z".repeat(26),
            "node" => context.review_node = "another-reviewer".into(),
            "metadata" => (),
            _ => unreachable!(),
        }
        let context_id = f
            .cas
            .put_artifact(
                TASK_REVIEW_CONTEXT_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(context).unwrap(),
            )
            .unwrap()
            .0;
        let attempt = f
            .store
            .bind_task_attempt_context(&f.cas, &lease, &reserved, &context_id, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let output = review_output(&f, &invocation, attempt.id(), corrupt_metadata, &context_id);
        settle(&mut f, &lease, attempt.id(), &output);
        f.store
            .publish_task_output(&f.cas, &lease, &output, Some(attempt.id()), &f.authority)
            .unwrap();
        assert!(
            f.store
                .publish_task_review_result(&f.cas, &lease, &output, &f.authority)
                .is_err()
        );
        assert_eq!(f.store.len("review-task").unwrap(), 3);
    }
}
