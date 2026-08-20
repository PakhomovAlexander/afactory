//! One review, end to end, from a real git repository.
//!
//! Capture a tree, plan the pipeline, run it, and read the ledger — with real checks in real
//! sandboxes and real reviewer processes. Nothing here is a stub except the reviewers'
//! *judgement*, which is a `command` runner emitting fixed findings; that is the one thing a
//! test cannot supply honestly, and the one thing the kernel deliberately knows nothing about.

mod support;

use std::path::PathBuf;

use review_check::{Arg, CheckDefinition, Command};
use review_graph::{Node, NodeKind, NodeOutcome, Pipeline, Port, Scheduler};
use review_pipeline::Kernel;
use review_source_git::{Capture, Repo};
use review_store::{Cas, ConvergencePolicy, EventStore, Status, Verdict};

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

fn heavy_pipeline() -> Pipeline {
    let mut pipeline = Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(
            Node::new("gather", NodeKind::Gather)
                .accepting(&["architecture", "performance"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["reports"])
                .emitting(&["findings"]),
        );
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

    let kernel = support::whole_tree_kernel(&cas, &mut store, "run", snapshot.manifest.clone())
        .with_checks(vec![passing_check()])
        .with_reviewer(
            "architecture",
            reviewer("architecture", "Unbounded loop never yields", "major"),
        )
        .with_reviewer(
            "performance",
            reviewer("performance", "Unbounded loop never yields", "blocker"),
        );

    let plan = heavy_pipeline().plan().unwrap();
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

    let kernel = support::whole_tree_kernel(&cas, &mut store, "run", snapshot.manifest.clone())
        .with_checks(vec![failing_check()])
        .with_reviewer(
            "architecture",
            reviewer("architecture", "should never be reported", "major"),
        )
        .with_reviewer(
            "performance",
            reviewer("performance", "should never be reported", "major"),
        );

    let plan = heavy_pipeline().plan().unwrap();
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
        let kernel = support::whole_tree_kernel(&cas, &mut store, "run", snapshot.manifest.clone())
            .with_checks(vec![passing_check()])
            .with_reviewer("architecture", reviewer("architecture", "A", "major"))
            .with_reviewer("performance", reviewer("performance", "B", "minor"));
        let plan = heavy_pipeline().plan().unwrap();
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
    let authority = support::test_round_authority(&cas, &mut store, "run");
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

    let kernel = support::whole_tree_kernel(&cas, &mut store, "run", snapshot.manifest.clone())
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
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["reports"])
                .emitting(&["findings"]),
        )
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

    let kernel = support::whole_tree_kernel(&cas, &mut store, "run", snapshot.manifest.clone())
        .with_checks(vec![passing_check()])
        .with_reviewer("architecture", reviewer("architecture", "Wired", "major"))
        .with_reviewer("sidecar", reviewer("sidecar", "Unwired", "blocker"));

    // `sidecar` runs (it is a planned node) but nothing consumes its result port.
    let pipeline = Pipeline::default()
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
                .accepting(&["architecture"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["reports"])
                .emitting(&["findings"]),
        )
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
        );

    let plan = pipeline.plan().unwrap();
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

    let kernel = support::whole_tree_kernel(&cas, &mut store, "run", snapshot.manifest.clone())
        .with_checks(vec![passing_check()])
        .with_budgets(1000, 10000)
        .with_reviewer("architecture", reviewer("architecture", "A", "major"))
        .with_reviewer("performance", reviewer("performance", "B", "minor"));

    let plan = heavy_pipeline().plan().unwrap();
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
            "RunReport@2",
        ],
        "the log holds the whole run"
    );
    assert_eq!(types[0], "CampaignOpened@1");
    assert_eq!(types[1], "RoundStarted@1");
    assert_eq!(*types.last().unwrap(), "RunReport@2");
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
    let kernel = support::whole_tree_kernel(&cas, &mut store, "run", snapshot.manifest.clone())
        .with_checks(vec![passing_check()])
        .with_budgets(1000, 10000)
        .with_reviewer(
            "architecture",
            reviewer("architecture", "Found it", "major"),
        )
        .with_reviewer("performance", boom);

    let plan = heavy_pipeline().plan().unwrap();
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
    assert!(types.contains(&"RunReport@2".to_string()));
}
