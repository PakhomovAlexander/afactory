use super::*;

#[test]
fn restarted_prior_headers_follow_only_exact_adjacent_supersession() {
    let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
    let first = RoundStartedPayloadV1 {
        round: 1,
        epoch: 1,
        campaign_manifest_id: digest('a'),
        subject_id: digest('b'),
        prior_finding_set_id: digest('c'),
        prior_demand_set_id: digest('d'),
    };
    let mut next = first.clone();
    next.epoch = 2;
    next.subject_id = digest('e');
    let row = |sequence, event_type, payload, causation_id| review_core::RunEvent {
        event_id: format!("event-{sequence}"),
        run_id: "campaign".into(),
        sequence,
        event_type,
        occurred_at: "2026-09-12T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id,
        correlation_id: None,
        artifact_refs: vec![],
        payload,
    };
    let mut events = vec![
        row(
            0,
            EventType::RoundStartedV1,
            serde_json::to_value(&first).unwrap(),
            None,
        ),
        row(
            1,
            EventType::RoundInputSupersededV1,
            serde_json::to_value(RoundInputSupersededPayloadV1 {
                round: 1,
                old_epoch: 1,
                new_epoch: 2,
                campaign_manifest_id: first.campaign_manifest_id.clone(),
                old_subject_id: first.subject_id.clone(),
                replacement_subject_id: next.subject_id.clone(),
            })
            .unwrap(),
            Some("event-0".into()),
        ),
        row(
            2,
            EventType::RoundStartedV1,
            serde_json::to_value(&next).unwrap(),
            Some("event-0".into()),
        ),
    ];
    assert_eq!(
        original_prior_subject(&events, &events[2], &next).unwrap(),
        first.subject_id
    );
    let mut wrong_prior = next.clone();
    wrong_prior.prior_finding_set_id = digest('f');
    assert!(original_prior_subject(&events, &events[2], &wrong_prior).is_err());
    events[1].payload["old_subject_id"] = serde_json::json!(digest('f'));
    assert!(original_prior_subject(&events, &events[2], &next).is_err());
    events.remove(1);
    assert!(original_prior_subject(&events, &events[1], &next).is_err());
}

fn git(repo: &Path, home: &Path, args: &[&str]) -> Vec<u8> {
    let result = std::process::Command::new("git")
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

#[test]
fn recorded_closed_round_loads_pinned_authority_without_capture_or_advancing_light_campaign() {
    let directory = tempfile::tempdir().unwrap();
    let repo_path = directory.path().join("repo");
    let home = directory.path().join("home");
    std::fs::create_dir_all(repo_path.join(".af/pipelines")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let pipeline = r#"version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
outputs = ["result"]
runner = {program="/bin/true"}
[[nodes]]
id = "gather"
kind = "gather"
inputs = ["reviewer"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [
  {name="findings",type="review.kernel/FindingSet@1",cardinality="one",optional=false,snapshot_affinity="same_subject"},
  {name="demands",type="review.kernel/DemandSet@1",cardinality="one",optional=false,snapshot_affinity="same_subject"}
]
[[edges]]
from={node="reviewer",port="result"}
to={node="gather",port="reviewer"}
[[edges]]
from={node="gather",port="reports"}
to={node="ledger",port="reports"}
"#;
    std::fs::write(repo_path.join(".af/pipelines/review.toml"), pipeline).unwrap();
    std::fs::write(
        repo_path.join(".af/af.toml"),
        "version=1\n[project]\nname='fixture'\nmin_af='0.6'\n[defaults]\npipeline='review'\n",
    )
    .unwrap();
    let mut lock = Lockfile::empty();
    lock.pipelines.insert(
        "review".into(),
        review_config::lock::Pin {
            version: "1.0.0".into(),
            digest: review_store::canonical::blob_content_id(pipeline.as_bytes()),
        },
    );
    std::fs::write(repo_path.join(".af/af.lock"), lock.to_toml()).unwrap();
    std::fs::write(repo_path.join("source.txt"), "captured source").unwrap();
    git(&repo_path, &home, &["init", "-q"]);
    git(
        &repo_path,
        &home,
        &["config", "user.email", "fixture@example.test"],
    );
    git(&repo_path, &home, &["config", "user.name", "Fixture"]);
    git(&repo_path, &home, &["add", "."]);
    git(&repo_path, &home, &["commit", "-qm", "fixture"]);
    let options = Options {
        repo: repo_path.clone(),
        pipeline: ".af/pipelines/review.toml".into(),
        pipeline_explicit: true,
        state: Some(directory.path().join("state")),
        campaign: Some("recorded".into()),
        focus: None,
        policy_rev: Some("HEAD".into()),
        base: None,
        candidate: None,
        uncommitted: false,
        restart_round: false,
        mode: crate::CampaignMode::Light,
        timeout: None,
        git_timeout: None,
        provider_bindings: BTreeMap::new(),
        provider_admission: None,
        json: true,
        node: None,
    };
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let path = directory.path().join("events.sqlite");
    let mut store = EventStore::open(&path).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let first = prepare(&options, &cas, &mut store, &repo).unwrap();
    let started = store.latest_round_started(&first.run_id).unwrap().unwrap();
    // This historical loader case closes through the existing exhausted-before-work report
    // contract. No Worker or Provider runs, and no synthetic successful result is published.
    store
        .append(
            &first.run_id,
            &cas,
            NewEvent::new(
                EventType::RunReportV3,
                serde_json::to_value(review_core::RunReportPayloadV3 {
                    outcomes: ["reviewer", "gather", "ledger"]
                        .into_iter()
                        .map(|node| review_core::RunNodeReportV2 {
                            node: node.into(),
                            outcome: review_core::RunNodeOutcomeV2::Failed {
                                error: "resource limit reached before dispatch".into(),
                            },
                        })
                        .collect(),
                    blocked_gates: vec![],
                    verdict: review_core::RunVerdictV3::Fail {
                        reason: review_core::RunFailureReasonV3::Exhausted,
                    },
                    spent_tokens: Some(0),
                })
                .unwrap(),
            )
            .caused_by(&started.event_id),
        )
        .unwrap();
    let before = store.replay(&first.run_id).unwrap();
    std::fs::write(
        repo_path.join(".af/pipelines/review.toml"),
        "invalid live policy",
    )
    .unwrap();
    std::fs::write(repo_path.join("source.txt"), "uncommitted replacement").unwrap();
    let head = git(&repo_path, &home, &["rev-parse", "HEAD"]);
    let index = std::fs::read(repo_path.join(".git/index")).unwrap();
    let reader = EventStore::open_read_only(&path).unwrap();
    let recorded = prepare_recorded_round(&options, &cas, &reader, &started.event_id).unwrap();
    assert_eq!(
        recorded.authority.head_snapshot_id(),
        first.authority.head_snapshot_id()
    );
    assert_eq!(recorded.authority.round_event_id(), started.event_id);
    assert_eq!(reader.replay(&first.run_id).unwrap(), before);
    assert_eq!(git(&repo_path, &home, &["rev-parse", "HEAD"]), head);
    assert_eq!(std::fs::read(repo_path.join(".git/index")).unwrap(), index);
    assert_eq!(
        std::fs::read_to_string(repo_path.join("source.txt")).unwrap(),
        "uncommitted replacement"
    );
    let mut wrong = options;
    wrong.focus = Some("changed focus".into());
    assert!(prepare_recorded_round(&wrong, &cas, &reader, &started.event_id).is_err());
}
