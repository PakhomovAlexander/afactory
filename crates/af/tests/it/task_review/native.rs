//! Native entrypoints: these fixtures build no Campaign, RoundStarted event or translation host.
use super::*;
#[path = "../../../../af-runtime/tests/it/support/schema_registry.rs"]
mod schema_registry;
use af_contracts::task::native_review::*;
use af_contracts::{ShardOutcomeV1, ShardSetV1};
use af_store::store::task::TaskProjection;

#[test]
fn native_reviewers_refuse_the_free_disk_floor_without_starting_or_charging_an_attempt() {
    const CHILD: &str = "AF_NATIVE_REVIEW_FLOOR_TEST";
    if std::env::var_os(CHILD).is_some() {
        run_case("native_floor");
        return;
    }
    // The installed StorageHost is process-global. Keep this fake in an isolated child of the
    // existing test binary, so ordinary cargo test parallelism cannot affect another case.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "task_review::native::native_reviewers_refuse_the_free_disk_floor_without_starting_or_charging_an_attempt", "--nocapture"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct RefuseNativeWorkerFloor;
static FLOOR_BLOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub(super) fn free_floor() {
    FLOOR_BLOCKED.store(false, std::sync::atomic::Ordering::Release);
}
impl af_runtime::storage::StorageHost for RefuseNativeWorkerFloor {
    fn ensure_free_disk(&self) -> Result<(), String> {
        if FLOOR_BLOCKED.load(std::sync::atomic::Ordering::Acquire) {
            Err("insufficient_disk: native fixture floor".into())
        } else {
            Ok(())
        }
    }
    fn before_new_warm_key(&self) {}
}

#[test]
fn disjoint_native_proposals_promote_only_after_checks_and_keep_original_claims_pending() {
    run_case("native_integration");
}
#[test]
fn overlapping_native_proposals_record_conflict_without_post_apply_checks_or_promotion() {
    run_case("native_integration_overlap");
}
#[test]
fn protected_native_proposals_record_conflict_without_post_apply_checks_or_promotion() {
    run_case("native_integration_protected");
}
#[test]
fn native_proposal_declaration_must_equal_the_entire_sealed_diff() {
    run_case("native_integration_wrong_paths");
}
#[test]
fn failed_native_post_apply_checks_cannot_publish_a_promoted_source() {
    run_case("native_integration_failed_checks");
}
#[test]
fn native_scatter_runs_real_owned_attempts_and_closes_the_whole_subject() {
    run_case("native_scatter");
}
#[test]
fn failed_native_shards_remain_in_the_complete_set_and_refuse_a_partial_ledger() {
    run_case("native_scatter_failed");
}
#[test]
fn native_scatter_refuses_a_closeout_that_does_not_read_every_shard_set() {
    run_case("native_scatter_no_closeout");
}
#[test]
fn native_slicing_refuses_subjects_above_the_exact_fanout_bound() {
    run_case("native_scatter_bound");
}

#[test]
fn unstarted_native_shards_remain_missing_without_spending_reserved_closeout_attempts() {
    run_case("native_scatter_missing");
}
#[test]
fn native_conclusions_freeze_exact_wide_usage_and_late_usage_fences_the_old_result() {
    run_case("native_accounting");
}

#[test]
fn native_scatter_refuses_a_plan_above_the_shared_canonical_stage_bound() {
    run_case("native_scatter_stage_bound");
}
#[test]
fn native_closeout_cannot_be_a_slice_scoped_worker() {
    run_case("native_scatter_sliced_closeout");
}
#[test]
fn native_proposals_cannot_substitute_an_unselected_reviewer_result() {
    run_case("native_integration_wrong_result");
}

#[test]
fn identical_native_sealed_edits_are_applied_once_with_both_original_claims() {
    run_case("native_integration_duplicate");
}
#[test]
fn native_integration_refuses_a_plan_without_a_full_successor() {
    run_case("native_integration_no_successor");
}
#[test]
fn native_integration_refuses_an_unrelated_successor_history() {
    run_case("native_integration_unrelated_successor");
}
#[test]
fn native_integration_incomplete_successor_cannot_accept_the_initial_round() {
    run_case("native_integration_incomplete_successor");
}
#[test]
fn native_scatter_supplies_each_childs_exact_diff_subject_and_replays_it() {
    run_case("native_scatter_diff");
}
#[test]
fn implementation_accepts_an_embedded_complete_sharded_review() {
    run_case("native_scatter_embedded_implementation");
}
#[test]
fn verification_accepts_an_embedded_complete_sharded_review() {
    run_case("native_scatter_embedded_verification");
}
#[test]
fn embedded_sharded_review_cannot_accept_a_failed_shard() {
    run_case("native_scatter_embedded_failed");
}

#[test]
fn identical_native_edits_recover_without_repeating_the_application_or_checks() {
    run_case("native_integration_duplicate_recover");
}
#[test]
fn native_scatter_diff_subjects_survive_writer_replacement_without_new_child_attempts() {
    run_case("native_scatter_diff_recover");
}
#[test]
fn embedded_sharded_review_cannot_omit_whole_subject_closeout() {
    run_case("native_scatter_embedded_no_closeout");
}
#[test]
fn embedded_sharded_review_cannot_accept_a_missing_shard() {
    run_case("native_scatter_embedded_missing");
}

#[test]
fn embedded_implementation_requires_the_exact_integrated_successor_round() {
    run_case("native_integration_embedded_implementation");
}
#[test]
fn embedded_implementation_cannot_accept_an_incomplete_integrated_successor() {
    run_case("native_integration_embedded_incomplete");
}
#[test]
fn embedded_integration_refuses_a_plan_covering_only_the_initial_round() {
    run_case("native_integration_embedded_no_successor");
}
#[test]
fn embedded_integrated_successor_recovers_without_repeating_the_commit_or_checks() {
    run_case("native_integration_embedded_implementation_recover");
}

#[test]
fn native_integration_refuses_a_successor_used_only_as_a_select_condition() {
    run_case("native_integration_condition_only");
}
#[test]
fn embedded_integration_refuses_a_successor_used_only_as_a_select_condition() {
    run_case("native_integration_embedded_condition_only");
}

#[test]
fn native_successor_blocking_findings_remain_the_selected_verdict() {
    run_case("native_integration_blocking");
}
#[test]
fn embedded_successor_blocking_findings_refuse_verified_acceptance() {
    run_case("native_integration_embedded_blocking");
}
#[test]
fn native_integration_rejects_a_select_that_actually_discards_its_failed_successor() {
    run_case("native_integration_selected_old");
}
#[test]
fn embedded_integration_rejects_a_select_that_actually_discards_its_failed_successor() {
    run_case("native_integration_embedded_selected_old");
}

#[test]
fn native_successor_retains_the_canonical_convergence_exhausted_verdict() {
    run_case("native_integration_blocking_exhausted");
}

#[test]
fn a_selected_later_round_preserves_the_exact_committed_successor_history() {
    run_case("native_integration_later");
}
#[test]
fn an_embedded_later_round_preserves_the_exact_committed_successor_history() {
    run_case("native_integration_embedded_later");
}

#[test]
fn later_round_cannot_return_to_the_original_source_after_integration() {
    run_case("native_integration_original_later");
}
#[test]
fn embedded_later_round_cannot_return_to_the_original_source_after_integration() {
    run_case("native_integration_embedded_original_later");
}
#[test]
fn later_round_cannot_select_a_sibling_of_the_committed_source() {
    run_case("native_integration_sibling_later");
}
#[test]
fn embedded_later_round_cannot_select_a_sibling_of_the_committed_source() {
    run_case("native_integration_embedded_sibling_later");
}

pub(super) fn rejects_later_source(case: &str) -> bool {
    case.contains("_original_later") || case.contains("_sibling_later")
}

fn later_source(case: &str) -> ValueRefV1 {
    if case.contains("_original_later") {
        if case.contains("_static_") {
            root("source")
        } else {
            from("later_source", "output")
        }
    } else if case.contains("_sibling_later") {
        root("sibling")
    } else if case.contains("_descendant_later") {
        from("advance_seal", "snapshot")
    } else {
        from("unpromoted", "source")
    }
}

fn later_checks(case: &str) -> ValueRefV1 {
    if case.contains("_original_later") {
        if case.contains("_static_") {
            from("check", "result")
        } else {
            from("later_checks", "output")
        }
    } else if case.contains("_sibling_later") {
        from("sibling_check", "result")
    } else if case.contains("_descendant_later") {
        from("advance_check", "result")
    } else {
        from("post_check", "result")
    }
}

#[test]
fn no_commit_transports_an_admitted_source_from_another_tasks_kernel_node() {
    run_case("native_integration_transport_protected");
}
#[test]
fn embedded_no_commit_keeps_its_exact_review_accept_snapshot_for_an_imported_source() {
    run_case("native_integration_embedded_transport_protected");
}

#[test]
fn a_later_round_accepts_an_admitted_checked_descendant_and_returns_its_exact_source() {
    run_case("native_integration_descendant_later");
}
#[test]
fn embedded_later_acceptance_returns_the_exact_admitted_checked_descendant() {
    run_case("native_integration_embedded_descendant_later");
}

#[test]
fn native_integration_rejects_a_statically_backwards_later_source_route() {
    run_case("native_integration_static_original_later");
}
#[test]
fn embedded_integration_rejects_a_statically_backwards_later_source_route() {
    run_case("native_integration_embedded_static_original_later");
}
#[test]
fn native_acceptance_refuses_a_public_snapshot_older_than_its_selected_round() {
    run_case("native_integration_mismatched_snapshot_later");
}

fn conditional(n: &mut TaskNodeV1, node: &str) {
    n.when = Some(NodeConditionV1 {
        node: node.into(),
        outcome: ReceiptOutcomeV1::Passed,
    });
}

pub(super) fn code(case: &str, policy: &mut CodeTaskPolicy) {
    if case == "native_integration_failed_checks" {
        policy.checks.get_mut("syntax").unwrap().command = af_contracts::Command::new(
            "/usr/bin/python3",
            vec![
                af_contracts::Arg::literal("-c"),
                af_contracts::Arg::literal(
                    "import pathlib; raise SystemExit(int(b'invalid' in pathlib.Path('lib.rs').read_bytes()))",
                ),
            ],
        );
    }
}

pub(super) fn configure(
    case: &str,
    pipeline: &mut PipelineDefinition,
    packages: &mut [(String, BTreeMap<String, Vec<u8>>)],
) {
    if case.starts_with("native_scatter") {
        configure_scatter(case, pipeline, packages);
    }
}

pub(super) fn configure_integration(
    case: &str,
    pipeline: &mut PipelineDefinition,
    packages: &mut Vec<(String, BTreeMap<String, Vec<u8>>)>,
) {
    if !case.starts_with("native_integration") {
        return;
    }
    for name in ["bugs", "correctness"] {
        let files = &mut packages
            .iter_mut()
            .find(|(n, _)| n == &format!("fixture/{name}"))
            .unwrap()
            .1;
        let path = if name == "bugs" { "other.rs" } else { "lib.rs" };
        let script = format!(
            r#"import json,sys
r=json.load(sys.stdin)
a=r['inputs']['assignment'][0]['payload']
s=r['inputs']['subject'][0]['payload']
report={{'severity':'minor','file':{path:?},'line':1,'title':'Native claim '+{name:?},'body':'The source needs the declared repair','fix':'Apply the exact proposed edit','confidence':1.0}}
stage={{'reports':[report] if s['round']==1 else [],'benchmark_demands':[],'dispositions':[{{'finding_id':f['finding_id'],'position':'not_reproduced','reason':'Checked the entire current Subject'}} for f in a['findings']]}}
if s['round']==2 and {name:?}=='correctness' and ('_blocking' in {case:?} or {case:?}.endswith('selected_old') or {case:?}.endswith('condition_only')):
    stage['reports']=[dict(report,severity='major',title='New blocker on the committed head')]
print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'result':[stage]}}}}))
"#
        );
        files.insert("worker.py".into(), script.into_bytes());
    }
    let mut preparation = BTreeMap::from([
        ("source".into(), root("source")),
        ("review".into(), from("reduce", "result")),
    ]);
    for name in ["bugs", "correctness"] {
        let slot = format!("propose_{name}");
        let package = format!("fixture/{slot}");
        let inputs = BTreeMap::from([
            (
                "source".into(),
                port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
            ),
            ("review".into(), port(TASK_REVIEW_ROUND_V1, same())),
            (
                "result".into(),
                port(af_contracts::contract::REVIEWER_RESULT_V2, same()),
            ),
        ]);
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: inputs.clone(),
                outputs: BTreeMap::from([
                    (
                        "candidate".into(),
                        port(af_source_git::task::CANDIDATE_TREE_V1, same()),
                    ),
                    ("proposal".into(), port(TASK_REVIEW_PROPOSAL_V1, same())),
                ]),
            },
            effects: BTreeSet::from(["read-source".into(), "write-source".into()]),
            evidence: BTreeMap::new(),
            retains: BTreeMap::from([("proposal".into(), inputs.keys().cloned().collect())]),
            roles: BTreeSet::from(["implement".into()]),
            worker_input_type: Some("af/ProposalInput@1".into()),
            worker_output_type: Some(TASK_REVIEW_PROPOSAL_V1.into()),
            outcome_port: None,
            attempt: Some(OperatorAttemptCost {
                tokens: 0,
                wall_ms: 5000,
            }),
            unbound_allowed: BTreeSet::new(),
        };
        let worker = TaskWorkerManifest {schema:"af.worker/1".into(), name:package.clone(), version:"1.0.0".into(), signature,
            runner: TaskWorkerRunner::Command {command:serde_json::from_value(json!({"program":"/usr/bin/python3","args":[{"value":"-B","provenance":"literal"},{"value":"@package/worker.py","provenance":"literal"}]})).unwrap()}};
        let path = if name == "bugs"
            && !matches!(
                case,
                "native_integration_overlap" | "native_integration_duplicate"
            ) {
            "other.rs"
        } else {
            "lib.rs"
        };
        let declared = if case == "native_integration_wrong_paths" && name == "bugs" {
            "lib.rs"
        } else {
            path
        };
        let contents = if case == "native_integration_overlap" && name == "correctness" {
            "distinct repair\n"
        } else if case == "native_integration_failed_checks" {
            "invalid\n"
        } else {
            "repaired\n"
        };
        let mut script = format!(
            r#"import json,sys,pathlib,os
r=json.load(sys.stdin)
assert len(r['inputs']['result'][0]['payload']['reports'])==1
pathlib.Path({path:?}).write_text({contents:?})
p={{'result_id':r['inputs']['review'][0]['payload']['selected_results'][{name:?}],'paths':[{declared:?}],'report_indices':[0],'evidence_ids':[],'auto_apply_nominated':True}}
if {case:?}=='native_integration_manifest_changes':
    if {name:?}=='bugs':
        pathlib.Path('other.rs').unlink()
        pathlib.Path('new.rs').write_bytes(b'added\n')
        p['paths']=['new.rs','other.rs']
    else: os.chmod('lib.rs',0o755)
print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'proposal':[p]}}}}))
"#
        );
        if case == "native_integration_wrong_result" && name == "bugs" {
            script = script.replace(
                "r['inputs']['review'][0]['payload']['selected_results'][\"bugs\"]",
                "'sha256:0000000000000000000000000000000000000000000000000000000000000000'",
            );
        }
        packages.push((package.clone(), BTreeMap::from([
            ("worker.toml".into(), toml::to_string(&worker).unwrap().into_bytes()),
            ("worker.py".into(), script.into_bytes()),
            ("input.schema.json".into(), serde_json::to_vec(&json!({"type":"object","required":["source","review","result"],"additionalProperties":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"object"}}})).unwrap()),
            ("outputs/proposal.schema.json".into(), serde_json::to_vec(&json!({"type":"object","required":["result_id","paths","report_indices","evidence_ids","auto_apply_nominated"],"additionalProperties":false,"properties":{"result_id":{"type":"string"},"paths":{"type":"array"},"report_indices":{"type":"array"},"evidence_ids":{"type":"array"},"auto_apply_nominated":{"type":"boolean"}}})).unwrap()),
        ])));
        pipeline.slots.insert(
            slot.clone(),
            WorkerSlotV1 {
                worker: package,
                role: "implement".into(),
                input_type: "af/ProposalInput@1".into(),
                output_type: TASK_REVIEW_PROPOSAL_V1.into(),
                min_attempts: 1,
                max_attempts: 1,
                allow_local_replacement: false,
                independent_from: BTreeSet::new(),
                recovery_attempts: None,
            },
        );
        let mut author = node(
            &slot,
            TaskOperatorV1::Worker { slot: slot.clone() },
            BTreeMap::from([
                ("source".into(), root("source")),
                ("review".into(), from("reduce", "result")),
                ("result".into(), from(name, "result")),
            ]),
        );
        conditional(&mut author, "check");
        pipeline.nodes.push(author);
        preparation.insert(format!("candidate_{name}"), from(&slot, "candidate"));
        preparation.insert(format!("proposal_{name}"), from(&slot, "proposal"));
    }
    pipeline.nodes.push(node(
        "prepare",
        TaskOperatorV1::ReviewIntegrate {
            reviewers: BTreeSet::from(["bugs".into(), "correctness".into()]),
            protected_paths: if case.ends_with("protected") {
                vec!["lib.rs".into()]
            } else {
                vec![]
            },
        },
        preparation,
    ));
    let mut source = node(
        "unpromoted",
        TaskOperatorV1::ReviewIntegrationSource {},
        BTreeMap::from([
            ("source".into(), root("source")),
            ("integration".into(), from("prepare", "result")),
        ]),
    );
    conditional(&mut source, "prepare");
    pipeline.nodes.push(source);
    let mut checks = node(
        "post_check",
        TaskOperatorV1::Check {
            checks: BTreeSet::from(["syntax".into()]),
            remote_checks: BTreeSet::new(),
        },
        BTreeMap::from([("source".into(), from("unpromoted", "source"))]),
    );
    conditional(&mut checks, "prepare");
    pipeline.nodes.push(checks);
    let mut commit = node(
        "commit",
        TaskOperatorV1::ReviewCommit {},
        BTreeMap::from([
            ("source".into(), from("unpromoted", "source")),
            ("integration".into(), from("prepare", "result")),
            ("checks".into(), from("post_check", "result")),
        ]),
    );
    conditional(&mut commit, "prepare");
    pipeline.nodes.push(commit);
    let mut bind = node(
        "integrated_bind",
        TaskOperatorV1::ReviewBindIntegrated {},
        BTreeMap::from([
            ("source".into(), from("unpromoted", "source")),
            ("history".into(), from("commit", "history")),
            ("integration".into(), from("commit", "result")),
        ]),
    );
    conditional(&mut bind, "prepare");
    pipeline.nodes.push(bind);
    for name in ["bugs", "correctness"] {
        let mut reviewer = node(
            &format!("integrated_{name}"),
            TaskOperatorV1::Verify { slot: name.into() },
            BTreeMap::from([
                ("source".into(), from("unpromoted", "source")),
                ("subject".into(), from("integrated_bind", "subject")),
                ("assignment".into(), from("integrated_bind", name)),
                ("history".into(), from("commit", "history")),
                ("checks".into(), from("post_check", "result")),
            ]),
        );
        conditional(&mut reviewer, "prepare");
        pipeline.nodes.push(reviewer);
    }
    let mut reduce = node(
        "integrated_reduce",
        TaskOperatorV1::ReviewReduce {},
        BTreeMap::from([
            ("source".into(), from("unpromoted", "source")),
            ("subject".into(), from("integrated_bind", "subject")),
            ("history".into(), from("commit", "history")),
            ("checks".into(), from("post_check", "result")),
            ("bugs".into(), from("integrated_bugs", "result")),
            (
                "correctness".into(),
                from("integrated_correctness", "result"),
            ),
        ]),
    );
    conditional(&mut reduce, "prepare");
    pipeline.nodes.push(reduce);
    if case.contains("_descendant_later") {
        let source_port = port(SOURCE_TREE_V1, PortAffinityV1::Unbound {});
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([("source".into(), source_port)]),
                outputs: BTreeMap::from([
                    (
                        "candidate".into(),
                        port(af_source_git::task::CANDIDATE_TREE_V1, same()),
                    ),
                    ("result".into(), port("af/AdvanceOutput@1", same())),
                ]),
            },
            effects: BTreeSet::from(["read-source".into(), "write-source".into()]),
            evidence: BTreeMap::new(),
            retains: BTreeMap::new(),
            roles: BTreeSet::from(["implement".into()]),
            worker_input_type: Some("af/AdvanceInput@1".into()),
            worker_output_type: Some("af/AdvanceOutput@1".into()),
            outcome_port: None,
            attempt: Some(OperatorAttemptCost {
                tokens: 0,
                wall_ms: 5000,
            }),
            unbound_allowed: BTreeSet::new(),
        };
        let worker = TaskWorkerManifest {schema: "af.worker/1".into(), name: "fixture/advance".into(), version: "1.0.0".into(), signature,
            runner: TaskWorkerRunner::Command {command: serde_json::from_value(json!({"program":"/usr/bin/python3", "args":[{"value":"-B", "provenance":"literal"},{"value":"@package/worker.py", "provenance":"literal"}]})).unwrap()}};
        packages.push(("fixture/advance".into(), BTreeMap::from([
            ("worker.toml".into(), toml::to_string(&worker).unwrap().into_bytes()),
            ("worker.py".into(), b"import pathlib,json\npathlib.Path('lib.rs').write_text('checked descendant\\n')\nprint(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'advanced':True}]}}))\n".to_vec()),
            ("outputs/result.schema.json".into(), br#"{"type":"object","required":["advanced"],"additionalProperties":false,"properties":{"advanced":{"const":true}}}"#.to_vec()),
            ("input.schema.json".into(), br#"{"type":"object","required":["source"],"additionalProperties":false,"properties":{"source":{"type":"array","minItems":1,"maxItems":1}}}"#.to_vec()),
        ])));
        pipeline.slots.insert(
            "advance".into(),
            WorkerSlotV1 {
                worker: "fixture/advance".into(),
                role: "implement".into(),
                input_type: "af/AdvanceInput@1".into(),
                output_type: "af/AdvanceOutput@1".into(),
                min_attempts: 1,
                max_attempts: 1,
                allow_local_replacement: false,
                independent_from: BTreeSet::new(),
                recovery_attempts: None,
            },
        );
        let mut author = node(
            "advance",
            TaskOperatorV1::Worker {
                slot: "advance".into(),
            },
            BTreeMap::from([("source".into(), from("unpromoted", "source"))]),
        );
        conditional(&mut author, "prepare");
        pipeline.nodes.push(author);
        pipeline.nodes.push(node(
            "advance_seal",
            TaskOperatorV1::Seal {},
            BTreeMap::from([("candidate".into(), from("advance", "candidate"))]),
        ));
        pipeline.nodes.push(node(
            "advance_check",
            TaskOperatorV1::Check {
                checks: BTreeSet::from(["syntax".into()]),
                remote_checks: BTreeSet::new(),
            },
            BTreeMap::from([("source".into(), from("advance_seal", "snapshot"))]),
        ));
    }
    if case.contains("_original_later") && !case.contains("_static_") {
        pipeline.nodes.push(node(
            "later_source",
            TaskOperatorV1::Select {},
            BTreeMap::from([
                ("condition".into(), from("check", "result")),
                ("passed".into(), root("source")),
                ("failed".into(), from("unpromoted", "source")),
                ("inconclusive".into(), from("unpromoted", "source")),
            ]),
        ));
    }
    if case.contains("_original_later") && !case.contains("_static_") {
        pipeline.nodes.push(node(
            "later_checks",
            TaskOperatorV1::Select {},
            BTreeMap::from([
                ("condition".into(), from("check", "result")),
                ("passed".into(), from("check", "result")),
                ("failed".into(), from("post_check", "result")),
                ("inconclusive".into(), from("post_check", "result")),
            ]),
        ));
    }
    for n in &mut pipeline.nodes {
        if matches!(
            n.id.as_str(),
            "advance_seal" | "advance_check" | "later_source" | "later_checks"
        ) {
            conditional(n, "prepare");
        }
    }
    if case.ends_with("later") {
        // The final verdict may come from a later canonical Round, but it must carry the
        // committed successor's exact history rather than merely depending on its condition.
        let mut bind = node(
            "later_bind",
            TaskOperatorV1::ReviewBind {},
            BTreeMap::from([
                ("source".into(), later_source(case)),
                ("history".into(), from("integrated_reduce", "history")),
            ]),
        );
        conditional(&mut bind, "prepare");
        pipeline.nodes.push(bind);
        for name in ["bugs", "correctness"] {
            let mut reviewer = node(
                &format!("later_{name}"),
                TaskOperatorV1::Verify { slot: name.into() },
                BTreeMap::from([
                    ("source".into(), later_source(case)),
                    ("subject".into(), from("later_bind", "subject")),
                    ("assignment".into(), from("later_bind", name)),
                    ("history".into(), from("integrated_reduce", "history")),
                    ("checks".into(), later_checks(case)),
                ]),
            );
            conditional(&mut reviewer, "prepare");
            pipeline.nodes.push(reviewer);
        }
        let mut reducer = node(
            "later_reduce",
            TaskOperatorV1::ReviewReduce {},
            BTreeMap::from([
                ("source".into(), later_source(case)),
                ("subject".into(), from("later_bind", "subject")),
                ("history".into(), from("integrated_reduce", "history")),
                ("checks".into(), later_checks(case)),
                ("bugs".into(), from("later_bugs", "result")),
                ("correctness".into(), from("later_correctness", "result")),
            ]),
        );
        conditional(&mut reducer, "prepare");
        pipeline.nodes.push(reducer);
    }
    if case.contains("_sibling_later") {
        pipeline.contract.inputs.insert(
            "sibling".into(),
            port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
        );
        pipeline.nodes.push(node(
            "sibling_check",
            TaskOperatorV1::Check {
                checks: BTreeSet::from(["syntax".into()]),
                remote_checks: BTreeSet::new(),
            },
            BTreeMap::from([("source".into(), root("sibling"))]),
        ));
    }
    for (id, port_name, output) in [
        ("select_review", "result", "review"),
        ("select_history", "history", "history"),
    ] {
        pipeline.nodes.push(node(
            id,
            TaskOperatorV1::Select {},
            BTreeMap::from([
                ("condition".into(), from("prepare", "result")),
                (
                    "passed".into(),
                    from(
                        if case.ends_with("later") {
                            "later_reduce"
                        } else {
                            "integrated_reduce"
                        },
                        port_name,
                    ),
                ),
                ("failed".into(), from("reduce", port_name)),
                ("inconclusive".into(), from("reduce", port_name)),
            ]),
        ));
        pipeline.contract.outputs.get_mut(output).unwrap().affinity = PortAffinityV1::Unbound {};
        pipeline.outputs.insert(output.into(), from(id, "output"));
    }
    pipeline
        .coverage
        .insert("reviewed".into(), from("select_review", "output"));
    if (case.contains("_transport_") || case.ends_with("later")) && !case.contains("_embedded_") {
        pipeline.contract.outputs.insert(
            "snapshot".into(),
            port(SOURCE_TREE_V1, PortAffinityV1::Unbound {}),
        );
        pipeline.nodes.push(node(
            "transport_source",
            TaskOperatorV1::Select {},
            BTreeMap::from([
                ("condition".into(), from("prepare", "result")),
                (
                    "passed".into(),
                    if case.contains("_mismatched_snapshot_") {
                        root("source")
                    } else {
                        later_source(case)
                    },
                ),
                ("failed".into(), root("source")),
                ("inconclusive".into(), root("source")),
            ]),
        ));
        pipeline
            .outputs
            .insert("snapshot".into(), from("transport_source", "output"));
    }
    pipeline.max_attempts = if case.ends_with("later") {
        10 + u32::from(case.contains("_sibling_later"))
            + 2 * u32::from(case.contains("_descendant_later"))
    } else {
        8
    };
    if case.ends_with("condition_only") || case.ends_with("selected_old") {
        let mut decoy = node(
            "condition_only",
            TaskOperatorV1::Select {},
            BTreeMap::from([
                ("condition".into(), from("integrated_reduce", "result")),
                ("passed".into(), from("reduce", "result")),
                ("failed".into(), from("reduce", "result")),
                ("inconclusive".into(), from("reduce", "result")),
            ]),
        );
        if case.ends_with("selected_old") {
            decoy
                .inputs
                .insert("passed".into(), from("integrated_reduce", "result"));
        }
        conditional(&mut decoy, "prepare");
        pipeline.nodes.push(decoy);
        pipeline
            .nodes
            .iter_mut()
            .find(|n| n.id == "select_review")
            .unwrap()
            .inputs
            .insert("passed".into(), from("condition_only", "output"));
    }
    if case.ends_with("no_successor") {
        pipeline
            .nodes
            .retain(|n| !n.id.starts_with("integrated_") && !n.id.starts_with("select_"));
        pipeline
            .outputs
            .insert("review".into(), from("reduce", "result"));
        pipeline
            .outputs
            .insert("history".into(), from("reduce", "history"));
        pipeline
            .coverage
            .insert("reviewed".into(), from("reduce", "result"));
    }
    if case == "native_integration_unrelated_successor" {
        pipeline
            .nodes
            .iter_mut()
            .find(|n| n.id == "integrated_bind")
            .unwrap()
            .inputs
            .insert("history".into(), from("reduce", "history"));
    }
    if case == "native_integration_incomplete_successor" || case.ends_with("_incomplete") {
        let files = &mut packages
            .iter_mut()
            .find(|(n, _)| n == "fixture/bugs")
            .unwrap()
            .1;
        let script = String::from_utf8(files["worker.py"].clone())
            .unwrap()
            .replace(
                "s=r['inputs']['subject'][0]['payload']",
                "s=r['inputs']['subject'][0]['payload']\nif s['round']==2: sys.exit(9)",
            );
        files.insert("worker.py".into(), script.into_bytes());
    }
}

fn configure_scatter(
    case: &str,
    pipeline: &mut PipelineDefinition,
    packages: &mut [(String, BTreeMap<String, Vec<u8>>)],
) {
    let files = &mut packages
        .iter_mut()
        .find(|(n, _)| n == "fixture/bugs")
        .unwrap()
        .1;
    let mut worker: TaskWorkerManifest =
        toml::from_str(std::str::from_utf8(&files["worker.toml"]).unwrap()).unwrap();
    if matches!(case, "native_scatter" | "native_scatter_diff") {
        let mut diff = port(af_contracts::task::diff_subject::DIFF_SUBJECT_V1, same());
        diff.optional = true;
        worker
            .signature
            .contract
            .inputs
            .insert("diff_subject".into(), diff);
        worker
            .signature
            .retains
            .get_mut("result")
            .unwrap()
            .insert("diff_subject".into());
    }
    worker.signature.contract.inputs.insert(
        "slice".into(),
        port(af_contracts::contract::REVIEW_SLICE_V1, same()),
    );
    worker
        .signature
        .retains
        .get_mut("result")
        .unwrap()
        .insert("slice".into());
    files.insert(
        "worker.toml".into(),
        toml::to_string(&worker).unwrap().into_bytes(),
    );
    files.insert("worker.py".into(),format!(r#"import json,sys
r=json.load(sys.stdin)
slice=r['inputs']['slice'][0]['payload']
assert len(slice['paths'])==1
if {case:?}=='native_scatter_diff':
    d=r['inputs']['diff_subject'][0]
    assert d['artifact_type']=='af/DiffSubject@1'
    assert d['payload']['snapshot_id']==r['inputs']['source'][0]['snapshot_id']
elif {case:?}=='native_scatter':
    assert 'diff_subject' not in r['inputs']
if {case:?} in ['native_scatter_failed','native_scatter_embedded_failed'] and slice['paths']==['other.rs']: sys.exit(9)
print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'result':[{{'reports':[],'benchmark_demands':[],'dispositions':[]}}]}}}}))
"#).into_bytes());
    let files = &mut packages
        .iter_mut()
        .find(|(n, _)| n == "fixture/correctness")
        .unwrap()
        .1;
    let mut worker: TaskWorkerManifest =
        toml::from_str(std::str::from_utf8(&files["worker.toml"]).unwrap()).unwrap();
    let mut shard_port = port(af_contracts::contract::SHARD_SET_V1, same());
    shard_port.optional = case.ends_with("no_closeout");
    worker
        .signature
        .contract
        .inputs
        .insert("shards".into(), shard_port);
    worker
        .signature
        .retains
        .get_mut("result")
        .unwrap()
        .insert("shards".into());
    if case == "native_scatter_sliced_closeout" {
        let mut slice = port(af_contracts::contract::REVIEW_SLICE_V1, same());
        slice.optional = true;
        worker
            .signature
            .contract
            .inputs
            .insert("slice".into(), slice);
    }
    files.insert(
        "worker.toml".into(),
        toml::to_string(&worker).unwrap().into_bytes(),
    );
    let count = if case == "native_scatter_bound" {
        1
    } else if case == "native_scatter_stage_bound" {
        64
    } else {
        2
    };
    let mut slice = node(
        "slicer",
        TaskOperatorV1::ReviewSlice {
            scatter: "bugs".into(),
            max_paths_per_slice: 1,
            max_fanout: count,
        },
        BTreeMap::from([
            ("source".into(), root("source")),
            ("subject".into(), from("bind", "subject")),
            ("history".into(), root("history")),
        ]),
    );
    conditional(&mut slice, "check");
    pipeline.nodes.push(slice);
    let scatter = pipeline.nodes.iter_mut().find(|n| n.id == "bugs").unwrap();
    scatter.operator = TaskOperatorV1::ReviewScatter {
        slot: "bugs".into(),
        max_fanout: count,
    };
    scatter
        .inputs
        .insert("slices".into(), from("slicer", "slices"));
    if !case.ends_with("no_closeout") {
        pipeline
            .nodes
            .iter_mut()
            .find(|n| n.id == "correctness")
            .unwrap()
            .inputs
            .insert("shards".into(), from("bugs", "result"));
    }
    pipeline
        .nodes
        .iter_mut()
        .find(|n| n.id == "reduce")
        .unwrap()
        .operator = TaskOperatorV1::ReviewReduceShards {
        reviewers: BTreeSet::from(["bugs".into()]),
        closeout: "correctness".into(),
    };
    pipeline.max_attempts = if case.ends_with("missing") { 3 } else { 4 };
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assert_native(
    case: &str,
    cas: &Cas,
    domain: &ReviewTaskDomain,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    result: &TaskResultV1,
    report: &af_graph::RunReport,
    original_snapshot: &str,
    directory: &std::path::Path,
    lease: &af_store::store::task::TaskLease,
) {
    if case == "native_integration" || case == "native_accounting" {
        assert_native_observation_integrity(cas, state, directory);
    }
    let execution = state.execution.as_ref().unwrap();
    assert!(execution.graph.nodes.values().all(|node| {
        !["review_domain", "review_integration_checks"].contains(
            &serde_json::to_value(&node.operator).unwrap()["kind"]
                .as_str()
                .unwrap(),
        )
    }));
    assert!(
        execution
            .graph
            .inputs
            .values()
            .all(|input| input.artifact_type != "af/LegacyReviewRound@1")
    );
    let store = EventStore::open_read_only(directory.join("events.sqlite")).unwrap();
    assert_eq!(
        store.run_ids().unwrap(),
        [af_store::store::task::task_run_id(&state.task_id).unwrap()]
    );
    let receipts: Vec<_> = result
        .evidence
        .iter()
        .filter_map(|id| {
            cas.get_artifact(id)
                .ok()
                .filter(|a| a.artifact_type == TASK_REVIEW_CONCLUSION_V1)
        })
        .collect();
    assert_eq!(receipts.len(), 1);
    let conclusion: TaskReviewConclusionV1 =
        serde_json::from_value(receipts[0].payload.clone()).unwrap();
    assert_eq!(conclusion.through_sequence, state.next_sequence);
    assert_eq!(
        conclusion.cumulative_chargeable_tokens.get(),
        execution.budget.committed_tokens()
    );
    assert_eq!(conclusion.begun_attempts, execution.budget.begun_attempts());
    let get = |node: &str, port: &str| {
        execution
            .outputs
            .get(&format!("root.nodes.{node}"))
            .and_then(|(_, o)| o.outputs.get(port))
    };
    if case == "native_accounting" {
        assert_late_usage(
            cas, domain, runtime, state, result, report, directory, lease,
        );
        return;
    } else if case.starts_with("native_integration") {
        if case == "native_integration_wrong_paths" || case == "native_integration_wrong_result" {
            assert!(get("prepare", "result").is_none(), "{report:?}");
            let expected = if case.ends_with("wrong_result") {
                "exact selected original Reviewer Result"
            } else {
                "complete sealed candidate diff"
            };
            assert!(report.outcomes.iter().any(|(n,o)|n.ends_with(".prepare") && matches!(o,af_graph::NodeOutcome::Failed {error,..} if error.contains(expected))),"{report:?}");
        } else {
            let preparation: TaskReviewIntegrationV1 = serde_json::from_value(
                cas.get_artifact(
                    &get("prepare", "result")
                        .unwrap_or_else(|| panic!("{report:?}"))
                        .artifact_ids[0],
                )
                .unwrap()
                .payload,
            )
            .unwrap();
            if case.ends_with("overlap") || case.ends_with("protected") {
                assert!(matches!(
                    preparation.selection,
                    TaskReviewIntegrationSelection::Conflict { .. }
                ));
                assert!(get("prepare", "source").is_none());
                assert!(get("post_check", "result").is_none());
                assert!(get("commit", "result").is_none());
                assert_eq!(execution.budget.begun_attempts(), 5);
            } else {
                let TaskReviewIntegrationSelection::Prepared {
                    snapshot_id,
                    proposals,
                    ..
                } = preparation.selection
                else {
                    panic!("{preparation:?}")
                };
                assert_eq!(proposals.len(), 2);
                let (snapshot, manifest) =
                    af_source_git::task::read_snapshot(cas, &snapshot_id).unwrap();
                assert_eq!(
                    snapshot.parent_snapshot_id.as_deref(),
                    Some(original_snapshot)
                );
                if case == "native_integration_manifest_changes" {
                    assert!(manifest.get("other.rs").is_none());
                    assert_eq!(manifest.get("lib.rs").unwrap().kind, EntryKind::Executable);
                    assert_eq!(
                        cas.get(&manifest.get("new.rs").unwrap().content).unwrap(),
                        b"added\n"
                    );
                    assert_eq!(
                        cas.get(&manifest.get("lib.rs").unwrap().content).unwrap(),
                        b"repaired\n"
                    );
                } else {
                    for path in if case == "native_integration_duplicate" {
                        vec!["lib.rs"]
                    } else {
                        vec!["lib.rs", "other.rs"]
                    } {
                        assert_eq!(
                            cas.get(&manifest.get(path).unwrap().content).unwrap(),
                            if case.ends_with("failed_checks") {
                                b"invalid\n".as_slice()
                            } else {
                                b"repaired\n".as_slice()
                            }
                        );
                    }
                }
                let commit: TaskReviewCommitV1 = serde_json::from_value(
                    cas.get_artifact(
                        &get("commit", "result")
                            .unwrap_or_else(|| panic!("{report:?}"))
                            .artifact_ids[0],
                    )
                    .unwrap()
                    .payload,
                )
                .unwrap();
                if case.ends_with("failed_checks") {
                    assert_eq!(commit.outcome, ReceiptOutcomeV1::Failed);
                    assert!(commit.pending_claims.is_empty());
                    assert!(get("commit", "source").is_none());
                    assert!(get("integrated_reduce", "result").is_none());
                } else {
                    assert_eq!(commit.outcome, ReceiptOutcomeV1::Passed);
                    assert_eq!(commit.pending_claims.len(), 2);
                    if result.acceptance == TaskAcceptanceV1::Satisfied {
                        af_store::store::task::review_integration::require_native_successors(
                            cas, state, result,
                        )
                        .unwrap();
                        for unrelated in [false, true] {
                            let mut broken = state.clone();
                            let execution = broken.execution.as_mut().unwrap();
                            let name = "root.nodes.integrated_reduce";
                            if unrelated {
                                let old =
                                    execution.outputs["root.nodes.reduce"].1.outputs["result"]
                                        .clone();
                                execution
                                    .outputs
                                    .get_mut(name)
                                    .unwrap()
                                    .1
                                    .outputs
                                    .insert("result".into(), old);
                            } else {
                                execution.outputs.remove(name);
                            }
                            assert!(af_store::store::task::review_integration::require_native_successors(cas, &broken, result).is_err());
                        }
                    }
                    assert!(result.outputs.contains_key("review"), "{report:?}");
                    let review: TaskReviewRoundV1 = serde_json::from_value(
                        cas.get_artifact(&result.outputs["review"].artifact_ids[0])
                            .unwrap()
                            .payload,
                    )
                    .unwrap();
                    assert_eq!(review.round, if case.ends_with("later") { 3 } else { 2 });
                    if case.ends_with("later") {
                        assert_eq!(
                            result.outputs["snapshot"].snapshot_id.as_ref(),
                            Some(&review.snapshot_id)
                        );
                        assert_eq!(
                            review.invocation.inputs["source"],
                            result.outputs["snapshot"]
                        );
                        if case.contains("_descendant_later") {
                            assert_ne!(review.snapshot_id, commit.snapshot_id);
                            assert!(
                                af_source_git::task::descends_from(
                                    cas,
                                    &review.snapshot_id,
                                    &commit.snapshot_id
                                )
                                .unwrap()
                            );
                        } else {
                            assert_eq!(review.snapshot_id, commit.snapshot_id);
                        }
                    }
                    if case.ends_with("later") {
                        let mut stale = state.clone();
                        let outputs = &mut stale.execution.as_mut().unwrap().outputs;
                        let initial = outputs["root.nodes.reduce"].1.outputs["history"].clone();
                        outputs
                            .get_mut("root.nodes.integrated_reduce")
                            .unwrap()
                            .1
                            .outputs
                            .insert("history".into(), initial);
                        assert!(
                            af_store::store::task::review_integration::require_native_successors(
                                cas, &stale, result
                            )
                            .is_err()
                        );
                    }
                    if case.contains("_blocking") {
                        assert_eq!(result.acceptance, TaskAcceptanceV1::Satisfied);
                        assert_eq!(
                            review.conclusion,
                            if case.ends_with("exhausted") {
                                ReviewConclusionV1::ConvergenceExhausted
                            } else {
                                ReviewConclusionV1::ChangesRequested
                            }
                        );
                        assert_eq!(
                            result.domain_conclusion,
                            if case.ends_with("exhausted") {
                                "convergence_exhausted"
                            } else {
                                "changes_requested"
                            }
                        );
                        let mut forged = result.clone();
                        forged.domain_conclusion = "pass".into();
                        assert!(
                            af_store::store::task::review_integration::require_native_successors(
                                cas, state, &forged
                            )
                            .is_err()
                        );
                        assert!(
                            domain
                                .validate_result(cas, &state.revision, &forged)
                                .is_err()
                        );
                        let id = cas
                            .put_artifact(
                                TASK_RESULT_V1,
                                producer(),
                                vec![],
                                None,
                                serde_json::to_value(&forged).unwrap(),
                            )
                            .unwrap()
                            .0;
                        assert!(runtime.finish(&id).is_err());
                    }
                    if case == "native_integration_incomplete_successor"
                        || case.ends_with("_incomplete")
                    {
                        assert_eq!(review.conclusion, ReviewConclusionV1::Incomplete);
                        assert!(review.finding_set_id.is_none());
                        assert_eq!(result.acceptance, TaskAcceptanceV1::Inconclusive);
                        let mut forged = result.clone();
                        forged.acceptance = TaskAcceptanceV1::Satisfied;
                        assert!(
                            af_store::store::task::review_integration::require_native_successors(
                                cas, state, &forged
                            )
                            .unwrap_err()
                            .to_string()
                            .contains("complete exact committed successor")
                        );
                        domain
                            .validate_result(cas, &state.revision, result)
                            .unwrap();
                        let id = cas
                            .put_artifact(
                                TASK_RESULT_V1,
                                producer(),
                                vec![],
                                None,
                                serde_json::to_value(result).unwrap(),
                            )
                            .unwrap()
                            .0;
                        runtime.finish(&id).unwrap();
                        let reopened = EventStore::open_read_only(directory.join("events.sqlite"))
                            .unwrap()
                            .task_projection(cas, &state.task_id)
                            .unwrap()
                            .unwrap();
                        let replay = reopened.execution.as_ref().unwrap();
                        assert_eq!(replay.outputs, execution.outputs);
                        assert_eq!(
                            replay.budget.begun_attempts(),
                            execution.budget.begun_attempts()
                        );
                        assert!(matches!(reopened.phase, TaskPhaseV1::Finished { .. }));
                        return;
                    }
                    if case.contains("_descendant_later") {
                        let (head, _) =
                            af_source_git::task::read_snapshot(cas, &review.snapshot_id).unwrap();
                        assert_eq!(head.parent_snapshot_id.as_ref(), Some(&snapshot_id));
                    } else {
                        assert_eq!(review.snapshot_id, snapshot_id);
                    }
                    let findings: af_contracts::FindingSetV1 = serde_json::from_value(
                        cas.get_artifact(review.finding_set_id.as_ref().unwrap())
                            .unwrap()
                            .payload,
                    )
                    .unwrap();
                    assert_eq!(
                        findings.findings.len(),
                        if case == "native_integration_sharded" || case.contains("_blocking") {
                            3
                        } else {
                            2
                        }
                    );
                    for f in findings.findings {
                        if commit.pending_claims.contains_key(&f.finding_id) {
                            assert_eq!(f.status, "pending-verification");
                        } else {
                            assert!(
                                case == "native_integration_sharded" || case.contains("_blocking")
                            );
                            assert_eq!(f.status, "open");
                        }
                    }
                    assert_eq!(
                        execution.budget.begun_attempts(),
                        if case == "native_integration_sharded" {
                            9
                        } else if case.ends_with("later") {
                            10 + 2 * u64::from(case.contains("_descendant_later"))
                        } else {
                            8
                        }
                    );
                }
            }
        }
    } else if case == "native_scatter_bound" {
        assert!(get("slicer", "slices").is_none());
        assert_eq!(result.domain_conclusion, "incomplete");
        assert_eq!(execution.budget.begun_attempts(), 1);
    } else {
        let shards: ShardSetV1 = serde_json::from_value(
            cas.get_artifact(
                &get("bugs", "result")
                    .unwrap_or_else(|| panic!("{report:?}"))
                    .artifact_ids[0],
            )
            .unwrap()
            .payload,
        )
        .unwrap();
        assert_eq!(shards.shards.len(), 2);
        if case == "native_scatter_diff" {
            let mut ids = BTreeSet::new();
            for index in 1..=2 {
                let child = format!("root.nodes.bugs.slice{index}");
                let (_, input) = &execution.invocations[&child];
                let carried = &input.inputs["diff_subject"];
                assert!(ids.insert(carried.artifact_ids[0].clone()));
                let artifact = cas.get_artifact(&carried.artifact_ids[0]).unwrap();
                assert_eq!(
                    artifact.producer,
                    Producer::KernelOperation {
                        run_id: af_store::store::task::task_run_id(&state.task_id).unwrap(),
                        node_id: Some(child.clone()),
                        operation_id: af_contracts::task::diff_subject::DIFF_SUBJECT_OPERATION
                            .into(),
                    }
                );
                let signature: OperatorSignature = serde_json::from_value(json!({
                    "contract": execution.graph.owned_children["root.nodes.bugs"].contract,
                    "effects": [], "evidence": {}, "roles": []
                }))
                .unwrap();
                let mut swapped = input.clone();
                let other = format!("root.nodes.bugs.slice{}", if index == 1 { 2 } else { 1 });
                swapped.inputs.insert(
                    "diff_subject".into(),
                    execution.invocations[&other].1.inputs["diff_subject"].clone(),
                );
                assert!(
                    af_runtime::task::source::check_diff_subject(cas, &swapped, &signature)
                        .is_err()
                );
                assert_eq!(
                    af_runtime::task::source::kernel_diff_subject(
                        cas,
                        &input.plan_id,
                        &child,
                        &input.inputs["source"]
                    )
                    .unwrap()
                    .as_ref(),
                    Some(carried)
                );
            }
            assert!(
                !execution.graph.nodes["root.nodes.bugs"]
                    .contract
                    .inputs
                    .contains_key("diff_subject")
            );
        }

        assert!(matches!(
            shards.shards[0].outcome,
            ShardOutcomeV1::Completed { .. }
        ));
        let review: TaskReviewRoundV1 = serde_json::from_value(
            cas.get_artifact(&result.outputs["review"].artifact_ids[0])
                .unwrap()
                .payload,
        )
        .unwrap();
        if case.ends_with("failed") || case.ends_with("missing") || case.ends_with("incomplete") {
            if case.ends_with("missing") {
                assert!(matches!(
                    shards.shards[1].outcome,
                    ShardOutcomeV1::Missing { .. }
                ));
            } else {
                assert!(matches!(
                    shards.shards[1].outcome,
                    ShardOutcomeV1::Failed { .. }
                ));
            }
            assert_eq!(review.conclusion, ReviewConclusionV1::Incomplete);
            assert!(review.finding_set_id.is_none() && review.demand_set_id.is_none());
            assert!(review.missing_reviewers.contains("bugs"));
        } else {
            assert!(shards.complete());
            assert_eq!(review.conclusion, ReviewConclusionV1::Pass);
            assert_eq!(review.selected_results.len(), 3);
            let frame = cas
                .get_artifact(&result.outputs["review"].artifact_ids[0])
                .unwrap();
            let closure: Vec<_> = frame
                .input_artifacts
                .iter()
                .filter_map(|id| {
                    cas.get_artifact(id)
                        .ok()
                        .filter(|a| a.artifact_type == af_contracts::contract::SEMANTIC_CLOSURE_V1)
                })
                .collect();
            assert_eq!(closure.len(), 1);
            let closure: af_contracts::SemanticClosureV1 =
                serde_json::from_value(closure[0].payload.clone()).unwrap();
            assert_eq!(closure.required_artifact_ids.len(), 2);
            assert!(closure.closeout_result_id.is_some());
        }
        assert_eq!(
            execution.budget.begun_attempts(),
            if case.ends_with("missing") { 3 } else { 4 }
        );
    }
    assert_replay_and_finish(cas, domain, runtime, state, result, report, directory);
}

/// Native replacement for the retired handoff module's read-closure regressions.
/// Integration/continuation receipts and accounting now share the one Task stream.
fn assert_native_observation_integrity(
    cas: &Cas,
    expected: &TaskProjection,
    directory: &std::path::Path,
) {
    let path = directory.join("events.sqlite");
    let reader = EventStore::open_read_only(&path).unwrap();
    let (state, reads) = cas.track_reads(|| reader.task_projection(cas, &expected.task_id));
    let state = state.unwrap().unwrap();
    assert_eq!(state.next_sequence, expected.next_sequence);
    let accounting = state.execution.as_ref().unwrap().attempt_accounting();
    let account = |attempt: &af_store::store::task::execution::TaskAttemptAccounting| {
        (
            attempt.attempt_id.clone(),
            attempt.reservation.clone(),
            attempt.charged_tokens,
            attempt.result.clone(),
            attempt.raw_artifact_ids.clone(),
            attempt.usage_id.clone(),
        )
    };
    let original: Vec<_> = accounting.iter().map(account).collect();
    for _ in 0..4 {
        assert!(reader.task_observation_unchanged(cas, &state).unwrap());
    }
    let targets: Vec<_> = reads
        .into_iter()
        .filter(|id| {
            cas.get_optional_artifact_shared(id)
                .unwrap()
                .is_some_and(|a| {
                    matches!(
                        a.artifact_type.as_str(),
                        "af/ReviewHistory@1"
                            | "af/TaskReviewRound@1"
                            | "af/TaskReviewCommit@1"
                            | "af/TaskInvocation@1"
                            | "af/TaskTokenUsage@3"
                    )
                })
        })
        .collect();
    assert!(
        !targets.is_empty(),
        "native evidence must be in the observed closure"
    );
    for id in targets {
        let file = directory.join("cas/objects").join(&id[7..9]).join(&id[9..]);
        let bytes = std::fs::read(&file).unwrap();
        for missing in [false, true] {
            if missing {
                std::fs::remove_file(&file).unwrap();
            } else {
                // A same-size mutation must fail even if a prior read warmed the parse memo.
                let mut changed = bytes.clone();
                changed[0] ^= 1;
                std::fs::write(&file, changed).unwrap();
            }
            assert!(
                reader.task_observation_unchanged(cas, &state).is_err(),
                "{id}"
            );
            assert!(
                reader.task_projection(cas, &state.task_id).is_err(),
                "warm: {id}"
            );
            let fresh = EventStore::open_read_only(&path).unwrap();
            assert!(
                fresh.task_projection(cas, &state.task_id).is_err(),
                "cold: {id}"
            );
            std::fs::write(&file, &bytes).unwrap();
            let restored = reader
                .task_projection(cas, &state.task_id)
                .unwrap()
                .unwrap();
            assert_eq!(restored.next_sequence, state.next_sequence);
            assert_eq!(restored.phase, state.phase);
            assert_eq!(
                restored
                    .execution
                    .as_ref()
                    .unwrap()
                    .attempt_accounting()
                    .iter()
                    .map(account)
                    .collect::<Vec<_>>(),
                original
            );
            assert_eq!(
                restored.execution.as_ref().unwrap().outputs,
                state.execution.as_ref().unwrap().outputs
            );
        }
    }
}

fn assert_replay_and_finish(
    cas: &Cas,
    domain: &ReviewTaskDomain,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    result: &TaskResultV1,
    report: &af_graph::RunReport,
    directory: &std::path::Path,
) {
    let execution = state.execution.as_ref().unwrap();
    domain
        .validate_result(cas, &state.revision, result)
        .unwrap();
    let replay = runtime.execute().unwrap();
    let replay_state = runtime.projection().unwrap();
    assert_eq!(replay.outcomes, report.outcomes);
    assert_eq!(
        replay_state
            .execution
            .as_ref()
            .unwrap()
            .budget
            .begun_attempts(),
        execution.budget.begun_attempts()
    );
    let replay_result = domain.result(cas, &replay_state, &replay).unwrap();
    let stale = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    if state.revision.kind == "review" {
        assert!(
            runtime
                .finish(&stale)
                .unwrap_err()
                .contains("exact admitted public outputs and evidence")
        );
    } else {
        // Embedded acceptance has no standalone conclusion prefix; unchanged common outputs
        // and accounting therefore retain exactly the same result after a no-work replay.
        assert_eq!(&replay_result, result);
    }
    let result_id = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&replay_result).unwrap(),
        )
        .unwrap()
        .0;
    runtime.finish(&result_id).unwrap();
    let reopened = EventStore::open_read_only(directory.join("events.sqlite")).unwrap();
    assert!(matches!(
        reopened
            .task_projection(cas, &state.task_id)
            .unwrap()
            .unwrap()
            .phase,
        af_contracts::task::TaskPhaseV1::Finished { .. }
    ));
}

#[allow(clippy::too_many_arguments)]
fn assert_late_usage(
    cas: &Cas,
    domain: &ReviewTaskDomain,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    result: &TaskResultV1,
    report: &af_graph::RunReport,
    directory: &std::path::Path,
    lease: &af_store::store::task::TaskLease,
) {
    use af_contracts::task::execution::TaskExecutionRecordV1;
    use af_contracts::task::usage::{TASK_TOKEN_USAGE_V3, TaskTokenUsageV3};
    let mut writer = EventStore::open(directory.join("events.sqlite")).unwrap();
    let attempts: Vec<_> = state
        .execution
        .as_ref()
        .unwrap()
        .attempt_accounting()
        .into_iter()
        .filter(|a| {
            a.reservation.node.ends_with(".bugs") || a.reservation.node.ends_with(".correctness")
        })
        .collect();
    assert_eq!(attempts.len(), 2);
    let amount = u128::from(u64::MAX) + 17;
    for attempt in &attempts {
        let usage = TaskTokenUsageV3::charge_only(amount);
        let id = cas
            .put_artifact(
                TASK_TOKEN_USAGE_V3,
                producer(),
                vec![],
                None,
                serde_json::to_value(usage).unwrap(),
            )
            .unwrap()
            .0;
        writer
            .observe_task_usage(
                cas,
                lease,
                TaskExecutionRecordV1::UsageObserved {
                    attempt_id: attempt.attempt_id.clone(),
                    charged_tokens: amount,
                    usage_id: id,
                    raw_artifact_ids: vec![],
                },
            )
            .unwrap();
    }
    let old = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    assert!(
        runtime
            .finish(&old)
            .unwrap_err()
            .contains("resource exhaustion")
    );
    let current = runtime.projection().unwrap();
    assert_eq!(
        current.execution.as_ref().unwrap().budget.begun_attempts(),
        3
    );
    let result = domain.result(cas, &current, report).unwrap();
    assert_eq!(result.execution, TaskExecutionV1::Exhausted);
    assert_eq!(result.acceptance, TaskAcceptanceV1::Inconclusive);
    let receipt: TaskReviewConclusionV1 = result
        .evidence
        .iter()
        .filter_map(|id| {
            cas.get_artifact(id)
                .ok()
                .filter(|a| a.artifact_type == TASK_REVIEW_CONCLUSION_V1)
        })
        .map(|a| serde_json::from_value(a.payload).unwrap())
        .next()
        .unwrap();
    assert_eq!(receipt.cumulative_chargeable_tokens.get(), amount * 2);
    assert_eq!(receipt.through_sequence, current.next_sequence);
    assert_eq!(receipt.begun_attempts, 3);
    let frozen: TaskResultV1 =
        serde_json::from_value(cas.get_artifact(&old).unwrap().payload).unwrap();
    let frozen: TaskReviewConclusionV1 = frozen
        .evidence
        .iter()
        .filter_map(|id| {
            cas.get_artifact(id)
                .ok()
                .filter(|a| a.artifact_type == TASK_REVIEW_CONCLUSION_V1)
        })
        .map(|a| serde_json::from_value(a.payload).unwrap())
        .next()
        .unwrap();
    assert_eq!(frozen.cumulative_chargeable_tokens.get(), 0);
    let id = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    runtime.finish(&id).unwrap();
    let reopened = EventStore::open_read_only(directory.join("events.sqlite")).unwrap();
    assert_eq!(
        reopened
            .task_projection(cas, &state.task_id)
            .unwrap()
            .unwrap()
            .execution
            .unwrap()
            .budget
            .committed_tokens(),
        amount * 2
    );
}

#[test]
fn native_integration_recovers_published_commit_without_repeating_proposals_or_checks() {
    run_case("native_integration_recover");
}
#[test]
fn native_scatter_recovers_published_shards_without_another_child_attempt() {
    run_case("native_scatter_recover");
}

pub(super) struct PublicationFault<'a> {
    inner: &'a ReviewTaskDomain,
    point: &'static str,
    pending: std::sync::atomic::AtomicBool,
    floor_after_check: bool,
}
impl<'a> PublicationFault<'a> {
    pub(super) fn new(inner: &'a ReviewTaskDomain, case: &str) -> Self {
        Self {
            inner,
            point: if case.contains("_embedded_") && case.starts_with("native_integration") {
                "root.nodes.child.nodes.commit"
            } else if case == "native_floor" {
                "root.nodes.check"
            } else if case.starts_with("native_integration") {
                "root.nodes.commit"
            } else {
                "root.nodes.bugs"
            },
            pending: std::sync::atomic::AtomicBool::new(true),
            floor_after_check: case == "native_floor",
        }
    }
}
impl TaskOperatorHost for PublicationFault<'_> {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &af_contracts::task::execution::TaskInvocationV1,
        definition: &af_graph::task::CompiledNode,
        attempt: &af_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.inner.prepare_context(cas, input, definition, attempt)
    }
    fn execute(
        &self,
        cas: &Cas,
        input: &af_contracts::task::execution::TaskInvocationV1,
        definition: &af_graph::task::CompiledNode,
        attempt: Option<&af_store::store::task::execution::PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        self.inner
            .execute(cas, input, definition, attempt, cancellation)
    }
    fn prepare_owned_children(
        &self,
        cas: &Cas,
        parent: &af_contracts::task::execution::TaskInvocationV1,
    ) -> Result<TaskOwnedChildrenInputs, String> {
        self.inner.prepare_owned_children(cas, parent)
    }
    fn complete_owned_children(
        &self,
        cas: &Cas,
        parent: &af_contracts::task::execution::TaskInvocationV1,
        children: &af_contracts::task::owned_children::TaskOwnedChildSetV1,
        facts: &[af_store::store::task::execution::owned::TaskOwnedChildEvidence],
    ) -> Result<BTreeMap<String, ArtifactInputV1>, String> {
        self.inner
            .complete_owned_children(cas, parent, children, facts)
    }
    fn commit_domain_output(
        &self,
        _cas: &Cas,
        input: &af_contracts::task::execution::TaskInvocationV1,
        _id: &str,
        _output: &af_contracts::task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        if input.node == self.point
            && self
                .pending
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            if self.floor_after_check {
                assert!(af_runtime::storage::install(std::sync::Arc::new(
                    RefuseNativeWorkerFloor
                )));
                Ok(())
            } else {
                Err("fixture lost acknowledgement after native output publication".into())
            }
        } else {
            Ok(())
        }
    }
}
impl TaskDomain for PublicationFault<'_> {
    fn validate_context(
        &self,
        cas: &Cas,
        input: &af_contracts::task::execution::TaskInvocationV1,
        attempt: &af_store::store::task::execution::ReservedTaskAttempt,
        id: &str,
    ) -> Result<(), String> {
        self.inner.validate_context(cas, input, attempt, id)
    }
    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlan,
        input: &af_contracts::task::execution::TaskInvocationV1,
        output: &af_contracts::task::execution::TaskOutputV1,
        definition: &af_graph::task::CompiledNode,
    ) -> Result<(), String> {
        self.inner
            .validate_output(cas, task, plan, input, output, definition)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        self.inner.validate_result(cas, task, result)
    }
    fn validate_owned_children(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlan,
        parent: &af_contracts::task::execution::TaskInvocationV1,
        children: &af_contracts::task::owned_children::TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        self.inner
            .validate_owned_children(cas, task, plan, parent, children)
    }
    fn validate_owned_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlan,
        parent: &af_contracts::task::execution::TaskInvocationV1,
        children: &af_contracts::task::owned_children::TaskOwnedChildSetV1,
        facts: &[af_store::store::task::execution::owned::TaskOwnedChildEvidence],
        output: &af_contracts::task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        self.inner
            .validate_owned_completion(cas, task, plan, parent, children, facts, output)
    }
}

#[test]
fn native_integration_resolves_shard_reports_to_their_original_logical_finding_source() {
    run_case("native_integration_sharded");
}

pub(super) fn configure_sharded_integration(
    case: &str,
    pipeline: &mut PipelineDefinition,
    packages: &mut Vec<(String, BTreeMap<String, Vec<u8>>)>,
) {
    if case != "native_integration_sharded" {
        return;
    }
    for name in ["bugs", "correctness"] {
        let mut files = packages
            .iter()
            .find(|(n, _)| n == &format!("fixture/{name}"))
            .unwrap()
            .1
            .clone();
        let mut worker: TaskWorkerManifest =
            toml::from_str(std::str::from_utf8(&files["worker.toml"]).unwrap()).unwrap();
        worker.name = format!("fixture/full_{name}");
        files.insert(
            "worker.toml".into(),
            toml::to_string(&worker).unwrap().into_bytes(),
        );
        let mut slot = pipeline.slots[name].clone();
        slot.worker = worker.name.clone();
        pipeline.slots.insert(format!("full_{name}"), slot);
        pipeline
            .nodes
            .iter_mut()
            .find(|n| n.id == format!("integrated_{name}"))
            .unwrap()
            .operator = TaskOperatorV1::Verify {
            slot: format!("full_{name}"),
        };
        packages.push((worker.name, files));
    }
    configure_scatter("native_scatter", pipeline, packages);
    let files = &mut packages
        .iter_mut()
        .find(|(n, _)| n == "fixture/bugs")
        .unwrap()
        .1;
    files.insert("worker.py".into(),br#"import json,sys
r=json.load(sys.stdin)
p=r['inputs']['slice'][0]['payload']['paths'][0]
f={'severity':'minor','file':p,'line':1,'title':'Native shard claim','body':'The source needs the declared repair','fix':'Apply the proposed edit','confidence':1.0}
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'reports':[f],'benchmark_demands':[],'dispositions':[]}]}}))
"#.to_vec());
    let files = &mut packages
        .iter_mut()
        .find(|(n, _)| n == "fixture/propose_bugs")
        .unwrap()
        .1;
    let mut worker: TaskWorkerManifest =
        toml::from_str(std::str::from_utf8(&files["worker.toml"]).unwrap()).unwrap();
    worker
        .signature
        .contract
        .inputs
        .get_mut("result")
        .unwrap()
        .artifact_type = af_contracts::contract::SHARD_SET_V1.into();
    files.insert(
        "worker.toml".into(),
        toml::to_string(&worker).unwrap().into_bytes(),
    );
    let script = String::from_utf8(files["worker.py"].clone())
        .unwrap()
        .replace(
            "assert len(r['inputs']['result'][0]['payload']['reports'])==1",
            "assert len(r['inputs']['result'][0]['payload']['shards'])==2",
        )
        .replace(
            "['selected_results'][\"bugs\"]",
            "['selected_results'][\"bugs_slice1\"]",
        );
    files.insert("worker.py".into(), script.into_bytes());
    let prepare = pipeline
        .nodes
        .iter_mut()
        .find(|n| n.id == "prepare")
        .unwrap();
    prepare.operator = TaskOperatorV1::ReviewIntegrate {
        reviewers: BTreeSet::from(["bugs_slice1".into(), "correctness".into()]),
        protected_paths: vec![],
    };
    for port in ["candidate", "proposal"] {
        let value = prepare.inputs.remove(&format!("{port}_bugs")).unwrap();
        prepare.inputs.insert(format!("{port}_bugs_slice1"), value);
    }
    pipeline.max_attempts = 9;
}

#[test]
fn native_sealed_proposals_preserve_additions_deletions_and_executable_modes() {
    run_case("native_integration_manifest_changes");
}

/// Exercise actual compiler output, including all seven new native operator discriminants.
/// A locally closed registry prevents network resolution from hiding a missing schema reference.
pub(super) fn assert_current_plan_schemas(
    pipeline: &PipelineDefinition,
    graph: &af_graph::task::CompiledTask,
) {
    for (file, value, retired) in [
        (
            "pipeline-definition-v6.json",
            serde_json::to_value(pipeline).unwrap(),
            "af.pipeline/5",
        ),
        (
            "compiled-task-v6.json",
            serde_json::to_value(graph).unwrap(),
            "af.compiled-task/4",
        ),
    ] {
        let validator = schema_registry::validator(file);
        let errors: Vec<_> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{file}: {errors:?}");
        let mut old = value.clone();
        old["schema"] = json!(retired);
        assert!(!validator.is_valid(&old));
        if file.starts_with("compiled") {
            for reference in [
                "urn:af:schema:task-inspection:20#/properties/graph",
                "urn:af:schema:task-plan-inspection:6#/properties/graph",
            ] {
                let enclosing = schema_registry::validator(reference);
                assert!(enclosing.is_valid(&value), "{reference}");
                assert!(
                    !enclosing.is_valid(&old),
                    "{reference} admitted the retired graph"
                );
                let mut open = value.clone();
                open["unadmitted_authority"] = json!(true);
                assert!(!enclosing.is_valid(&open), "{reference} opened its graph");
            }
        }
        let operators: Vec<_> = if let Some(nodes) = value["nodes"].as_array() {
            nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    n["operator"]["op"]
                        .as_str()
                        .is_some_and(|op| op.starts_with("review_"))
                })
                .map(|(i, _)| i.to_string())
                .collect()
        } else {
            value["nodes"]
                .as_object()
                .unwrap()
                .iter()
                .filter(|(_, n)| {
                    n["operator"]["operator"]["op"]
                        .as_str()
                        .is_some_and(|op| op.starts_with("review_"))
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        assert!(!operators.is_empty());
        for node in operators {
            let mut changed = value.clone();
            let operator = if file.starts_with("pipeline") {
                &mut changed["nodes"][node.parse::<usize>().unwrap()]["operator"]
            } else {
                &mut changed["nodes"][&node]["operator"]["operator"]
            };
            operator["unadmitted_authority"] = json!(true);
            assert!(
                !validator.is_valid(&changed),
                "{file} accepted an open native operator {node}"
            );
        }
    }
}

pub(super) fn embed(
    case: &str,
    pipeline: &mut PipelineDefinition,
    packages: &mut Vec<(String, BTreeMap<String, Vec<u8>>)>,
) {
    if !case.contains("_embedded_") {
        return;
    }
    use af_contracts::task::verification::REVIEWED_IMPLEMENTATION_V1;
    let integrated = case.starts_with("native_integration") && !case.ends_with("no_successor");
    pipeline.accepts.kinds = BTreeSet::from(["implementation".into(), "verification".into()]);
    let mut child = pipeline.clone();
    child.name = "fixture/native-review".into();
    let affinity = if integrated {
        PortAffinityV1::Unbound {}
    } else {
        same()
    };
    child.contract.outputs.insert(
        "checks".into(),
        port(TASK_CHECK_RECEIPT_V2, affinity.clone()),
    );
    if integrated {
        for (id, passed, failed, output) in [
            (
                "select_source",
                later_source(case),
                root("source"),
                "snapshot",
            ),
            (
                "select_checks",
                later_checks(case),
                from("check", "result"),
                "checks",
            ),
        ] {
            let passed = if case.ends_with("condition_only") {
                failed.clone()
            } else if case.ends_with("selected_old") {
                let inner = format!("{id}_inner");
                let mut select = node(
                    &inner,
                    TaskOperatorV1::Select {},
                    BTreeMap::from([
                        ("condition".into(), from("integrated_reduce", "result")),
                        ("passed".into(), passed),
                        ("failed".into(), failed.clone()),
                        ("inconclusive".into(), failed.clone()),
                    ]),
                );
                conditional(&mut select, "prepare");
                child.nodes.push(select);
                from(&inner, "output")
            } else {
                passed
            };
            child.nodes.push(node(
                id,
                TaskOperatorV1::Select {},
                BTreeMap::from([
                    ("condition".into(), from("prepare", "result")),
                    ("passed".into(), passed),
                    ("failed".into(), failed.clone()),
                    ("inconclusive".into(), failed),
                ]),
            ));
            child.outputs.insert(output.into(), from(id, "output"));
        }
        child
            .contract
            .outputs
            .insert("snapshot".into(), port(SOURCE_TREE_V1, affinity.clone()));
    } else {
        child
            .outputs
            .insert("checks".into(), from("check", "result"));
    }
    packages.push((
        child.name.clone(),
        BTreeMap::from([(
            "pipeline.toml".into(),
            toml::to_string(&child).unwrap().into_bytes(),
        )]),
    ));
    let mut reviewed = port(REVIEWED_IMPLEMENTATION_V1, affinity.clone());
    reviewed.covers.insert("reviewed".into());
    pipeline.name = "fixture/embedded-review".into();
    pipeline.contract.outputs = BTreeMap::from([
        ("snapshot".into(), port(SOURCE_TREE_V1, affinity)),
        ("review".into(), reviewed),
    ]);
    pipeline.slots.clear();
    pipeline.nodes = vec![
        node(
            "child",
            TaskOperatorV1::Call {
                pipeline: child.name,
                bindings: BTreeMap::new(),
            },
            pipeline
                .contract
                .inputs
                .keys()
                .map(|name| (name.clone(), root(name)))
                .collect(),
        ),
        node(
            "review_accept",
            TaskOperatorV1::ReviewAccept {},
            BTreeMap::from([
                (
                    "source".into(),
                    if integrated {
                        from("child", "snapshot")
                    } else {
                        root("source")
                    },
                ),
                ("review".into(), from("child", "review")),
                ("checks".into(), from("child", "checks")),
            ]),
        ),
    ];
    pipeline.outputs = BTreeMap::from([
        ("snapshot".into(), from("review_accept", "snapshot")),
        ("review".into(), from("review_accept", "result")),
    ]);
    pipeline.coverage = BTreeMap::from([("reviewed".into(), from("review_accept", "result"))]);
}

pub(super) fn embedded_task(case: &str, task: &mut TaskRevisionV1) {
    if !case.contains("_embedded_") {
        return;
    }
    use af_contracts::task::verification::REVIEWED_IMPLEMENTATION_V1;
    task.kind = if case.ends_with("verification") {
        "verification"
    } else {
        "implementation"
    }
    .into();
    task.required_outputs = BTreeMap::from([
        (
            "snapshot".into(),
            RequiredOutputV1 {
                artifact_type: SOURCE_TREE_V1.into(),
                cardinality: PortCardinality::One,
            },
        ),
        (
            "review".into(),
            RequiredOutputV1 {
                artifact_type: REVIEWED_IMPLEMENTATION_V1.into(),
                cardinality: PortCardinality::One,
            },
        ),
    ]);
    task.acceptance.get_mut("reviewed").unwrap().evidence_type = REVIEWED_IMPLEMENTATION_V1.into();
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assert_embedded(
    case: &str,
    cas: &Cas,
    domain: &ReviewTaskDomain,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    result: &TaskResultV1,
    report: &af_graph::RunReport,
    directory: &std::path::Path,
) {
    use af_contracts::task::verification::ReviewedImplementationV1;
    let receipt: ReviewedImplementationV1 = serde_json::from_value(
        cas.get_artifact(
            &result
                .outputs
                .get("review")
                .unwrap_or_else(|| {
                    panic!(
                        "Embedded Review acceptance did not publish its exact receipt: {report:?}"
                    )
                })
                .artifact_ids[0],
        )
        .unwrap()
        .payload,
    )
    .unwrap();
    assert_eq!(
        result.acceptance,
        if case.ends_with("failed") || case.ends_with("missing") || case.ends_with("incomplete") {
            TaskAcceptanceV1::Inconclusive
        } else if case.ends_with("blocking") {
            TaskAcceptanceV1::Unsatisfied
        } else {
            TaskAcceptanceV1::Satisfied
        }
    );
    assert_eq!(
        receipt.outcome,
        if case.ends_with("failed") || case.ends_with("missing") || case.ends_with("incomplete") {
            ReceiptOutcomeV1::Inconclusive
        } else if case.ends_with("blocking") {
            ReceiptOutcomeV1::Failed
        } else {
            ReceiptOutcomeV1::Passed
        }
    );
    assert_eq!(
        state.execution.as_ref().unwrap().budget.begun_attempts(),
        if case.ends_with("later") {
            10 + 2 * u64::from(case.contains("_descendant_later"))
        } else if case.starts_with("native_integration") {
            8
        } else if case.ends_with("missing") {
            3
        } else {
            4
        }
    );
    assert_eq!(
        result.outputs["snapshot"].snapshot_id.as_ref(),
        Some(&receipt.snapshot_id)
    );
    if case.ends_with("later") {
        let execution = state.execution.as_ref().unwrap();
        let (_, (_, output)) = execution
            .outputs
            .iter()
            .find(|(n, _)| n.ends_with(".later_reduce"))
            .unwrap();
        let round: TaskReviewRoundV1 = serde_json::from_value(
            cas.get_artifact(&output.outputs["result"].artifact_ids[0])
                .unwrap()
                .payload,
        )
        .unwrap();
        assert_eq!(round.snapshot_id, receipt.snapshot_id);
        assert_eq!(
            round.invocation.inputs["source"].snapshot_id.as_ref(),
            Some(&receipt.snapshot_id)
        );
        if case.contains("_descendant_later") {
            let (_, (_, output)) = execution
                .outputs
                .iter()
                .find(|(n, _)| n.ends_with(".commit"))
                .unwrap();
            let commit: TaskReviewCommitV1 = serde_json::from_value(
                cas.get_artifact(&output.outputs["result"].artifact_ids[0])
                    .unwrap()
                    .payload,
            )
            .unwrap();
            assert_ne!(round.snapshot_id, commit.snapshot_id);
            assert!(
                af_source_git::task::descends_from(cas, &round.snapshot_id, &commit.snapshot_id)
                    .unwrap()
            );
        }
    }
    if case.starts_with("native_integration") {
        let mut forged = result.clone();
        forged.acceptance = TaskAcceptanceV1::Satisfied;
        if case.ends_with("blocking") {
            assert_eq!(result.domain_conclusion, "changes_requested");
            forged.domain_conclusion = "verified".into();
            assert!(
                af_store::store::task::review_integration::require_native_successors(
                    cas, state, &forged
                )
                .is_err()
            );
            assert!(
                domain
                    .validate_result(cas, &state.revision, &forged)
                    .is_err()
            );
            let id = cas
                .put_artifact(
                    TASK_RESULT_V1,
                    producer(),
                    vec![],
                    None,
                    serde_json::to_value(&forged).unwrap(),
                )
                .unwrap()
                .0;
            assert!(runtime.finish(&id).is_err());
            return assert_terminal_projection(cas, domain, runtime, state, result, directory);
        }
        if case.ends_with("incomplete") {
            assert!(
                af_store::store::task::review_integration::require_native_successors(
                    cas, state, &forged
                )
                .is_err()
            );
            return assert_terminal_projection(cas, domain, runtime, state, result, directory);
        }
        af_store::store::task::review_integration::require_native_successors(cas, state, result)
            .unwrap();
        // Verification can retain Review only through its public Snapshot, with separate
        // requirements evidence occupying coverage. Exercise that exact receipt path too.
        let mut snapshot_only = state.clone();
        snapshot_only
            .execution
            .as_mut()
            .unwrap()
            .graph
            .coverage
            .clear();
        af_store::store::task::review_integration::require_native_successors(
            cas,
            &snapshot_only,
            result,
        )
        .unwrap();
        let execution = snapshot_only.execution.as_mut().unwrap();
        let address = execution.graph.outputs["snapshot"].clone();
        let original = execution.graph.inputs["source"].clone();
        execution
            .outputs
            .get_mut(&address.node)
            .unwrap()
            .1
            .outputs
            .insert(address.port, original);
        assert!(
            af_store::store::task::review_integration::require_native_successors(
                cas,
                &snapshot_only,
                result
            )
            .is_err()
        );
        let mut broken = state.clone();
        broken
            .execution
            .as_mut()
            .unwrap()
            .outputs
            .remove("root.nodes.child.nodes.integrated_reduce");
        assert!(
            af_store::store::task::review_integration::require_native_successors(
                cas, &broken, &forged
            )
            .is_err()
        );
    }
    assert_replay_and_finish(cas, domain, runtime, state, result, report, directory);
}

fn assert_terminal_projection(
    cas: &Cas,
    domain: &ReviewTaskDomain,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    result: &TaskResultV1,
    directory: &std::path::Path,
) {
    domain
        .validate_result(cas, &state.revision, result)
        .unwrap();
    let id = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    runtime.finish(&id).unwrap();
    let reopened = EventStore::open_read_only(directory.join("events.sqlite"))
        .unwrap()
        .task_projection(cas, &state.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        reopened.execution.as_ref().unwrap().outputs,
        state.execution.as_ref().unwrap().outputs
    );
    assert_eq!(
        reopened.execution.as_ref().unwrap().budget.begun_attempts(),
        state.execution.as_ref().unwrap().budget.begun_attempts()
    );
    assert!(matches!(reopened.phase, TaskPhaseV1::Finished { .. }));
}

pub(super) fn assert_source_transport(
    cas: &Cas,
    domain: &ReviewTaskDomain,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    result: &TaskResultV1,
    report: &af_graph::RunReport,
    directory: &std::path::Path,
) {
    let execution = state.execution.as_ref().unwrap();
    assert!(!execution.outputs.keys().any(|n| n.ends_with(".commit")));
    assert_eq!(
        result.outputs["snapshot"].snapshot_id,
        state.revision.inputs["source"].snapshot_id
    );
    assert!(matches!(
        cas.get_artifact(&state.revision.inputs["source"].artifact_ids[0])
            .unwrap()
            .producer,
        Producer::KernelOperation {
            node_id: Some(_),
            ..
        }
    ));
    if state.revision.kind == "review" {
        assert_eq!(result.outputs["snapshot"], state.revision.inputs["source"]);
    } else {
        assert_ne!(result.outputs["snapshot"], state.revision.inputs["source"]);
    }
    assert_eq!(result.acceptance, TaskAcceptanceV1::Satisfied);
    assert_replay_and_finish(cas, domain, runtime, state, result, report, directory);
}

pub(super) fn assert_rejected_later_source(
    cas: &Cas,
    domain: &ReviewTaskDomain,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    report: &af_graph::RunReport,
    directory: &std::path::Path,
) {
    let execution = state.execution.as_ref().unwrap();
    let (_, (_, output)) = execution
        .outputs
        .iter()
        .find(|(n, _)| n.ends_with(".later_reduce"))
        .unwrap();
    let round: TaskReviewRoundV1 = serde_json::from_value(
        cas.get_artifact(&output.outputs["result"].artifact_ids[0])
            .unwrap()
            .payload,
    )
    .unwrap();
    assert_eq!(round.conclusion, ReviewConclusionV1::Pass);
    assert_eq!(round.round, 3);
    assert!(
        domain.result(cas, state, report).is_err(),
        "accepted later Round on a source outside the committed lineage"
    );
    assert_forged_success_refused(cas, runtime, state, directory);
}

fn assert_forged_success_refused(
    cas: &Cas,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    directory: &std::path::Path,
) {
    let execution = state.execution.as_ref().unwrap();
    let mut forged = TaskResultV1 {
        task_revision_id: state.revision_id.clone(),
        execution: TaskExecutionV1::Completed,
        acceptance: TaskAcceptanceV1::Satisfied,
        domain_conclusion: if state.revision.kind == "review" {
            "pass"
        } else {
            "verified"
        }
        .into(),
        outputs: execution
            .graph
            .outputs
            .iter()
            .map(|(name, a)| {
                (
                    name.clone(),
                    execution.outputs[&a.node].1.outputs[&a.port].clone(),
                )
            })
            .collect(),
        evidence: execution
            .graph
            .coverage
            .values()
            .flat_map(|a| {
                execution.outputs[&a.node].1.outputs[&a.port]
                    .artifact_ids
                    .clone()
            })
            .collect(),
        missing_obligations: BTreeSet::new(),
    };
    if state.revision.kind == "review" {
        // Forge the entire otherwise valid conclusion, including exact accounting and prefix.
        // Missing conclusion evidence must not be the reason finish/replay refuses this head.
        let value = TaskReviewConclusionV1 {
            task_revision_id: state.revision_id.clone(),
            plan_id: state.plan_id.clone().unwrap(),
            task_report_id: state.run_reports.last().unwrap().clone(),
            through_sequence: state.next_sequence,
            cumulative_chargeable_tokens: execution.budget.committed_tokens().into(),
            begun_attempts: execution.budget.begun_attempts(),
            round_ids: execution
                .graph
                .coverage
                .values()
                .filter_map(|a| {
                    execution
                        .outputs
                        .get(&a.node)
                        .and_then(|(_, o)| o.outputs.get(&a.port))
                })
                .filter(|p| p.artifact_type == TASK_REVIEW_ROUND_V1)
                .flat_map(|p| p.artifact_ids.clone())
                .collect(),
            conclusion: ReviewConclusionV1::Pass,
        };
        value.validate().unwrap();
        let id = cas
            .put_artifact(
                TASK_REVIEW_CONCLUSION_V1,
                Producer::KernelOperation {
                    run_id: af_store::store::task::task_run_id(&state.task_id).unwrap(),
                    node_id: None,
                    operation_id: "native-review-conclusion@1".into(),
                },
                value.artifact_refs(),
                None,
                serde_json::to_value(value).unwrap(),
            )
            .unwrap()
            .0;
        forged.evidence.insert(id);
    }
    let id = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&forged).unwrap(),
        )
        .unwrap()
        .0;
    let error = runtime.finish(&id).unwrap_err().to_string();
    assert!(
        error.contains("Snapshot ancestry") || error.contains("complete exact committed successor"),
        "{error}"
    );
    let reopened = EventStore::open_read_only(directory.join("events.sqlite"))
        .unwrap()
        .task_projection(cas, &state.task_id)
        .unwrap()
        .unwrap();
    assert!(
        af_store::store::task::review_integration::require_native_successors(
            cas, &reopened, &forged
        )
        .is_err()
    );
    assert_eq!(
        reopened.execution.as_ref().unwrap().outputs,
        execution.outputs
    );
    // Damage the private fixture log with an otherwise valid terminal event, bypassing the
    // normal finish API. Reopening must run the same Snapshot check during actual replay.
    let run = af_store::store::task::task_run_id(&state.task_id).unwrap();
    let reader = EventStore::open_read_only(directory.join("events.sqlite")).unwrap();
    let events = reader.replay(&run).unwrap();
    let last = events
        .iter()
        .rev()
        .find(|e| e.event_type == af_contracts::EventType::TaskTransitionV6)
        .unwrap();
    let transition = af_contracts::task::event::TaskTransitionV1 {
        writer: last.payload["writer"].as_str().unwrap().into(),
        epoch: last.payload["epoch"].as_u64().unwrap(),
        now_unix_ms: last.payload["now_unix_ms"].as_u64().unwrap(),
        change: af_contracts::task::event::TaskChangeV1::Finished {
            result_id: id.clone(),
        },
    };
    let sequence = events.last().unwrap().sequence + 1;
    let hash = std::process::Command::new("/usr/bin/python3").args([
        "-B", "-c", "import hashlib,sys; print(hashlib.sha256(b'review.kernel/event-id/v1\\0'+sys.argv[1].encode()+b'\\0'+sys.argv[2].encode()).hexdigest()[:26])",
        &run, &sequence.to_string(),
    ]).output().unwrap();
    assert!(hash.status.success());
    let event_id = String::from_utf8(hash.stdout).unwrap().trim().to_owned();
    let mut references = BTreeSet::from([id.clone(), state.revision_id.clone()]);
    references.extend(forged.evidence.iter().cloned());
    for port in forged.outputs.values() {
        for id in &port.artifact_ids {
            references.insert(id.clone());
            references.extend(cas.get_artifact(id).unwrap().input_artifacts);
        }
        references.extend(port.snapshot_id.iter().cloned());
    }
    let connection = rusqlite::Connection::open(directory.join("events.sqlite")).unwrap();
    connection.execute("INSERT INTO events (run_id,sequence,event_id,type,occurred_at,artifact_refs,payload) VALUES (?1,?2,?3,'TaskTransition@6',?4,?5,?6)", rusqlite::params![run, i64::try_from(sequence).unwrap(), event_id, last.occurred_at, serde_json::to_string(&references).unwrap(), serde_json::to_string(&transition).unwrap()]).unwrap();
    drop(connection);
    let error = EventStore::open_read_only(directory.join("events.sqlite"))
        .unwrap()
        .task_projection(cas, &state.task_id)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Snapshot ancestry") || error.contains("complete exact committed successor"),
        "{error}"
    );
}

pub(super) fn assert_discarded_successor(
    cas: &Cas,
    domain: &ReviewTaskDomain,
    runtime: &TaskRuntime<'_, '_>,
    state: &TaskProjection,
    report: &af_graph::RunReport,
    directory: &std::path::Path,
) {
    let execution = state.execution.as_ref().unwrap();
    let (name, (_, output)) = execution
        .outputs
        .iter()
        .find(|(name, _)| name.ends_with(".condition_only"))
        .unwrap();
    let prefix = name.strip_suffix("condition_only").unwrap();
    assert_eq!(
        output.outputs["output"],
        execution.outputs[&format!("{prefix}reduce")].1.outputs["result"]
    );
    let successor: TaskReviewRoundV1 = serde_json::from_value(
        cas.get_artifact(
            &execution.outputs[&format!("{prefix}integrated_reduce")]
                .1
                .outputs["result"]
                .artifact_ids[0],
        )
        .unwrap()
        .payload,
    )
    .unwrap();
    assert_eq!(
        successor.conclusion,
        ReviewConclusionV1::ConvergenceExhausted
    );
    assert_eq!(successor.outcome, ReceiptOutcomeV1::Failed);
    assert!(
        domain
            .result(cas, state, report)
            .unwrap_err()
            .contains("complete exact committed successor")
    );
    let forged = TaskResultV1 {
        task_revision_id: state.revision_id.clone(),
        execution: TaskExecutionV1::Completed,
        acceptance: TaskAcceptanceV1::Satisfied,
        domain_conclusion: if state.revision.kind == "review" {
            "pass"
        } else {
            "verified"
        }
        .into(),
        outputs: execution
            .graph
            .outputs
            .iter()
            .map(|(name, a)| {
                (
                    name.clone(),
                    execution.outputs[&a.node].1.outputs[&a.port].clone(),
                )
            })
            .collect(),
        evidence: execution
            .graph
            .coverage
            .values()
            .flat_map(|a| {
                execution.outputs[&a.node].1.outputs[&a.port]
                    .artifact_ids
                    .clone()
            })
            .collect(),
        missing_obligations: BTreeSet::new(),
    };
    assert!(
        af_store::store::task::review_integration::require_native_successors(cas, state, &forged)
            .is_err()
    );
    let id = cas
        .put_artifact(
            TASK_RESULT_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&forged).unwrap(),
        )
        .unwrap()
        .0;
    assert!(runtime.finish(&id).is_err());
    for acceptance in [
        TaskAcceptanceV1::Unsatisfied,
        TaskAcceptanceV1::Inconclusive,
    ] {
        let mut contradictory = forged.clone();
        contradictory.acceptance = acceptance;
        assert!(
            af_store::store::task::review_integration::require_native_successors(
                cas,
                state,
                &contradictory
            )
            .is_err()
        );
    }
    let reopened = EventStore::open_read_only(directory.join("events.sqlite"))
        .unwrap()
        .task_projection(cas, &state.task_id)
        .unwrap()
        .unwrap();
    assert!(
        af_store::store::task::review_integration::require_native_successors(
            cas, &reopened, &forged
        )
        .is_err()
    );
    assert_eq!(
        reopened.execution.as_ref().unwrap().outputs,
        execution.outputs
    );
}
