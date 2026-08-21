//! Subject support is enforced at the composition boundary, not only by reviewctl.

use std::path::Path;
use std::sync::{Arc, Mutex};

use review_config::Definition;
use review_config::lock::{Lockfile, Registry};
use review_pipeline::Kernel;
use review_runner::{ReviewerAdapter, ReviewerInputs, ReviewerReturn, RunnerError};
use review_source_git::Manifest;
use review_store::{Cas, EventStore};

mod support;

const DIFF_PIPELINE: &str = r#"
version = 2
[subject]
kind = "diff"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [
  { name = "findings", type = "review.kernel/PriorFindings@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[nodes]]
id = "reviewer"
kind = "reviewer"
package = "tester"
inputs = [{ name = "change_set", type = "review.kernel/ChangeSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
[[edges]]
from = { node = "generation", port = "change_set" }
to = { node = "reviewer", port = "change_set" }
"#;

struct Recorder {
    seen: Arc<Mutex<Option<String>>>,
}

impl ReviewerAdapter for Recorder {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        *self.seen.lock().unwrap() = inputs
            .artifacts
            .get("change_set")
            .and_then(|artifacts| artifacts.first())
            .map(|artifact| artifact.artifact_id.clone());
        Ok(ReviewerReturn {
            output: serde_json::from_str(
                r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}"#,
            )
            .unwrap(),
            cost_tokens: 1,
            raw_artifact: cas.put(b"stub").unwrap(),
        })
    }
}

#[test]
fn a_diff_subject_executes_only_with_its_exact_change_set_authority() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let reviewers = directory.path().join("reviewers");
    let package = reviewers.join("tester");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        "name = \"tester\"\nversion = \"1.0.0\"\nsubjects = [\"diff\"]\n\n\
         [runner]\nprogram = \"codex\"\nargs = []\n",
    )
    .unwrap();
    let registry = Registry::new([&reviewers]);
    let mut lockfile = Lockfile::empty();
    lockfile
        .reviewers
        .insert("tester".into(), Lockfile::pin("tester", &registry).unwrap());
    let loaded = Definition::from_toml(DIFF_PIPELINE)
        .unwrap()
        .load_with(&lockfile, &registry)
        .unwrap();

    let manifest = Manifest::new(vec![]);
    let authority =
        support::test_diff_round_authority(&cas, &mut store, "run", &manifest, DIFF_PIPELINE);
    let seen = Arc::new(Mutex::new(None));
    let kernel = Kernel::from_loaded(
        &cas,
        &mut store,
        "run",
        manifest.clone(),
        &loaded,
        authority.clone(),
    )
    .unwrap()
    .with_adapter("reviewer", Box::new(Recorder { seen: seen.clone() }));

    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    let delivered = seen
        .lock()
        .unwrap()
        .clone()
        .expect("reviewer received Change Set identity");
    let change_set: review_core::ChangeSetV1 =
        serde_json::from_value(cas.get_json(&delivered).unwrap()).unwrap();
    change_set.validate().unwrap();
}
