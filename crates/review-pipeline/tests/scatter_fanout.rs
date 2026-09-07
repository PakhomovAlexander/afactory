//! Scatter shards run at the fan-out the Slice policy declares, not at the width of the shared
//! filesystem executor. This binary holds one test so it can pin that executor to a single
//! worker before anything uses it: dispatching shards on that pool would then run them one at
//! a time, and the rendezvous below — every shard holds its slot until three are in flight —
//! would time out. Bounded scoped threads reach `max_fanout` regardless.

mod support;

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use review_config::Definition;
use review_runner::{ReviewerAdapter, ReviewerInputs, ReviewerReturn, RunnerError};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::{Cas, EventStore};

const MAX_FANOUT: usize = 3;

const PIPELINE: &str = r#"version = 5

[subject]
kind = "whole-tree"

[gate]
provider = "trusted_local"
required_isolation = "none"
mode = "ephemeral-write"

[budgets]
unit = "tokens"
attempt = 100
fan_out = 300
run = 500

[[nodes]]
id = "gate"
kind = "gate"
outputs = [{ name = "decision", type = "review.kernel/GateDecision@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]

[[nodes]]
id = "slicer"
kind = "slicer"
outputs = [{ name = "slices", type = "review.kernel/SliceSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
slicing = { scatter = "scatter", max_paths_per_slice = 1, max_fanout = 3, coverage = "complete", all_shards_required = true, closeout = "required" }

[[nodes]]
id = "scatter"
kind = "scatter"
inputs = [
  { name = "slices", type = "review.kernel/SliceSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
outputs = [{ name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
runner = { program = "/bin/true" }
execution = { credential_mode = "credential_free" }

[[nodes]]
id = "closeout"
kind = "reviewer"
closeout_for = "scatter"
inputs = [
  { name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" },
]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
runner = { program = "/bin/true" }
execution = { credential_mode = "credential_free" }

[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [
  { name = "shards", type = "review.kernel/ShardSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "closeout", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
outputs = [{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]

[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "scatter", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "findings" }
to = { node = "closeout", port = "prior_findings" }
[[edges]]
from = { node = "slicer", port = "slices" }
to = { node = "scatter", port = "slices" }
[[edges]]
from = { node = "scatter", port = "shards" }
to = { node = "closeout", port = "shards" }
[[edges]]
from = { node = "scatter", port = "shards" }
to = { node = "ledger", port = "shards" }
[[edges]]
from = { node = "closeout", port = "result" }
to = { node = "ledger", port = "closeout" }
"#;

/// Every shard holds its invocation until `MAX_FANOUT` shards are in flight together, and
/// records the most that ever were.
struct Rendezvous {
    /// `(active now, maximum ever active)`, shared with the test's assertion.
    state: Arc<Mutex<(usize, usize)>>,
    arrived: Arc<Condvar>,
}

impl ReviewerAdapter for Rendezvous {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        assert_eq!(inputs.artifacts["slice"].len(), 1);
        let mut state = self.state.lock().unwrap();
        state.0 += 1;
        state.1 = state.1.max(state.0);
        self.arrived.notify_all();
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.1 < MAX_FANOUT {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.0 -= 1;
                return Err(RunnerError::Unavailable(format!(
                    "waited 10s for {MAX_FANOUT} simultaneous shards; saw at most {}",
                    state.1
                )));
            }
            state = self.arrived.wait_timeout(state, remaining).unwrap().0;
        }
        state.0 -= 1;
        drop(state);
        Ok(clean(cas, b"clean shard"))
    }
}

struct CleanCloseout;

impl ReviewerAdapter for CleanCloseout {
    fn invoke(
        &self,
        cas: &Cas,
        _root: &Path,
        _inputs: &ReviewerInputs,
    ) -> Result<ReviewerReturn, RunnerError> {
        Ok(clean(cas, b"whole-subject closeout"))
    }
}

fn clean(cas: &Cas, raw: &[u8]) -> ReviewerReturn {
    ReviewerReturn {
        output: serde_json::from_value(serde_json::json!({
            "verdict": "approve",
            "summary": null,
            "findings": [],
            "benchmark_demands": [],
            "disputes": []
        }))
        .unwrap(),
        proposal: Ok(None),
        cost_tokens: 10,
        raw_artifact: cas.put(raw).unwrap(),
    }
}

fn manifest(cas: &Cas) -> Manifest {
    Manifest::new(
        ["left.rs", "middle.rs", "right.rs"]
            .into_iter()
            .map(|path| {
                let bytes = format!("// {path}\n");
                Entry {
                    path: path.into(),
                    kind: EntryKind::File,
                    content: cas.put(bytes.as_bytes()).unwrap(),
                    size: bytes.len() as u64,
                }
            })
            .collect(),
    )
    .unwrap()
}

#[test]
fn shards_reach_the_declared_max_fanout_on_a_single_worker_executor() {
    // First use in this process: the shared filesystem executor is one worker wide from here
    // on. Shards dispatched onto it could never overlap.
    review_parallel::init_worker_limit(1).expect("this binary's first use of the executor");
    assert_eq!(review_parallel::worker_limit(), 1);

    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let snapshot = manifest(&cas);
    let loaded = Definition::from_toml(PIPELINE).unwrap().load().unwrap();
    assert_eq!(loaded.slicing()["slicer"].max_fanout as usize, MAX_FANOUT);
    let state = Arc::new(Mutex::new((0, 0)));
    let shards = Rendezvous {
        state: Arc::clone(&state),
        arrived: Arc::new(Condvar::new()),
    };
    let kernel = support::canonical_whole_tree_kernel_for_pipeline(
        &cas, &mut store, "run", snapshot, PIPELINE,
    )
    .with_checks(loaded.checks().to_vec())
    .with_budgets(100, 500)
    .with_adapter("scatter", Box::new(shards))
    .with_adapter("closeout", Box::new(CleanCloseout));

    let report = loaded.run(&kernel).unwrap();
    assert!(report.complete(), "{:?}", report.outcomes);
    let (active, max_active) = *state.lock().unwrap();
    assert_eq!(active, 0);
    assert_eq!(
        max_active, MAX_FANOUT,
        "shards must fan out to the Slice policy's max_fanout, not the executor's width"
    );
}
