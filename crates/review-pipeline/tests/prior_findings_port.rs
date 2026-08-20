//! Prior findings reach a reviewer through a wired input port, not ambient kernel state.
//!
//! A generation node emits the campaign's prior findings; a reviewer that declares a
//! `prior_findings` input, wired from it, receives them — and a reviewer that declares no such
//! input receives nothing, whatever the kernel holds. The plan is the delivery.

use std::path::Path;
use std::sync::{Arc, Mutex};

use review_core::LegacyStageOutput;
use review_graph::{Node, NodeKind, Pipeline, Port, Scheduler};
use review_pipeline::Kernel;
use review_runner::{ReviewerAdapter, ReviewerInputs, ReviewerReturn, RunnerError};
use review_source_git::{Capture, Repo};
use review_store::{Cas, EventStore};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let home = dir.path().join("home");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let git = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repo)
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    };
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}\n").unwrap();
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "e2e@example.invalid"]);
    git(&["config", "user.name", "E2E"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "initial"]);
    (dir, repo, home)
}

fn clean_output() -> LegacyStageOutput {
    serde_json::from_str(
        r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}"#,
    )
    .unwrap()
}

/// Records the `prior_findings` it was handed, so a test can assert what the port delivered.
struct Recorder {
    seen: Arc<Mutex<Option<Option<serde_json::Value>>>>,
}

impl ReviewerAdapter for Recorder {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        *self.seen.lock().unwrap() = Some(inputs.prior_findings.clone());
        Ok(ReviewerReturn {
            output: clean_output(),
            cost_tokens: 1,
            raw_artifact: cas.put(b"stub").unwrap(),
        })
    }
}

/// generation → reviewer(prior_findings) → gather → ledger.
fn pipeline() -> Pipeline {
    Pipeline::default()
        .node(Node::new("generation", NodeKind::Generation).emitting(&["findings"]))
        .node(
            Node::new("reviewer", NodeKind::Reviewer)
                .accepting(&["prior_findings"])
                .emitting(&["result"]),
        )
        .node(
            Node::new("gather", NodeKind::Gather)
                .accepting(&["reviewer"])
                .emitting(&["reports"]),
        )
        .node(
            Node::new("ledger", NodeKind::Ledger)
                .accepting(&["reports"])
                .emitting(&["findings"]),
        )
        .edge(
            Port::new("generation", "findings"),
            Port::new("reviewer", "prior_findings"),
        )
        .edge(
            Port::new("reviewer", "result"),
            Port::new("gather", "reviewer"),
        )
        .edge(
            Port::new("gather", "reports"),
            Port::new("ledger", "reports"),
        )
}

fn run(prior: Option<&str>) -> Option<Option<serde_json::Value>> {
    let (_dir, repo_path, home) = fixture();
    let ws = tempfile::tempdir().unwrap();
    let cas = Cas::open(ws.path().join("cas")).unwrap();
    let mut store = EventStore::open(ws.path().join("events.sqlite")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    let seen = Arc::new(Mutex::new(None));
    let mut kernel = Kernel::for_subject(
        &cas,
        &mut store,
        "run",
        snapshot.manifest.clone(),
        review_core::SubjectKind::WholeTree,
    )
    .unwrap()
    .with_adapter("reviewer", Box::new(Recorder { seen: seen.clone() }));
    if let Some(doc) = prior {
        let artifact = cas.put_json(&serde_json::from_str(doc).unwrap()).unwrap();
        kernel = kernel.with_prior_findings(artifact);
    }
    let plan = pipeline().plan().unwrap();
    let report = Scheduler::new(&plan).run(&kernel);
    assert!(report.complete(), "{:?}", report.outcomes);
    seen.lock().unwrap().clone()
}

#[test]
fn prior_findings_arrive_through_the_port() {
    let doc = r#"{"round":1,"prior_findings":[{"key":"ab12","title":"T","file":"src/a.rs"}]}"#;
    let seen = run(Some(doc)).expect("the reviewer ran");
    let delivered = seen.expect("prior findings were delivered");
    assert_eq!(delivered["prior_findings"][0]["key"], "ab12");
}

#[test]
fn round_one_delivers_an_empty_set_as_no_prior_findings() {
    // No kernel prior findings: the generation node emits an empty set, and the reviewer sees
    // None (an empty set is not worth rendering into the prompt).
    let seen = run(None).expect("the reviewer ran");
    assert_eq!(
        seen, None,
        "an empty generation set delivers no prior findings"
    );
}
