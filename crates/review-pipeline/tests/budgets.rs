//! The owner's budget policy (2026-08-18), executed: reserve before dispatch, charge what ran
//! — fenced or not — and when the run exhausts, finish in-flight work, dispatch nothing new,
//! and report *incomplete*, which can never pass.
//!
//! Reviewers here are in-process stubs implementing [`ReviewerAdapter`] directly, because what
//! is under test is the kernel's accounting, not process supervision — that has its own tests
//! in `review-runner`.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use review_check::{Arg, CheckDefinition, Command};
use review_core::LegacyStageOutput;
use review_graph::{Node, NodeKind, NodeOutcome, Pipeline, Port, Scheduler};
use review_pipeline::{RunVerdict, run_verdict};
use review_runner::{ReviewerAdapter, ReviewerInputs, ReviewerReturn, RunnerError};
use review_source_git::{Capture, Repo};
use review_store::{Cas, ConvergencePolicy, EventStore};

const BUDGET_PIPELINE: &str = r#"
version = 2
[subject]
kind = "whole-tree"

[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]

[[nodes]]
id = "r-alpha"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }

[[nodes]]
id = "r-beta"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }

[[nodes]]
id = "r-gamma"
kind = "reviewer"
inputs = ["gate"]
outputs = ["result"]
gated_by = "gate"
runner = { program = "/bin/true" }

[[nodes]]
id = "gather"
kind = "gather"
inputs = ["r-alpha", "r-beta", "r-gamma"]
outputs = ["reports"]

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = ["findings"]

[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "r-alpha", port = "gate" }
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "r-beta", port = "gate" }
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "r-gamma", port = "gate" }
[[edges]]
from = { node = "r-alpha", port = "result" }
to = { node = "gather", port = "r-alpha" }
[[edges]]
from = { node = "r-beta", port = "result" }
to = { node = "gather", port = "r-beta" }
[[edges]]
from = { node = "r-gamma", port = "result" }
to = { node = "gather", port = "r-gamma" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

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
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}\n").unwrap();
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "e2e@example.invalid"]);
    git(&["config", "user.name", "E2E"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "initial"]);
    (dir, repo, home)
}

/// The gate fails closed on an empty check set, so every run carries one honest check.
fn passing_check() -> Vec<CheckDefinition> {
    vec![CheckDefinition::new(
        "noop",
        Command::new("/bin/sh", vec![Arg::literal("-c"), Arg::literal("true")]),
    )]
}

fn clean_output() -> LegacyStageOutput {
    serde_json::from_str(
        r#"{"verdict":"approve","summary":null,"findings":[],
            "benchmark_demands":[],"disputes":[]}"#,
    )
    .unwrap()
}

/// A reviewer that answers cleanly and reports what it spent.
struct Costed {
    cost: u64,
}

impl ReviewerAdapter for Costed {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        _inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        Ok(ReviewerReturn {
            output: clean_output(),
            cost_tokens: self.cost,
            raw_artifact: cas.put(b"stub answer").unwrap(),
        })
    }
}

/// A reviewer that hangs on its first attempt and answers on the second.
struct FlakyOnce {
    calls: AtomicU32,
    cost: u64,
}

impl ReviewerAdapter for FlakyOnce {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        _inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(RunnerError::TimedOut { after_ms: 200 });
        }
        Ok(ReviewerReturn {
            output: clean_output(),
            cost_tokens: self.cost,
            raw_artifact: cas.put(b"second answer").unwrap(),
        })
    }
}

/// gate → three reviewers → gather → ledger.
fn three_reviewer_pipeline() -> Pipeline {
    let mut pipeline = Pipeline::default()
        .node(Node::new("gate", NodeKind::Gate).emitting(&["decision"]))
        .node(
            Node::new("gather", NodeKind::Gather)
                .accepting(&["r-alpha", "r-beta", "r-gamma"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["reports"])
                .emitting(&["findings"]),
        );
    for reviewer in ["r-alpha", "r-beta", "r-gamma"] {
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

struct Run {
    _dir: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    cas: Cas,
    store: EventStore,
    snapshot: review_source_git::Manifest,
}

fn run_fixture() -> Run {
    let (dir, repo_path, home) = fixture();
    let workspace = tempfile::tempdir().unwrap();
    let cas = Cas::open(workspace.path().join("cas")).unwrap();
    let store = EventStore::open(workspace.path().join("events.sqlite")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas)
        .committed("HEAD")
        .unwrap()
        .manifest;
    Run {
        _dir: dir,
        _workspace: workspace,
        cas,
        store,
        snapshot,
    }
}

/// The run cap admits two 100k-reservation attempts, not three. The third reviewer never
/// dispatches, its refusal names the run scope, everything downstream of it is suppressed, and
/// the verdict is incomplete — even though every finding that *did* land converged clean.
#[test]
fn exhaustion_mid_run_finishes_what_ran_and_reports_incomplete() {
    let mut run = run_fixture();
    let kernel = support::whole_tree_kernel_for_pipeline(
        &run.cas,
        &mut run.store,
        "run",
        run.snapshot.clone(),
        None,
        BUDGET_PIPELINE,
    )
    .with_checks(passing_check())
    .with_budgets(100_000, 250_000)
    .with_adapter("r-alpha", Box::new(Costed { cost: 90_000 }))
    .with_adapter("r-beta", Box::new(Costed { cost: 90_000 }))
    .with_adapter("r-gamma", Box::new(Costed { cost: 90_000 }));

    let plan = three_reviewer_pipeline().plan().unwrap();
    // Sequential on purpose: these tests pin the budget ledger's per-attempt accounting,
    // which is only well-defined against a fixed dispatch order.
    let report = Scheduler::new(&plan).with_parallelism(1).run(&kernel);

    // Alpha and beta ran to completion — exhaustion stops the *next* dispatch, never the work
    // already in flight.
    for done in ["r-alpha", "r-beta"] {
        assert!(
            matches!(report.outcome(done), Some(NodeOutcome::Completed { .. })),
            "{done} should have completed: {:?}",
            report.outcome(done)
        );
    }

    // Gamma never ran, and its outcome says which scope refused.
    let Some(NodeOutcome::Failed { error }) = report.outcome("r-gamma") else {
        panic!(
            "r-gamma should have been refused: {:?}",
            report.outcome("r-gamma")
        );
    };
    assert!(error.contains("never dispatched"), "{error}");
    assert!(error.contains("run budget exhausted"), "{error}");

    // Downstream of the missing reviewer is suppressed, not quietly run on partial input.
    for downstream in ["gather", "ledger"] {
        assert!(
            matches!(
                report.outcome(downstream),
                Some(NodeOutcome::Suppressed { .. })
            ),
            "{downstream} must not run without gamma"
        );
    }

    // The verdict is incomplete and names every node that never contributed. It cannot pass —
    // the two clean reviews that did land do not speak for the one that never happened.
    let convergence = kernel.convergence(ConvergencePolicy::default());
    let verdict = run_verdict(&report, &convergence);
    assert_eq!(verdict, RunVerdict::Fail(review_store::Verdict::Exhausted));
    assert!(!verdict.passed());

    // And the books balance: two committed attempts, nothing phantom-reserved.
    assert_eq!(kernel.spent(), Some(180_000));
}

/// A timeout retries: the first attempt is fenced and charged its full reservation (its true
/// spend is unreportable), the retry answers, and both charges are on the books.
#[test]
fn a_timeout_is_fenced_charged_and_retried() {
    let mut run = run_fixture();
    let kernel = support::whole_tree_kernel_for_pipeline(
        &run.cas,
        &mut run.store,
        "run",
        run.snapshot.clone(),
        None,
        BUDGET_PIPELINE,
    )
    .with_checks(passing_check())
    .with_budgets(100_000, 2_000_000)
    .with_adapter(
        "r-alpha",
        Box::new(FlakyOnce {
            calls: AtomicU32::new(0),
            cost: 40_000,
        }),
    )
    .with_adapter("r-beta", Box::new(Costed { cost: 10_000 }))
    .with_adapter("r-gamma", Box::new(Costed { cost: 10_000 }));

    let plan = three_reviewer_pipeline().plan().unwrap();
    // Sequential on purpose: these tests pin the budget ledger's per-attempt accounting,
    // which is only well-defined against a fixed dispatch order.
    let report = Scheduler::new(&plan).with_parallelism(1).run(&kernel);
    assert!(
        report.complete(),
        "the retry should have completed the run: {report:?}"
    );

    // Full reservation for the fenced attempt + actual for the retry + the other two.
    assert_eq!(kernel.spent(), Some(100_000 + 40_000 + 10_000 + 10_000));

    let attempts = kernel.attempts();
    assert_eq!(
        attempts.quarantined().len(),
        0,
        "a fenced attempt that never delivered is not quarantined"
    );
    let alpha_attempts: Vec<_> = attempts
        .attempts()
        .into_iter()
        .filter(|a| a.node == "r-alpha")
        .collect();
    assert_eq!(alpha_attempts.len(), 2, "one fenced, one selected");

    let convergence = kernel.convergence(ConvergencePolicy::default());
    assert!(run_verdict(&report, &convergence).passed());
}

/// Exhaustion by repeated hangs: with the run cap equal to two reservations, a reviewer that
/// never answers consumes both and the run reports incomplete — a hang is not a free retry.
#[test]
fn repeated_timeouts_exhaust_rather_than_loop() {
    struct AlwaysHangs;
    impl ReviewerAdapter for AlwaysHangs {
        fn invoke(
            &self,
            _cas: &Cas,
            _root: &Path,
            _inputs: &ReviewerInputs,
        ) -> Result<ReviewerReturn, RunnerError> {
            Err(RunnerError::TimedOut { after_ms: 200 })
        }
    }

    let mut run = run_fixture();
    let kernel = support::whole_tree_kernel_for_pipeline(
        &run.cas,
        &mut run.store,
        "run",
        run.snapshot.clone(),
        None,
        BUDGET_PIPELINE,
    )
    .with_checks(passing_check())
    .with_budgets(100_000, 200_000)
    .with_adapter("r-alpha", Box::new(AlwaysHangs))
    .with_adapter("r-beta", Box::new(Costed { cost: 10_000 }))
    .with_adapter("r-gamma", Box::new(Costed { cost: 10_000 }));

    let plan = three_reviewer_pipeline().plan().unwrap();
    // Sequential on purpose: these tests pin the budget ledger's per-attempt accounting,
    // which is only well-defined against a fixed dispatch order.
    let report = Scheduler::new(&plan).with_parallelism(1).run(&kernel);

    let Some(NodeOutcome::Failed { error }) = report.outcome("r-alpha") else {
        panic!("alpha should have failed");
    };
    assert!(error.contains("timed out"), "{error}");
    // Both attempts charged in full; the cap is spent and beta/gamma are refused after it.
    assert_eq!(kernel.spent(), Some(200_000));

    let convergence = kernel.convergence(ConvergencePolicy::default());
    assert!(!run_verdict(&report, &convergence).passed());
}

/// An adapter that never started spends nothing: the reservation is released, and the node
/// after it still has the full cap available.
#[test]
fn an_unavailable_reviewer_releases_its_reservation() {
    struct Missing;
    impl ReviewerAdapter for Missing {
        fn invoke(
            &self,
            _cas: &Cas,
            _root: &Path,
            _inputs: &ReviewerInputs,
        ) -> Result<ReviewerReturn, RunnerError> {
            Err(RunnerError::Unavailable("no such provider".to_string()))
        }
    }

    // The cap arithmetic is the test: 170k of run budget fits beta's reservation (100k) plus
    // gamma's only if alpha's 100k was *released*. A leaked reservation would refuse beta
    // outright (100k held + 100k asked > 170k).
    let mut run = run_fixture();
    let kernel = support::whole_tree_kernel_for_pipeline(
        &run.cas,
        &mut run.store,
        "run",
        run.snapshot.clone(),
        None,
        BUDGET_PIPELINE,
    )
    .with_checks(passing_check())
    .with_budgets(100_000, 170_000)
    .with_adapter("r-alpha", Box::new(Missing))
    .with_adapter("r-beta", Box::new(Costed { cost: 60_000 }))
    .with_adapter("r-gamma", Box::new(Costed { cost: 10_000 }));

    let plan = three_reviewer_pipeline().plan().unwrap();
    // Sequential on purpose: these tests pin the budget ledger's per-attempt accounting,
    // which is only well-defined against a fixed dispatch order.
    let report = Scheduler::new(&plan).with_parallelism(1).run(&kernel);

    assert!(matches!(
        report.outcome("r-alpha"),
        Some(NodeOutcome::Failed { .. })
    ));
    // Beta and gamma could both still reserve: alpha's failure held nothing back.
    for done in ["r-beta", "r-gamma"] {
        assert!(
            matches!(report.outcome(done), Some(NodeOutcome::Completed { .. })),
            "{done}: {:?}",
            report.outcome(done)
        );
    }
    assert_eq!(kernel.spent(), Some(60_000 + 10_000));
}
