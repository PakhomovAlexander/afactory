//! The trusted CI Pipeline exception (ADR-0141) through the public compiler, the Store capture
//! and the code check operator of a real Task: only the selected root Pipeline that the Task's
//! captured run authority pins with the exact tag `ci` sends a candidate that changes
//! `.github/`; every record of that phase names the exception, and every reader — the recording
//! domain, a reopened one, one built without the capture — recomputes it from the Snapshots.

use super::task::{Recorded, Shape, mapped, policy, run_with};
use super::*;
use review_check::CheckStatus;
use review_core::task::EXECUTION_PLAN_V1;
use review_core::task::pipeline::ReceiptOutcomeV1;
use review_core::task::remote_check::{
    REMOTE_CHECK_EVIDENCE_V1, RemoteCheckEvidenceV1, RemoteTrustedCiV1,
};
use review_pipeline::task::code::CodeTaskDomain;
use review_pipeline::task::remote_check::{RemoteCheckHost, TrustedCiPipeline};

const WORKFLOW: &str = ".github/workflows/ci.yml";
const CHANGED_WORKFLOW: &[u8] = b"on: pull_request\njobs:\n  changed: {}\n";
/// The candidate rewrites the workflow that will judge it.
const CHANGES_WORKFLOW: &[(&str, &[u8])] = &[(WORKFLOW, CHANGED_WORKFLOW)];
/// The candidate leaves `.github/` alone.
const SOURCE_ONLY: &[(&str, &[u8])] = &[("src/lib.txt", b"version 2\n")];

/// A Task as the coordinator builds it: a run authority pins the Pipelines, and the domain
/// receives the capture of the admitted plan.
fn coordinated<'a>(tags: &'a [&'a str], changes: &'a [(&'a str, &'a [u8])]) -> Shape<'a> {
    Shape {
        tags,
        run_authority: true,
        changes,
        capture: true,
        ..Shape::default()
    }
}

/// What the fake GitHub serves, and the state and reason the evidence must then derive.
type Case<'a> = (
    &'a str,
    &'a dyn Fn(&Remote),
    RemoteCheckStateV1,
    Option<RemoteCheckReasonV1>,
);

fn passing_remote() -> Remote {
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    remote
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

/// The exception this recorded Task's plan grants, as evidence names it.
fn granted(recorded: &Recorded, pipeline: &str) -> RemoteTrustedCiV1 {
    RemoteTrustedCiV1 {
        tag: "ci".into(),
        authority_id: recorded.authority_id.clone(),
        plan_id: recorded.plan_id.clone(),
        pipeline: pipeline.into(),
        pipeline_id: recorded.pipeline_id.clone(),
    }
}

/// The recorded Task's code domain, rebuilt from its Store the way a resume rebuilds it, with
/// or without the coordinator's capture.
fn reopened(recorded: &Recorded, cas: &Cas, capture: bool) -> CodeTaskDomain {
    let trusted = if capture {
        TrustedCiPipeline::capture(cas, &recorded.plan_id).unwrap()
    } else {
        None
    };
    CodeTaskDomain::captured(cas, &recorded.policy_id, recorded.graph.clone())
        .unwrap()
        .with_trusted_ci(trusted)
        .unwrap()
}

/// The recorded receipt with its `kernel` result pointing at edited evidence for `snapshot`.
fn forged_receipt(
    recorded: &Recorded,
    snapshot: &str,
    edit: &dyn Fn(&mut RemoteCheckEvidenceV1),
) -> review_core::task::verification::TaskCheckReceiptV1 {
    let (_, kernel) = recorded.result("kernel");
    let mut evidence = recorded.evidence("kernel");
    evidence.snapshot_id = snapshot.into();
    edit(&mut evidence);
    let evidence_id = recorded
        .cas
        .put_artifact(
            REMOTE_CHECK_EVIDENCE_V1,
            producer(),
            vec![],
            Some(snapshot.to_string()),
            serde_json::to_value(&evidence).unwrap(),
        )
        .unwrap()
        .0;
    let mut result = serde_json::to_value(&kernel).unwrap();
    result["remote"] = json!(evidence_id);
    let result_id = recorded.cas.put_json(&result).unwrap();
    let mut receipt = recorded.receipt().clone();
    receipt.snapshot_id = snapshot.into();
    receipt.checks.insert("kernel".into(), result_id);
    receipt
}

#[test]
fn a_pinned_ci_tagged_root_sends_its_changed_workflow_and_binds_the_evidence() {
    let remote = passing_remote();
    let recorded = run_with(
        &policy(true, true),
        &["fmt", "kernel"],
        mapped(&remote, remote.mapping_text(&["kernel"])),
        coordinated(&["ci"], CHANGES_WORKFLOW),
    );
    assert_eq!(recorded.receipt().outcome, ReceiptOutcomeV1::Passed);
    let (_, kernel) = recorded.result("kernel");
    assert_eq!(kernel.status, CheckStatus::Passed);
    let evidence = recorded.evidence("kernel");
    assert_eq!(evidence.state, RemoteCheckStateV1::Observed);
    assert_eq!(evidence.reason, None);
    assert_eq!(
        evidence.trusted_ci,
        Some(granted(&recorded, "fixture/remote-checks"))
    );
    // The base is the Task's source, recomputed along the candidate's lineage.
    assert_eq!(
        evidence.source_snapshot_id,
        Snapshots::new(&recorded.cas).source_id
    );
    // GitHub ran the changed workflow: the gate head carries it, the base the source's.
    let head = remote.branch("head").unwrap();
    let base = remote.branch("base").unwrap();
    assert_eq!(evidence.head_commit.as_deref(), Some(head.as_str()));
    assert_eq!(evidence.base_commit.as_deref(), Some(base.as_str()));
    assert_eq!(
        git(&remote.bare, &["show", &format!("{head}:{WORKFLOW}")]),
        String::from_utf8_lossy(CHANGED_WORKFLOW).trim()
    );
    assert_eq!(
        git(&remote.bare, &["show", &format!("{base}:{WORKFLOW}")]),
        "on: pull_request\njobs: {}"
    );
    // Every required job of the run is bound, in declared order.
    assert_eq!(
        evidence
            .jobs
            .iter()
            .map(|job| (job.name.as_str(), job.conclusion.as_str()))
            .collect::<Vec<_>>(),
        [(LINT, "success"), (CHECK, "success")]
    );

    // The recorded document validates against the schema and round-trips exactly.
    let schema: serde_json::Value = serde_json::from_slice(
        &std::fs::read(workspace_root().join("schemas/remote-check-evidence-v1.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::options().build(&schema).unwrap();
    let artifact = recorded
        .cas
        .get_artifact(kernel.remote.as_ref().unwrap())
        .unwrap();
    assert!(
        validator.is_valid(&artifact.payload),
        "{}",
        artifact.payload
    );
    assert_eq!(serde_json::to_value(&evidence).unwrap(), artifact.payload);

    // The recording reader and a reopened one, from the Store on disk, agree.
    assert_eq!(
        recorded
            .domain
            .check_receipt_outcome(&recorded.cas, recorded.receipt()),
        Ok(ReceiptOutcomeV1::Passed)
    );
    let cas = Cas::open_existing(recorded.directory.path().join("cas")).unwrap();
    assert_eq!(
        TrustedCiPipeline::capture(&cas, &recorded.plan_id)
            .unwrap()
            .map(|trusted| trusted.record().clone()),
        Some(granted(&recorded, "fixture/remote-checks"))
    );
    let store =
        review_store::EventStore::open_read_only(recorded.directory.path().join("events.sqlite"))
            .unwrap();
    let projection = store.task_projection(&cas, TASK).unwrap().unwrap();
    let stored = projection
        .execution
        .as_ref()
        .unwrap()
        .outputs
        .get("root.nodes.check")
        .map(|(_, output)| output.outputs["result"].artifact_ids[0].clone())
        .unwrap();
    let receipt: review_core::task::verification::TaskCheckReceiptV1 =
        serde_json::from_value(cas.get_artifact(&stored).unwrap().payload).unwrap();
    assert_eq!(&receipt, recorded.receipt());
    assert_eq!(
        reopened(&recorded, &cas, true).check_receipt_outcome(&cas, &receipt),
        Ok(ReceiptOutcomeV1::Passed)
    );
    // A domain built without the coordinator's capture — a direct API path — fails closed.
    assert!(
        reopened(&recorded, &cas, false)
            .check_receipt_outcome(&cas, &receipt)
            .is_err()
    );
    // A capture belongs to the exact plan graph and code policy of the domain it is given to.
    let mut other_graph = recorded.graph.clone();
    other_graph.max_parallel += 1;
    let capture = TrustedCiPipeline::capture(&cas, &recorded.plan_id).unwrap();
    assert!(
        CodeTaskDomain::captured(&cas, &recorded.policy_id, other_graph)
            .unwrap()
            .with_trusted_ci(capture)
            .is_err()
    );
    // Nothing kept names the push URL or the mapping.
    let bytes = store_bytes(recorded.directory.path());
    assert!(!contains(&bytes, &remote.push_url()));
}

#[test]
fn a_reader_recomputes_the_exception_and_refuses_every_other_binding() {
    let remote = passing_remote();
    let recorded = run_with(
        &policy(true, true),
        &["fmt", "kernel"],
        mapped(&remote, remote.mapping_text(&["kernel"])),
        coordinated(&["ci"], CHANGES_WORKFLOW),
    );
    let candidate = recorded.candidate.clone();
    let read = |receipt| {
        recorded
            .domain
            .check_receipt_outcome(&recorded.cas, &receipt)
    };
    assert_eq!(
        read(forged_receipt(&recorded, &candidate, &|_| {})),
        Ok(ReceiptOutcomeV1::Passed),
        "the unedited forgery is the recorded evidence"
    );
    for (why, edit) in [
        (
            "a changed workflow sent without the exception",
            Box::new(|e: &mut RemoteCheckEvidenceV1| e.trusted_ci = None)
                as Box<dyn Fn(&mut RemoteCheckEvidenceV1)>,
        ),
        (
            "another run authority",
            Box::new(|e| e.trusted_ci.as_mut().unwrap().authority_id = digest('9')),
        ),
        (
            "another plan",
            Box::new(|e| e.trusted_ci.as_mut().unwrap().plan_id = digest('9')),
        ),
        (
            "another root Pipeline",
            Box::new(|e| {
                e.trusted_ci.as_mut().unwrap().pipeline = "fixture/remote-checks-sibling".into()
            }),
        ),
        (
            "another root package",
            Box::new(|e| e.trusted_ci.as_mut().unwrap().pipeline_id = digest('9')),
        ),
        (
            "a stale source Snapshot",
            Box::new(|e| e.source_snapshot_id = digest('9')),
        ),
        (
            "the candidate as its own source",
            Box::new(|e| e.source_snapshot_id = e.snapshot_id.clone()),
        ),
    ] {
        assert!(
            read(forged_receipt(&recorded, &candidate, &*edit)).is_err(),
            "the reader accepted {why}"
        );
    }
    // Evidence of the exception moved onto a candidate that changes no workflow is refused:
    // the exception exists only for a changed `.github/`.
    let (unchanged, _) = Snapshots::new(&recorded.cas).candidate(&recorded.cas, "version 3\n");
    assert!(read(forged_receipt(&recorded, &unchanged, &|_| {})).is_err());
    // A receipt of another plan cannot carry this plan's exception.
    let mut replanned = forged_receipt(&recorded, &candidate, &|_| {});
    replanned.plan_id = digest('8');
    assert!(read(replanned).is_err());
}

#[test]
fn untagged_and_lookalike_roots_never_send_a_changed_workflow() {
    let remote = passing_remote();
    for tags in [
        &[][..],
        &["CI"][..],
        &["cicd"][..],
        &["Ci", "ci-trusted", "trusted-ci"][..],
        &["release"][..],
    ] {
        let recorded = run_with(
            &policy(true, true),
            &["fmt", "kernel"],
            mapped(&remote, remote.mapping_text(&["kernel"])),
            coordinated(tags, CHANGES_WORKFLOW),
        );
        assert_eq!(
            TrustedCiPipeline::capture(&recorded.cas, &recorded.plan_id).unwrap(),
            None,
            "{tags:?}"
        );
        let evidence = recorded.evidence("kernel");
        assert_eq!(
            (evidence.state, evidence.reason),
            (
                RemoteCheckStateV1::Refused,
                Some(RemoteCheckReasonV1::RemoteCandidateChangesCi)
            ),
            "{tags:?}"
        );
        assert_eq!(evidence.trusted_ci, None);
        let (_, kernel) = recorded.result("kernel");
        assert_eq!(kernel.status, CheckStatus::NotRun);
        assert_eq!(
            recorded
                .domain
                .check_receipt_outcome(&recorded.cas, recorded.receipt()),
            Ok(ReceiptOutcomeV1::Inconclusive)
        );
    }
    assert_eq!(remote.refs(), "", "nothing was pushed");
    assert_eq!(remote.calls(), "", "gh was never called");
}

#[test]
fn only_the_coordinators_capture_of_the_selected_pinned_root_grants() {
    let remote = passing_remote();
    for (why, shape, captured) in [
        (
            "a tagged root the Task's authority does not pin",
            Shape {
                run_authority: false,
                ..coordinated(&["ci"], CHANGES_WORKFLOW)
            },
            false,
        ),
        (
            "a domain built without the coordinator's capture",
            Shape {
                capture: false,
                ..coordinated(&["ci"], CHANGES_WORKFLOW)
            },
            true,
        ),
        (
            "an embedded child tagged `ci` under an untagged root",
            Shape {
                wrapped: true,
                ..coordinated(&["ci"], CHANGES_WORKFLOW)
            },
            false,
        ),
        (
            "a pinned sibling tagged `ci` the Task never selects",
            Shape {
                sibling: true,
                ..coordinated(&[], CHANGES_WORKFLOW)
            },
            false,
        ),
    ] {
        let recorded = run_with(
            &policy(true, true),
            &["fmt", "kernel"],
            mapped(&remote, remote.mapping_text(&["kernel"])),
            shape,
        );
        assert_eq!(
            TrustedCiPipeline::capture(&recorded.cas, &recorded.plan_id)
                .unwrap()
                .is_some(),
            captured,
            "{why}"
        );
        let evidence = recorded.evidence("kernel");
        assert_eq!(
            (evidence.state, evidence.reason, evidence.trusted_ci),
            (
                RemoteCheckStateV1::Refused,
                Some(RemoteCheckReasonV1::RemoteCandidateChangesCi),
                None
            ),
            "{why}"
        );
    }
    assert_eq!(remote.refs(), "", "nothing was pushed");
    assert_eq!(remote.calls(), "", "gh was never called");
}

#[test]
fn tags_a_candidate_writes_cannot_grant() {
    let remote = passing_remote();
    // The candidate rewrites the workflow and also tags, in its own tree, the very Pipeline
    // the Task runs. The authority was captured before the candidate existed.
    let tagged =
        b"schema = \"af.pipeline/1\"\nname = \"fixture/remote-checks\"\nversion = \"1.0.0\"\n\
tags = [\"ci\"]\n";
    let catalog: &[u8] = b"# the candidate pins whatever it likes\n";
    let changes: &[(&str, &[u8])] = &[
        (WORKFLOW, CHANGED_WORKFLOW),
        ("package/pipeline.toml", &tagged[..]),
        (".af/task-catalog.toml", catalog),
    ];
    let recorded = run_with(
        &policy(true, true),
        &["fmt", "kernel"],
        mapped(&remote, remote.mapping_text(&["kernel"])),
        coordinated(&[], changes),
    );
    assert_eq!(
        TrustedCiPipeline::capture(&recorded.cas, &recorded.plan_id).unwrap(),
        None
    );
    let evidence = recorded.evidence("kernel");
    assert_eq!(
        (evidence.state, evidence.reason),
        (
            RemoteCheckStateV1::Refused,
            Some(RemoteCheckReasonV1::RemoteCandidateChangesCi)
        )
    );
    assert_eq!(remote.refs(), "");
}

#[test]
fn a_generated_or_preparation_plan_never_grants_even_when_its_root_is_tagged() {
    let recorded = run_with(
        &policy(true, true),
        &["fmt", "kernel"],
        |_: &Path| RemoteCheckHost::default(),
        coordinated(&["ci"], CHANGES_WORKFLOW),
    );
    // The plan as admitted grants.
    assert!(
        TrustedCiPipeline::capture(&recorded.cas, &recorded.plan_id)
            .unwrap()
            .is_some()
    );
    let plan = recorded
        .cas
        .get_artifact(&recorded.plan_id)
        .unwrap()
        .payload;
    let capture = |value: serde_json::Value| {
        let id = recorded
            .cas
            .put_artifact(EXECUTION_PLAN_V1, producer(), vec![], None, value)
            .unwrap()
            .0;
        TrustedCiPipeline::capture(&recorded.cas, &id).unwrap()
    };
    // A root a Planner proposed, however it tagged itself, is generated and never trusted.
    let mut generated = plan.clone();
    generated["generated_origins"] = json!([{
        "pipeline_id": recorded.pipeline_id,
        "proposal_id": digest('a'),
        "bootstrap_plan_id": digest('b'),
    }]);
    assert_eq!(capture(generated), None);
    // A Planner bootstrap claims nothing.
    let mut preparation = plan.clone();
    preparation["preparation"] = json!({"kind": "planning"});
    preparation["acceptance"] = json!({});
    assert_eq!(capture(preparation), None);
    // A plan whose recorded authority is not its revision's is not read as anyone's grant.
    let mut foreign = plan.clone();
    foreign["authority"]["policy_id"] = json!(recorded.policy_id);
    let id = recorded
        .cas
        .put_artifact(EXECUTION_PLAN_V1, producer(), vec![], None, foreign)
        .unwrap()
        .0;
    assert!(TrustedCiPipeline::capture(&recorded.cas, &id).is_err());
    // An identifier that is not an Execution Plan is refused.
    assert!(TrustedCiPipeline::capture(&recorded.cas, &recorded.authority_id).is_err());
}

#[test]
fn a_trusted_phase_still_needs_every_required_job_and_the_merge_proof() {
    // Each case: what the fake GitHub serves, then the state, reason and status it must derive.
    let cases: [Case<'_>; 5] = [
        (
            "a failed job",
            &|remote| {
                remote.serve_runs("runs-pull-request.json");
                remote.serve_jobs(77, 1, "jobs-failure.json");
            },
            RemoteCheckStateV1::Observed,
            None,
        ),
        (
            "a skipped job",
            &|remote| {
                remote.serve_runs("runs-pull-request.json");
                remote.serve_jobs(77, 1, "jobs-skipped.json");
            },
            RemoteCheckStateV1::Observed,
            Some(RemoteCheckReasonV1::RemoteCheckInconclusive),
        ),
        (
            "a completed run without a required job",
            &|remote| {
                remote.serve_runs("runs-pull-request.json");
                remote.serve_jobs(77, 1, "jobs-in-progress.json");
            },
            RemoteCheckStateV1::Published,
            Some(RemoteCheckReasonV1::RemoteCheckMissing),
        ),
        (
            "no run of the workflow at all",
            &|_| {},
            RemoteCheckStateV1::Published,
            Some(RemoteCheckReasonV1::RemoteCheckMissing),
        ),
        (
            "a merge ref with another tree than the candidate's",
            &|remote| {
                remote.serve_runs("runs-pull-request.json");
                remote.serve_jobs(77, 1, "jobs-success.json");
                remote.merge_mode("other-tree");
            },
            RemoteCheckStateV1::Published,
            Some(RemoteCheckReasonV1::RemoteMergeMismatch),
        ),
    ];
    for (why, serve, state, reason) in cases {
        let remote = Remote::new();
        serve(&remote);
        let recorded = run_with(
            &policy(true, true),
            &["fmt", "kernel"],
            mapped(&remote, remote.mapping_text(&["kernel"])),
            coordinated(&["ci"], CHANGES_WORKFLOW),
        );
        let evidence = recorded.evidence("kernel");
        assert_eq!((evidence.state, evidence.reason), (state, reason), "{why}");
        assert_eq!(
            evidence.trusted_ci,
            Some(granted(&recorded, "fixture/remote-checks")),
            "{why}: every record of a granted phase names the grant"
        );
        assert_ne!(
            recorded.receipt().outcome,
            ReceiptOutcomeV1::Passed,
            "{why}: a changed workflow is no guarantee by itself"
        );
        // The reader derives the same outcome the recording did.
        assert_eq!(
            recorded
                .domain
                .check_receipt_outcome(&recorded.cas, recorded.receipt()),
            Ok(recorded.receipt().outcome),
            "{why}"
        );
    }
}

#[test]
fn a_granted_phase_behind_a_failed_local_check_pushes_nothing() {
    let remote = passing_remote();
    let recorded = run_with(
        &policy(false, true),
        &["fmt", "kernel"],
        mapped(&remote, remote.mapping_text(&["kernel"])),
        coordinated(&["ci"], CHANGES_WORKFLOW),
    );
    let evidence = recorded.evidence("kernel");
    assert_eq!(
        (evidence.state, evidence.reason),
        (
            RemoteCheckStateV1::Refused,
            Some(RemoteCheckReasonV1::RemoteSkippedLocalFailed)
        )
    );
    assert_eq!(
        evidence.trusted_ci,
        Some(granted(&recorded, "fixture/remote-checks"))
    );
    assert_eq!(remote.refs(), "", "nothing was pushed");
    assert_eq!(remote.calls(), "", "gh was never called");
    assert_eq!(
        recorded
            .domain
            .check_receipt_outcome(&recorded.cas, recorded.receipt()),
        Ok(recorded.receipt().outcome)
    );
}

#[test]
fn a_granted_task_whose_candidate_keeps_the_workflow_records_no_exception() {
    let remote = passing_remote();
    let recorded = run_with(
        &policy(true, true),
        &["fmt", "kernel"],
        mapped(&remote, remote.mapping_text(&["kernel"])),
        coordinated(&["ci"], SOURCE_ONLY),
    );
    assert!(
        TrustedCiPipeline::capture(&recorded.cas, &recorded.plan_id)
            .unwrap()
            .is_some()
    );
    let evidence = recorded.evidence("kernel");
    assert_eq!(evidence.state, RemoteCheckStateV1::Observed);
    assert_eq!(evidence.trusted_ci, None, "the exception was not used");
    assert_eq!(recorded.receipt().outcome, ReceiptOutcomeV1::Passed);
}

#[test]
fn the_executor_sends_a_changed_workflow_only_with_a_capture() {
    // A capture as the coordinator makes it, from a recorded Task whose root is tagged `ci`.
    let granting = run_with(
        &policy(true, true),
        &["fmt", "kernel"],
        |_: &Path| RemoteCheckHost::default(),
        coordinated(&["ci"], CHANGES_WORKFLOW),
    );
    let trusted = TrustedCiPipeline::capture(&granting.cas, &granting.plan_id)
        .unwrap()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let snapshots = Snapshots::new(&cas);
    let candidate = snapshots.derived(&cas, WORKFLOW, EntryKind::File, CHANGED_WORKFLOW);
    let remote = passing_remote();
    let run = |trusted_ci| {
        phase_trusted(
            &cas,
            &remote,
            &snapshots,
            &candidate,
            OWNER,
            TASK,
            &[kernel_request(WORKFLOW)],
            Duration::from_secs(30),
            None,
            trusted_ci,
        )
        .unwrap()
        .remove(0)
    };
    let refused = run(None);
    expect(
        &refused,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemoteCandidateChangesCi),
    );
    assert!(refused.message.unwrap().contains("tagged `ci`"));
    assert_eq!(remote.refs(), "", "nothing was pushed without the capture");
    let sent = run(Some(&trusted));
    expect(&sent, RemoteCheckStateV1::Observed, None);
    assert_eq!(sent.evidence.trusted_ci.as_ref(), Some(trusted.record()));
    let head = remote.branch("head").unwrap();
    assert_eq!(
        git(&remote.bare, &["show", &format!("{head}:{WORKFLOW}")]),
        String::from_utf8_lossy(CHANGED_WORKFLOW).trim()
    );
    // A capture used on a candidate that changes nothing under `.github/` is not recorded.
    let unchanged = snapshots.candidate(&cas, "version 2\n");
    let plain = phase_trusted(
        &cas,
        &remote,
        &snapshots,
        &unchanged,
        OWNER,
        TASK,
        &[kernel_request(WORKFLOW)],
        Duration::from_secs(30),
        None,
        Some(&trusted),
    )
    .unwrap()
    .remove(0);
    expect(&plain, RemoteCheckStateV1::Observed, None);
    assert_eq!(plain.evidence.trusted_ci, None);
}
