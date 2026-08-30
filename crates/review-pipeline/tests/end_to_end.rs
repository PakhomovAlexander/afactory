//! One review, end to end, from a real git repository.
//!
//! Capture a tree, plan the pipeline, run it, and read the ledger — with real checks in real
//! sandboxes and real reviewer processes. Nothing here is a stub except the reviewers'
//! *judgement*, which is a `command` runner emitting fixed findings; that is the one thing a
//! test cannot supply honestly, and the one thing the kernel deliberately knows nothing about.

mod support;

use std::path::PathBuf;

use review_check::{Arg, CheckDefinition, Command};
use review_graph::{Node, NodeKind, NodeOutcome, Pipeline, Port, PortContract, Scheduler};
use review_pipeline::Kernel;
use review_source_git::{Capture, Repo};
use review_store::{Cas, ConvergencePolicy, EventStore, NewEvent, Status, Verdict};

const HEAVY_AUTHORITY: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "architecture"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "performance"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = ["architecture", "performance"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["set"]
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "architecture", port = "gate" }
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "performance", port = "gate" }
[[edges]]
from = { node = "architecture", port = "result" }
to = { node = "gather", port = "architecture" }
[[edges]]
from = { node = "performance", port = "result" }
to = { node = "gather", port = "performance" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

const AWKWARD_AUTHORITY: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "gather"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "collect"
kind = "gather"
inputs = ["reports"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "gather", port = "gate" }
[[edges]]
from = { node = "gather", port = "result" }
to = { node = "collect", port = "reports" }
[[edges]]
from = { node = "collect", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

const UNWIRED_AUTHORITY: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "architecture"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "sidecar"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "gather"
kind = "gather"
inputs = [
  { name = "architecture", type = "review.kernel/Opaque@1", cardinality = "one", optional = false, snapshot_affinity = "any" },
  { name = "extra", type = "review.kernel/Opaque@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "architecture", port = "gate" }
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "sidecar", port = "gate" }
[[edges]]
from = { node = "architecture", port = "result" }
to = { node = "gather", port = "architecture" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

const NON_REVIEWER_GATHER_AUTHORITY: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "reviewer"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }
[[nodes]]
id = "evidence-gather"
kind = "gather"
inputs = ["decision"]
outputs = ["reports"]
[[nodes]]
id = "reviewer-gather"
kind = "gather"
inputs = ["reviewer"]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["set"]
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "reviewer", port = "gate" }
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "evidence-gather", port = "decision" }
[[edges]]
from = { node = "reviewer", port = "result" }
to = { node = "reviewer-gather", port = "reviewer" }
[[edges]]
from = { node = "reviewer-gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

fn canonical_authority(authority: &str) -> String {
    for finding_port in ["set", "findings"] {
        let original =
            format!("kind = \"ledger\"\ninputs = [\"reports\"]\noutputs = [\"{finding_port}\"]");
        if authority.contains(&original) {
            return authority.replacen(
                &original,
                &format!(
                    "kind = \"ledger\"\ninputs = [\"reports\"]\noutputs = [\n  \
                     {{ name = \"{finding_port}\", type = \"review.kernel/FindingSet@1\", \
                     cardinality = \"one\", optional = false, snapshot_affinity = \
                     \"same_subject\" }},\n  {{ name = \"demands\", type = \
                     \"review.kernel/DemandSet@1\", cardinality = \"one\", optional = false, \
                     snapshot_affinity = \"same_subject\" }},\n]"
                ),
                1,
            );
        }
    }
    panic!("test authority has no recognized Ledger finding output")
}

/// A repository with a defect to find.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let home = dir.path().join("home");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    std::fs::write(repo.join("src/main.rs"), b"fn main() { loop {} }\n").unwrap();
    std::fs::write(repo.join("build.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "e2e@example.invalid"]);
    git(&["config", "user.name", "E2E"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "initial"]);

    (dir, repo, home)
}

fn reviewer(node: &str, title: &str, severity: &str) -> Command {
    let json = format!(
        r#"{{"verdict":"request-changes","summary":null,"findings":[
             {{"severity":"{severity}","file":"src/main.rs","line":1,"title":"{title}",
               "body":"found by {node}","fix":"bound the loop","confidence":0.9}}
           ],"benchmark_demands":[],"disputes":[]}}"#
    );
    Command::new(
        "/bin/sh",
        vec![
            Arg::literal("-c"),
            // The reviewer reads the tree it was given, proving the sandbox is real, then
            // answers. A reviewer that never opened the code would still pass this test — which
            // is exactly why the kernel does not try to judge reviewer quality.
            Arg::literal(format!(
                "cat src/main.rs > /dev/null; cat <<'EOF'\n{json}\nEOF"
            )),
        ],
    )
}

fn clean_reviewer() -> Command {
    Command::new(
        "/bin/sh",
        vec![
            Arg::literal("-c"),
            Arg::literal(
                "cat src/main.rs > /dev/null; printf '%s\\n' '{\"verdict\":\"approve\",\"summary\":null,\"findings\":[],\"benchmark_demands\":[],\"disputes\":[]}'",
            ),
        ],
    )
}

fn gate_isolated_reviewer() -> Command {
    Command::new(
        "/bin/sh",
        vec![
            Arg::literal("-c"),
            Arg::literal(
                "test ! -e gate-output && cat src/main.rs > /dev/null && printf '%s\\n' '{\"verdict\":\"approve\",\"summary\":null,\"findings\":[],\"benchmark_demands\":[],\"disputes\":[]}'",
            ),
        ],
    )
}

fn v3_gate_authority(provider: &str, required_isolation: &str) -> String {
    let image = (provider == "container")
        .then(|| format!("image = \"{}\"\n", review_sandbox::container::DEFAULT_IMAGE));
    HEAVY_AUTHORITY.replacen(
        "version = 2\n",
        &format!(
            "version = 3\n[gate]\nprovider = \"{provider}\"\nrequired_isolation = \"{required_isolation}\"\nmode = \"ephemeral-write\"\n{}",
            image.unwrap_or_default()
        ),
        1,
    )
}

fn ledger_node(finding_port: &str, canonical: bool) -> Node {
    let node = Node::new("ledger", NodeKind::Ledger).accepting(&["reports"]);
    if canonical {
        node.emitting_contracts(vec![
            PortContract::new(finding_port, review_core::contract::FINDING_SET_V1),
            PortContract::new("demands", review_core::contract::DEMAND_SET_V1),
        ])
    } else {
        node.emitting(&[finding_port])
    }
}

fn heavy_pipeline(canonical: bool) -> Pipeline {
    let mut pipeline = Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(
            Node::new("gather", NodeKind::Gather)
                .accepting(&["architecture", "performance"])
                .emitting(&["reports"]),
        )
        .node(ledger_node("set", canonical));
    for reviewer in ["architecture", "performance"] {
        pipeline = pipeline
            .node(
                Node::new(reviewer, NodeKind::Reviewer)
                    .accepting(&["gate"])
                    .emitting(&["result"])
                    .gated_by("gate"),
            )
            .edge(Port::new("gate", "decision"), Port::new(reviewer, "gate"))
            .edge(Port::new(reviewer, "result"), Port::new("gather", reviewer));
    }
    pipeline.edge(
        Port::new("gather", "reports"),
        Port::new("ledger", "reports"),
    )
}

fn unwired_pipeline(canonical: bool) -> Pipeline {
    Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(
            Node::new("architecture", NodeKind::Reviewer)
                .accepting(&["gate"])
                .emitting(&["result"])
                .gated_by("gate"),
        )
        .node(
            Node::new("sidecar", NodeKind::Reviewer)
                .accepting(&["gate"])
                .emitting(&["result"])
                .gated_by("gate"),
        )
        .node(
            Node::new("gather", NodeKind::Gather)
                .accepting_contracts(vec![
                    PortContract::opaque("architecture"),
                    PortContract::opaque("extra").optional(),
                ])
                .emitting(&["reports"]),
        )
        .node(ledger_node("findings", canonical))
        .edge(
            Port::new("gate", "decision"),
            Port::new("architecture", "gate"),
        )
        .edge(Port::new("gate", "decision"), Port::new("sidecar", "gate"))
        .edge(
            Port::new("architecture", "result"),
            Port::new("gather", "architecture"),
        )
        .edge(
            Port::new("gather", "reports"),
            Port::new("ledger", "reports"),
        )
}

fn non_reviewer_gather_pipeline(canonical: bool) -> Pipeline {
    Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(
            Node::new("reviewer", NodeKind::Reviewer)
                .accepting(&["gate"])
                .emitting(&["result"])
                .gated_by("gate"),
        )
        .node(
            Node::new("evidence-gather", NodeKind::Gather)
                .accepting(&["decision"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("reviewer-gather", NodeKind::Gather)
                .accepting(&["reviewer"])
                .emitting(&["reports"]),
        )
        .node(ledger_node("set", canonical))
        .edge(Port::new("gate", "decision"), Port::new("reviewer", "gate"))
        .edge(
            Port::new("gate", "decision"),
            Port::new("evidence-gather", "decision"),
        )
        .edge(
            Port::new("reviewer", "result"),
            Port::new("reviewer-gather", "reviewer"),
        )
        .edge(
            Port::new("reviewer-gather", "reports"),
            Port::new("ledger", "reports"),
        )
}

fn passing_check() -> CheckDefinition {
    CheckDefinition::new(
        "build",
        Command::new("/bin/sh", vec![Arg::literal("./build.sh")]),
    )
}

fn failing_check() -> CheckDefinition {
    CheckDefinition::new(
        "build",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("echo does not build >&2; exit 1"),
            ],
        ),
    )
}

#[test]
fn a_full_review_runs_and_lands_in_the_ledger() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();

    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let before_state = review_source_git::worktree_state(&repo).unwrap();

    let kernel = support::whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        None,
        HEAVY_AUTHORITY,
    )
    .with_checks(vec![passing_check()])
    .with_reviewer(
        "architecture",
        reviewer("architecture", "Unbounded loop never yields", "major"),
    )
    .with_reviewer(
        "performance",
        reviewer("performance", "Unbounded loop never yields", "blocker"),
    );

    let plan = heavy_pipeline(false).plan().unwrap();
    let report = Scheduler::new(&plan).run(&kernel);

    assert!(report.complete(), "{:?}", report.outcomes);
    assert!(kernel.gate_decision("gate").unwrap().passed());

    // Both reviewers found the same defect, so the ledger holds one finding with both reports —
    // and the blocker severity wins, because severity is monotone under re-report.
    let ledger = kernel.ledger();
    assert_eq!(ledger.len(), 1);
    let finding = ledger.findings()[0];
    assert_eq!(finding.title, "Unbounded loop never yields");
    assert_eq!(finding.severity, review_core::Severity::Blocker);
    assert_eq!(finding.status, Status::Open);
    assert_eq!(finding.reports.len(), 2, "both reports stay attached");
    assert_eq!(
        finding.corroborating_sources(),
        vec!["architecture", "performance"],
        "canonical order, not completion order"
    );

    // An open blocker cannot converge.
    let convergence = kernel.convergence(ConvergencePolicy::default());
    assert_eq!(convergence.verdict, Verdict::NotConverged);
    assert_eq!(convergence.open_blocking, 1);

    // The checkout is untouched: the whole review ran against copies.
    assert_eq!(
        before_state,
        review_source_git::worktree_state(&repo).unwrap(),
        "the review modified the checkout it was reviewing"
    );
    assert_eq!(
        Capture::new(&repo, &cas)
            .committed("HEAD")
            .unwrap()
            .content_digest,
        snapshot.content_digest
    );
}

#[test]
fn canonical_barrier_keeps_same_presentation_claims_distinct_and_emits_the_exact_set_id() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority_definition = canonical_authority(HEAVY_AUTHORITY);
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        &authority_definition,
    )
    .with_checks(vec![passing_check()])
    .with_reviewer(
        "architecture",
        reviewer("architecture", "Same presentation", "major"),
    )
    .with_reviewer(
        "performance",
        reviewer("performance", "Same presentation", "major"),
    );

    let report = Scheduler::new(&heavy_pipeline(true).plan().unwrap()).run(&kernel);
    assert!(report.complete(), "{:?}", report.outcomes);
    assert_eq!(kernel.ledger().len(), 2, "path and title are not identity");
    let NodeOutcome::Completed { outputs } = report.outcome("ledger").unwrap() else {
        panic!("canonical ledger did not complete")
    };
    let set_id = outputs["set"][0].clone();
    let envelope: review_core::ArtifactEnvelope =
        serde_json::from_value(cas.get_json(&set_id).unwrap()).unwrap();
    assert_eq!(set_id, envelope.artifact_id, "the edge carries the Set ID");
    review_store::validate_envelope(&envelope).unwrap();
    assert_eq!(
        envelope.artifact_type,
        review_core::contract::FINDING_SET_V1
    );
    assert!(envelope.subject_snapshot_id.is_some());
    let set: review_core::FindingSetV1 = serde_json::from_value(envelope.payload).unwrap();
    set.validate().unwrap();
    assert_eq!(set.selected_report_ids.len(), 2);
    assert_eq!(set.findings.len(), 2);
    assert!(
        envelope.producer.is_deterministic(),
        "a ledger barrier is a kernel operation"
    );
    assert!(matches!(
        kernel
            .publish_report(&report, ConvergencePolicy::default())
            .unwrap(),
        review_pipeline::RunVerdict::Fail(Verdict::NotConverged)
    ));

    drop(kernel);
    let opened_event = store.campaign_opened("run").unwrap().unwrap();
    let opened_event_id = opened_event.event_id.clone();
    let opened: review_core::CampaignOpenedPayloadV1 =
        serde_json::from_value(opened_event.payload).unwrap();
    let campaign: review_core::CampaignManifestV1 =
        serde_json::from_value(cas.get_json(&opened.campaign_manifest_id).unwrap()).unwrap();
    assert_eq!(
        set.prior_finding_set_id, campaign.finding_genesis_id,
        "round 1 reduces from the canonical genesis, not the flat prompt projection"
    );
    let round = store.latest_round_started("run").unwrap().unwrap();
    let authority =
        review_pipeline::RoundAuthority::load(&store, &cas, "run", &round.event_id).unwrap();
    let loaded = review_config::Definition::from_toml(&authority_definition)
        .unwrap()
        .load()
        .unwrap();
    let replay = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        &loaded,
        authority,
    )
    .unwrap();
    let replayed = Scheduler::new(&heavy_pipeline(true).plan().unwrap()).run(&replay);
    let NodeOutcome::Completed { outputs } = replayed.outcome("ledger").unwrap() else {
        panic!("canonical ledger replay did not complete")
    };
    assert_eq!(outputs["set"], [set_id.clone()]);
    drop(replay);

    let prior_prompt = cas
        .put_json(&serde_json::json!({
            "subject_id": round.payload["subject_id"],
            "round": 2,
            "prior_findings": [],
        }))
        .unwrap();
    let prior_demands = cas
        .put_json(&serde_json::json!({
            "subject_id": round.payload["subject_id"],
            "round": 2,
            "demands": [],
        }))
        .unwrap();
    let round_one: review_core::RoundStartedPayloadV1 =
        serde_json::from_value(round.payload).unwrap();
    let subject: review_core::SubjectV1 =
        serde_json::from_value(cas.get_json(&round_one.subject_id).unwrap()).unwrap();
    let round_two = review_core::RoundStartedPayloadV1 {
        round: 2,
        epoch: 1,
        campaign_manifest_id: opened.campaign_manifest_id.clone(),
        subject_id: round_one.subject_id.clone(),
        prior_finding_set_id: prior_prompt.clone(),
        prior_demand_set_id: prior_demands.clone(),
    };
    let round_two_event = store
        .append(
            "run",
            &cas,
            NewEvent::new(
                review_core::EventType::RoundStartedV1,
                serde_json::to_value(&round_two).unwrap(),
            )
            .caused_by(opened_event_id)
            .correlating(round_two.subject_id.clone())
            .referencing(vec![
                campaign.authority_snapshot_id,
                opened.campaign_manifest_id,
                subject.head_snapshot_id,
                round_two.subject_id,
                prior_prompt,
                prior_demands,
            ]),
        )
        .unwrap();
    store
        .append(
            "run",
            &cas,
            NewEvent::new(
                review_core::EventType::GenerationAdvancedV1,
                serde_json::json!({ "round": 2 }),
            )
            .caused_by(round_two_event.event_id.clone()),
        )
        .unwrap();
    let authority =
        review_pipeline::RoundAuthority::load(&store, &cas, "run", &round_two_event.event_id)
            .unwrap();
    let round_two_kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        snapshot.manifest,
        &loaded,
        authority,
    )
    .unwrap()
    .with_checks(vec![passing_check()])
    .with_reviewer(
        "architecture",
        reviewer("architecture", "Round two architecture", "major"),
    )
    .with_reviewer(
        "performance",
        reviewer("performance", "Round two performance", "major"),
    );
    let round_two_report =
        Scheduler::new(&heavy_pipeline(true).plan().unwrap()).run(&round_two_kernel);
    let NodeOutcome::Completed { outputs } = round_two_report.outcome("ledger").unwrap() else {
        panic!("round two canonical ledger did not complete")
    };
    let round_two_envelope: review_core::ArtifactEnvelope =
        serde_json::from_value(cas.get_json(&outputs["set"][0]).unwrap()).unwrap();
    let round_two_set: review_core::FindingSetV1 =
        serde_json::from_value(round_two_envelope.payload).unwrap();
    assert_eq!(round_two_set.round, 2);
    assert_eq!(
        round_two_set.prior_finding_set_id, set_id,
        "round 2 reduces from the exact round 1 FindingSet output"
    );
}

#[test]
fn canonical_barrier_assigns_identical_clean_results_to_distinct_attempts() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority = canonical_authority(HEAVY_AUTHORITY);
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest,
        &authority,
    )
    .with_checks(vec![passing_check()])
    .with_reviewer("architecture", clean_reviewer())
    .with_reviewer("performance", clean_reviewer());

    let report = Scheduler::new(&heavy_pipeline(true).plan().unwrap()).run(&kernel);

    assert!(report.complete(), "{:?}", report.outcomes);
    assert!(kernel.ledger().is_empty());
    assert!(matches!(
        report.outcome("ledger"),
        Some(NodeOutcome::Completed { .. })
    ));
}

#[test]
fn a_pinned_pre_m4_canonical_campaign_resumes_without_selected_demands() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    // This helper writes the same immutable authority events an older release persisted. New
    // Campaign creation rejects this pipeline, but an existing Campaign must remain resumable.
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest,
        HEAVY_AUTHORITY,
    )
    .with_checks(vec![passing_check()])
    .with_reviewer("architecture", clean_reviewer())
    .with_reviewer("performance", clean_reviewer());

    let report = Scheduler::new(&heavy_pipeline(false).plan().unwrap()).run(&kernel);
    assert!(report.complete(), "{:?}", report.outcomes);
    let NodeOutcome::Completed { outputs } = report.outcome("ledger").unwrap() else {
        panic!("pre-M4 Ledger did not complete")
    };
    assert!(outputs.contains_key("set"));
    assert!(!outputs.contains_key("demands"));
}

#[test]
fn an_unwired_identical_result_does_not_confuse_canonical_provenance() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority = canonical_authority(UNWIRED_AUTHORITY);
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest,
        &authority,
    )
    .with_checks(vec![passing_check()])
    .with_reviewer("architecture", clean_reviewer())
    .with_reviewer("sidecar", clean_reviewer());

    let report = Scheduler::new(&unwired_pipeline(true).plan().unwrap()).run(&kernel);

    assert!(report.complete(), "{:?}", report.outcomes);
    assert!(kernel.ledger().is_empty());
}

#[test]
fn canonical_gather_binds_non_reviewer_inputs_to_their_pinned_node_output() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority = canonical_authority(NON_REVIEWER_GATHER_AUTHORITY);
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest,
        &authority,
    )
    .with_checks(vec![passing_check()])
    .with_reviewer("reviewer", clean_reviewer());

    let report = Scheduler::new(&non_reviewer_gather_pipeline(true).plan().unwrap()).run(&kernel);

    assert!(report.complete(), "{:?}", report.outcomes);
    let NodeOutcome::Completed {
        outputs: gate_outputs,
    } = report.outcome("gate").unwrap()
    else {
        panic!("gate did not complete")
    };
    let NodeOutcome::Completed {
        outputs: gather_outputs,
    } = report.outcome("evidence-gather").unwrap()
    else {
        panic!("gather did not complete")
    };
    assert_eq!(
        cas.get_json(&gather_outputs["reports"][0]).unwrap(),
        serde_json::json!({"gate": gate_outputs["decision"]}),
        "the manifest labels a deterministic artifact with its pinned upstream node"
    );
}

/// The property the gate exists for, end to end: a change that does not build produces **no
/// reviewer artifacts at all** — not reviewer artifacts nobody reads.
#[test]
fn a_failing_gate_means_no_reviewer_ever_runs() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();

    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    let kernel = support::whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        None,
        HEAVY_AUTHORITY,
    )
    .with_checks(vec![failing_check()])
    .with_reviewer(
        "architecture",
        reviewer("architecture", "should never be reported", "major"),
    )
    .with_reviewer(
        "performance",
        reviewer("performance", "should never be reported", "major"),
    );

    let plan = heavy_pipeline(false).plan().unwrap();
    let report = Scheduler::new(&plan).run(&kernel);

    assert!(!report.complete());
    assert!(!kernel.gate_decision("gate").unwrap().passed());
    assert_eq!(
        report.dispatched(),
        vec!["gate"],
        "only the gate may have run"
    );
    assert_eq!(
        report.suppressed(),
        vec!["architecture", "performance", "gather", "ledger"]
    );
    assert!(matches!(
        report.outcome("architecture"),
        Some(NodeOutcome::Suppressed { .. })
    ));

    // Nothing reached the ledger, and the failing check's evidence did.
    assert!(kernel.ledger().is_empty(), "no findings from a blocked run");
    let events = store.replay("run").unwrap();
    assert_eq!(
        events.len(),
        6,
        "campaign and Round authority plus one invocation, check, decision, and receipt"
    );
    assert_eq!(events[0].event_type, "CampaignOpened@1");
    assert_eq!(events[1].event_type, "RoundStarted@1");
    assert_eq!(events[2].event_type, "NodeInvocation@1");
    assert_eq!(events[2].payload["node"], "gate");
    assert_eq!(events[3].event_type, "CheckCompleted@1");
    assert_eq!(events[3].payload["status"], "failed");
    assert_eq!(events[4].event_type, "GateDecision@1");
    assert_eq!(events[4].payload["outcome"], "Blocked");
    assert_eq!(events[5].event_type, "NodeOutputReceipt@1");
}

/// Same inputs, same pipeline, twice — the ledger and the verdict must not move.
#[test]
fn two_runs_of_the_same_review_agree() {
    let (_dir, repo_path, home) = fixture();
    let repo = Repo::open(&repo_path, &home);

    let fingerprint = || {
        let workspace = tempfile::tempdir().unwrap();
        let cas = Cas::open(workspace.path().join("cas")).unwrap();
        let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
        let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
        let kernel = support::whole_tree_kernel_for_pipeline(
            &cas,
            &mut store,
            "run",
            snapshot.manifest.clone(),
            None,
            HEAVY_AUTHORITY,
        )
        .with_checks(vec![passing_check()])
        .with_reviewer("architecture", reviewer("architecture", "A", "major"))
        .with_reviewer("performance", reviewer("performance", "B", "minor"));
        let plan = heavy_pipeline(false).plan().unwrap();
        Scheduler::new(&plan).run(&kernel);

        let rows: Vec<String> = kernel
            .ledger()
            .findings()
            .into_iter()
            .map(|f| format!("{} {} {:?} {}", f.key, f.title, f.severity, f.source))
            .collect();
        // The whole log, ids included: reviewers run on concurrent threads, so this is the
        // property the buffered canonical-order flush exists to keep — two identical runs
        // produce byte-identical logs, not just identical ledgers.
        let log: Vec<(u64, String, String)> = store
            .replay("run")
            .unwrap()
            .into_iter()
            .map(|e| (e.sequence, e.event_type.to_string(), e.event_id))
            .collect();
        (snapshot.content_digest, rows, log)
    };

    let first = fingerprint();
    let second = fingerprint();
    assert_eq!(
        first, second,
        "two identical runs must agree, event ids included"
    );
    assert_eq!(first.1.len(), 2);
    assert!(
        first.2.iter().any(|(_, t, _)| t == "NodeOutputReceipt@1"),
        "the reviewer events are in the compared log"
    );
}

/// The payoff of the definition format: a review described in a file, run end to end, with no
/// pipeline construction in code at all.
#[test]
fn a_review_runs_from_a_definition_file() {
    use review_config::Definition;

    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();

    let definition = r#"
version = 1

[[checks]]
name = "build"
program = "/bin/sh"
args = [{ value = "./build.sh" }]

[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]

[[nodes]]
id = "architecture"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/sh", args = [
  { value = "-c" },
  { value = "cat <<'EOF'\n{\"verdict\":\"request-changes\",\"summary\":null,\"findings\":[{\"severity\":\"major\",\"file\":\"src/main.rs\",\"line\":1,\"title\":\"Unbounded loop never yields\",\"body\":\"b\",\"fix\":\"f\",\"confidence\":0.9}],\"benchmark_demands\":[],\"disputes\":[]}\nEOF" },
] }

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]

[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "architecture", port = "gate" }

[[edges]]
from = { node = "architecture", port = "result" }
to = { node = "ledger", port = "reports" }

[convergence]
clean_rounds = 1
max_rounds = 3
gate = "major"
"#;

    let loaded = Definition::from_toml(definition).unwrap().load().unwrap();

    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &snapshot.manifest,
        definition,
    );
    let mut kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        &loaded,
        authority,
    )
    .unwrap()
    .with_checks(loaded.checks().to_vec());
    for (node, command) in loaded.reviewers() {
        kernel = kernel.with_reviewer(node.clone(), command.clone());
    }

    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);

    let ledger = kernel.ledger();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger.findings()[0].title, "Unbounded loop never yields");

    // And the convergence policy came from the file too.
    let convergence = kernel.convergence(*loaded.convergence());
    assert_eq!(convergence.verdict, Verdict::NotConverged);
    assert_eq!(convergence.open_blocking, 1);
}

#[test]
fn a_v3_gate_binding_allows_disposable_writes_without_tainting_reviewer_sandboxes() {
    use review_config::Definition;

    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let definition = v3_gate_authority("trusted_local", "none");
    let loaded = Definition::from_toml(&definition).unwrap().load().unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &snapshot.manifest,
        &definition,
    );
    let manifest = snapshot.manifest;
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        manifest.clone(),
        &loaded,
        authority.clone(),
    )
    .unwrap()
    .with_checks(vec![CheckDefinition::new(
        "write-scaffold",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("printf gate > gate-output"),
            ],
        ),
    )])
    .with_reviewer("architecture", gate_isolated_reviewer())
    .with_reviewer("performance", gate_isolated_reviewer());

    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    assert!(kernel.gate_decision("gate").unwrap().passed());
    kernel
        .publish_report(&report, *loaded.convergence())
        .unwrap();
    assert!(!repo_path.join("gate-output").exists());
    drop(kernel);

    let report = store
        .replay("run")
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == review_core::EventType::RunReportV4)
        .expect("RunReport@4");
    let payload: review_core::RunReportPayloadV4 = serde_json::from_value(report.payload).unwrap();
    assert_eq!(payload.execution_bindings.len(), 1);
    let binding = &payload.execution_bindings[0];
    assert_eq!(binding.node, "gate");
    assert_eq!(
        binding.provider,
        review_core::RunExecutionProviderV4::TrustedLocal
    );
    assert_eq!(
        binding.required_isolation,
        review_core::RunIsolationV4::None
    );
    assert_eq!(
        binding.provided_isolation,
        review_core::RunIsolationV4::None
    );
    assert!(binding.admitted);
}

#[test]
fn a_resumed_v3_round_replays_its_durable_gate_binding() {
    use review_config::Definition;

    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let definition = v3_gate_authority("trusted_local", "none");
    let loaded = Definition::from_toml(&definition).unwrap().load().unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let manifest = snapshot.manifest;
    let authority =
        support::test_round_authority_for_pipeline(&cas, &mut store, "run", &manifest, &definition);
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        manifest.clone(),
        &loaded,
        authority.clone(),
    )
    .unwrap()
    .with_checks(vec![passing_check()])
    .with_reviewer("architecture", clean_reviewer())
    .with_reviewer("performance", clean_reviewer());

    let first = loaded.run(&kernel).unwrap();
    assert!(first.complete(), "{:?}", first.outcomes);
    drop(kernel); // crash window: every node receipt is durable, RunReport is not.

    let resumed = Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority)
        .unwrap()
        .with_checks(vec![passing_check()])
        .with_reviewer("architecture", clean_reviewer())
        .with_reviewer("performance", clean_reviewer());
    let replayed = loaded.run(&resumed).unwrap();
    assert!(replayed.complete(), "{:?}", replayed.outcomes);
    resumed
        .publish_report(&replayed, *loaded.convergence())
        .unwrap();
    drop(resumed);

    let events = store.replay("run").unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == review_core::EventType::GateExecutionBoundV1)
            .count(),
        1
    );
    let report = events
        .into_iter()
        .find(|event| event.event_type == review_core::EventType::RunReportV4)
        .expect("resumed RunReport@4");
    let payload: review_core::RunReportPayloadV4 = serde_json::from_value(report.payload).unwrap();
    assert_eq!(payload.execution_bindings.len(), 1);
    assert!(payload.execution_bindings[0].admitted);
}

#[test]
fn a_v3_unusable_container_is_not_admitted_or_executed() {
    use review_config::Definition;

    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let definition = v3_gate_authority("container", "none");
    let loaded = Definition::from_toml(&definition).unwrap().load().unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &snapshot.manifest,
        &definition,
    );
    let manifest = snapshot.manifest;
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        manifest.clone(),
        &loaded,
        authority.clone(),
    )
    .unwrap()
    .with_container_provider(review_sandbox::ContainerProvider::with_runtime(
        workspace.path().join("missing-container-runtime"),
    ))
    .with_checks(vec![passing_check()])
    .with_reviewer("architecture", clean_reviewer())
    .with_reviewer("performance", clean_reviewer());

    let report = loaded.run(&kernel).unwrap();
    assert!(matches!(
        report.outcome("gate"),
        Some(NodeOutcome::Failed { error, .. }) if error.contains("container provider unavailable")
    ));
    kernel
        .publish_report(&report, *loaded.convergence())
        .unwrap();
    drop(kernel);

    let report = store
        .replay("run")
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == review_core::EventType::RunReportV4)
        .expect("RunReport@4");
    let payload: review_core::RunReportPayloadV4 = serde_json::from_value(report.payload).unwrap();
    assert_eq!(payload.execution_bindings.len(), 1);
    let binding = &payload.execution_bindings[0];
    assert_eq!(
        binding.provider,
        review_core::RunExecutionProviderV4::Container
    );
    assert_eq!(
        binding.provided_isolation,
        review_core::RunIsolationV4::None
    );
    assert!(binding.image.is_some());
    assert!(!binding.admitted);
    assert!(
        store
            .replay("run")
            .unwrap()
            .iter()
            .all(|event| event.event_type != review_core::EventType::CheckCompletedV1)
    );

    // The same Round/epoch remains resumable when provider availability changes. `/bin/true`
    // is a deterministic provider stub: its `info` and `run` invocations both succeed. The
    // ignored live test below proves the real container boundary.
    let resumed = Kernel::from_loaded(&cas, &mut store, "run", manifest, &loaded, authority)
        .unwrap()
        .with_container_provider(review_sandbox::ContainerProvider::with_runtime(
            "/usr/bin/true",
        ))
        .with_checks(vec![passing_check()])
        .with_reviewer("architecture", clean_reviewer())
        .with_reviewer("performance", clean_reviewer());
    let report = loaded.run(&resumed).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    resumed
        .publish_report(&report, *loaded.convergence())
        .unwrap();
    drop(resumed);

    let events = store.replay("run").unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == review_core::EventType::GateExecutionBoundV1)
            .count(),
        2,
        "both failed and successful provider observations remain durable"
    );
    let final_report = events
        .iter()
        .rev()
        .find(|event| event.event_type == review_core::EventType::RunReportV4)
        .expect("final RunReport@4");
    let final_report: review_core::RunReportPayloadV4 =
        serde_json::from_value(final_report.payload.clone()).unwrap();
    assert!(final_report.execution_bindings[0].admitted);
    assert_eq!(
        final_report.execution_bindings[0].provided_isolation,
        review_core::RunIsolationV4::Container
    );
}

#[test]
#[ignore = "needs a live container runtime; run with the container probe gate"]
fn a_v3_container_gate_executes_through_the_pipeline() {
    use review_config::Definition;
    use review_sandbox::Availability;

    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let definition = v3_gate_authority("container", "container");
    let loaded = Definition::from_toml(&definition).unwrap().load().unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let authority = support::test_round_authority_for_pipeline(
        &cas,
        &mut store,
        "run",
        &snapshot.manifest,
        &definition,
    );
    let provider = review_sandbox::ContainerProvider::detect();
    assert!(
        matches!(provider.availability(), Availability::Usable { .. }),
        "{}",
        provider.availability().reason()
    );
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        snapshot.manifest,
        &loaded,
        authority,
    )
    .unwrap()
    .with_container_provider(provider)
    .with_checks(vec![CheckDefinition::new(
        "container-control",
        Command::new(
            "/bin/sh",
            vec![Arg::literal("-c"), Arg::literal("test -f src/main.rs")],
        ),
    )])
    .with_reviewer("architecture", clean_reviewer())
    .with_reviewer("performance", clean_reviewer());

    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    assert!(kernel.gate_decision("gate").unwrap().passed());
    kernel
        .publish_report(&report, *loaded.convergence())
        .unwrap();
    drop(kernel);

    let report = store
        .replay("run")
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == review_core::EventType::RunReportV4)
        .expect("RunReport@4");
    let payload: review_core::RunReportPayloadV4 = serde_json::from_value(report.payload).unwrap();
    let binding = &payload.execution_bindings[0];
    assert_eq!(
        binding.provided_isolation,
        review_core::RunIsolationV4::Container
    );
    assert_eq!(
        binding.image.as_deref(),
        Some(review_sandbox::container::DEFAULT_IMAGE)
    );
    assert!(binding.admitted);
}

/// A reviewer's id is a name, not its role. Dispatch that routed on the id string silently
/// skipped a reviewer named `gather` — never executed, yet reported `Completed`. Routing is on
/// `NodeKind` now, so the awkward name must not matter.
#[test]
fn a_reviewer_named_gather_still_runs() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();

    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    let authority = canonical_authority(AWKWARD_AUTHORITY);
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        &authority,
    )
    .with_checks(vec![passing_check()])
    .with_reviewer(
        "gather",
        reviewer("gather", "Found by the awkwardly named reviewer", "major"),
    );

    let pipeline = Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(
            Node::new("gather", NodeKind::Reviewer)
                .accepting(&["gate"])
                .emitting(&["result"])
                .gated_by("gate"),
        )
        .node(
            Node::new("collect", NodeKind::Gather)
                .accepting(&["reports"])
                .emitting(&["reports"]),
        )
        .node(ledger_node("findings", true))
        .edge(Port::new("gate", "decision"), Port::new("gather", "gate"))
        .edge(
            Port::new("gather", "result"),
            Port::new("collect", "reports"),
        )
        .edge(
            Port::new("collect", "reports"),
            Port::new("ledger", "reports"),
        );

    let plan = pipeline.plan().unwrap();
    let report = Scheduler::new(&plan).run(&kernel);

    assert!(report.complete(), "{:?}", report.outcomes);
    let ledger = kernel.ledger();
    assert_eq!(ledger.len(), 1, "the reviewer executed; its finding landed");
    assert_eq!(
        ledger.findings()[0].title,
        "Found by the awkwardly named reviewer"
    );
    assert_eq!(
        ledger.findings()[0].reports[0].source,
        "gather",
        "the gather input-port label must not replace reviewer provenance"
    );
}

/// The plan is the data flow: a reviewer whose result port feeds no edge contributes nothing
/// downstream. Before the ledger consumed its inputs, it reduced a global results map, and the
/// unwired reviewer's findings landed anyway.
#[test]
fn an_unwired_reviewer_result_never_reaches_the_ledger() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();

    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    let kernel = support::whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        None,
        UNWIRED_AUTHORITY,
    )
    .with_checks(vec![passing_check()])
    .with_reviewer("architecture", reviewer("architecture", "Wired", "major"))
    .with_reviewer("sidecar", reviewer("sidecar", "Unwired", "blocker"));

    // `sidecar` runs (it is a planned node) but nothing consumes its result port.
    let plan = unwired_pipeline(false).plan().unwrap();
    let report = Scheduler::new(&plan).run(&kernel);

    assert!(report.complete(), "{:?}", report.outcomes);
    let ledger = kernel.ledger();
    assert_eq!(ledger.len(), 1, "only the wired reviewer's finding lands");
    assert_eq!(ledger.findings()[0].title, "Wired");
}

/// The run's story is in its log: capture aside (the driver appends that), every kernel
/// decision — gate verdicts, attempt lifecycle, reviewer results, findings, the report —
/// replays from the event store alone.
#[test]
fn the_event_log_tells_the_whole_story() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();

    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    let kernel = support::whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        None,
        HEAVY_AUTHORITY,
    )
    .with_checks(vec![passing_check()])
    .with_budgets(1000, 10000)
    .with_reviewer("architecture", reviewer("architecture", "A", "major"))
    .with_reviewer("performance", reviewer("performance", "B", "minor"));

    let plan = heavy_pipeline(false).plan().unwrap();
    let report = Scheduler::new(&plan).run(&kernel);
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();
    assert!(
        kernel
            .publish_report(&report, ConvergencePolicy::default())
            .unwrap_err()
            .contains("already published")
    );

    let events = store.replay("run").unwrap();
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();

    // Concurrent reviewers interleave, so the log's global order is what happened, not a
    // fixed sequence. What IS fixed: the multiset of events, the endpoints, and each node's
    // own lifecycle order.
    let mut sorted = types.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec![
            "AttemptAdmitted@1",
            "AttemptAdmitted@1",
            "AttemptDispatched@1",
            "AttemptDispatched@1",
            "CampaignOpened@1",
            "CheckCompleted@1",
            "FindingReported@1",
            "FindingReported@1",
            "GateDecision@1",
            "NodeInvocation@1",
            "NodeInvocation@1",
            "NodeInvocation@1",
            "NodeInvocation@1",
            "NodeInvocation@1",
            "NodeOutputReceipt@1",
            "NodeOutputReceipt@1",
            "NodeOutputReceipt@1",
            "NodeOutputReceipt@1",
            "NodeOutputReceipt@1",
            "RoundStarted@1",
            "RunReport@3",
        ],
        "the log holds the whole run"
    );
    assert_eq!(types[0], "CampaignOpened@1");
    assert_eq!(types[1], "RoundStarted@1");
    assert_eq!(*types.last().unwrap(), "RunReport@3");
    for node in ["architecture", "performance"] {
        let lifecycle: Vec<&str> = events
            .iter()
            .filter(|e| e.node_id.as_deref() == Some(node))
            .map(|e| e.event_type.as_str())
            .collect();
        assert_eq!(
            lifecycle,
            vec![
                "NodeInvocation@1",
                "AttemptDispatched@1",
                "AttemptAdmitted@1",
                "NodeOutputReceipt@1"
            ],
            "{node}'s own lifecycle stays ordered"
        );
    }
    let run_report = events.last().unwrap();
    assert_eq!(run_report.payload["verdict"]["kind"], "fail");
    assert_eq!(run_report.payload["verdict"]["reason"], "not_converged");
    assert_eq!(run_report.payload["outcomes"].as_array().unwrap().len(), 5);
    let admitted: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e.event_type == "AttemptAdmitted@1")
        .map(|e| &e.payload)
        .collect();
    assert!(
        admitted.iter().all(|p| p["selection"] == "selected"),
        "no quarantines in a clean run"
    );
    for receipt in events
        .iter()
        .filter(|event| event.event_type == "NodeOutputReceipt@1")
    {
        let selected: Vec<&str> = receipt.payload["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|port| port["artifact_ids"].as_array().unwrap())
            .map(|id| id.as_str().unwrap())
            .collect();
        assert!(
            selected
                .iter()
                .all(|artifact| receipt.artifact_refs.iter().any(|id| id == artifact)),
            "receipt lost a selected output artifact"
        );
        assert!(
            receipt.artifact_refs.len() > selected.len(),
            "receipt is not bound to Round authority"
        );
        assert!(receipt.causation_id.is_some());
        assert!(receipt.correlation_id.is_some());
    }
}

/// A failed reviewer suppresses the gather it feeds — and the buffered attempt events, charges
/// included, must still reach the log. Before publish_report guaranteed the flush, a suppressed
/// gather erased the whole attempt record, including the surviving reviewer's.
#[test]
fn a_suppressed_gather_does_not_erase_the_attempt_log() {
    let (_dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let mut store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();

    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    // A reviewer that exits non-zero, and one that answers cleanly.
    let boom = Command::new(
        "/bin/sh",
        vec![Arg::literal("-c"), Arg::literal("echo boom >&2; exit 7")],
    );
    let kernel = support::whole_tree_kernel_for_pipeline(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        None,
        HEAVY_AUTHORITY,
    )
    .with_checks(vec![passing_check()])
    .with_budgets(1000, 10000)
    .with_reviewer(
        "architecture",
        reviewer("architecture", "Found it", "major"),
    )
    .with_reviewer("performance", boom);

    let plan = heavy_pipeline(false).plan().unwrap();
    let report = Scheduler::new(&plan).run(&kernel);
    // Gather (and ledger) are suppressed because `performance` failed.
    assert!(matches!(
        report.outcome("gather"),
        Some(NodeOutcome::Suppressed { .. })
    ));
    kernel
        .publish_report(&report, ConvergencePolicy::default())
        .unwrap();

    // The paid work is in the log: both reviewers dispatched, architecture produced a result,
    // performance failed — none of it lost to the suppressed gather.
    let types: Vec<String> = store
        .replay("run")
        .unwrap()
        .into_iter()
        .map(|e| e.event_type.to_string())
        .collect();
    let count = |t: &str| types.iter().filter(|x| x.as_str() == t).count();
    assert_eq!(
        count("AttemptDispatched@1"),
        2,
        "both reviewers dispatched: {types:?}"
    );
    let architecture_receipts = store
        .replay("run")
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.event_type == "NodeOutputReceipt@1"
                && event.node_id.as_deref() == Some("architecture")
        })
        .count();
    assert_eq!(architecture_receipts, 1, "architecture's result recorded");
    assert_eq!(
        count("AttemptFailed@1"),
        1,
        "performance's failure recorded"
    );
    assert!(types.contains(&"RunReport@3".to_string()));
}
