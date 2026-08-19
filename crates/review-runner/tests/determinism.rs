//! Deterministic replay under randomized completion order.
//!
//! Four reviewers run concurrently. Each reports one finding of its own and one finding they all
//! share — the shared one matters, because the projection gives a finding to its **first**
//! reporter and turns every later report into a duplicate. So "who owns it" is decided by
//! ingest order, and ingest order is where nondeterminism would enter.
//!
//! Two things are proved here, and the second is what makes the first meaningful:
//!
//! 1. Admitting in canonical order produces byte-identical event streams and ledgers, whatever
//!    order the reviewers actually finished in.
//! 2. Admitting in completion order genuinely produces different ledgers. Without this, test 1
//!    could pass simply because nothing was order-dependent, and the barrier would be
//!    ceremony.

use std::sync::atomic::{AtomicUsize, Ordering};

use review_core::LegacyStageOutput;
use review_core::{Arg, Command};
use review_runner::{CommandRunner, Invocation, Outcome, gather};
use review_store::{Cas, EventStore, Ingest, Ledger};

/// A reviewer that sleeps, then emits its result. The sleep is how completion order is forced
/// to differ between runs without the test itself becoming nondeterministic.
fn reviewer(name: &str, delay_ms: u64) -> Command {
    let json = format!(
        r#"{{"verdict":"request-changes","summary":null,"findings":[
             {{"severity":"major","file":"src/{name}.rs","line":10,
               "title":"{name} found something only it can see","body":"from {name}",
               "fix":"fix it","confidence":0.9}},
             {{"severity":"major","file":"src/shared.rs","line":42,
               "title":"Everyone finds this one","body":"seen by {name}",
               "fix":"fix it","confidence":0.9}}
           ],"benchmark_demands":[],"disputes":[]}}"#
    );
    Command::new(
        "/bin/sh",
        vec![
            Arg::literal("-c"),
            Arg::literal(format!(
                "sleep {}.{:03}; cat <<'EOF'\n{json}\nEOF",
                delay_ms / 1000,
                delay_ms % 1000
            )),
        ],
    )
}

const NODES: [&str; 4] = ["architecture", "performance", "security", "tdd"];

/// Run every reviewer concurrently, returning outcomes plus the order they actually finished in.
fn run_concurrently(delays: &[u64; 4]) -> (Vec<Outcome>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let runner = CommandRunner::new(&cas, dir.path());
    let counter = AtomicUsize::new(0);
    let mut finished: Vec<(usize, String)> = Vec::new();

    let results: Vec<(Outcome, usize)> = std::thread::scope(|scope| {
        let handles: Vec<_> = NODES
            .iter()
            .zip(delays.iter())
            .map(|(node, delay)| {
                let runner = &runner;
                let counter = &counter;
                scope.spawn(move || {
                    let result = runner.invoke(&reviewer(node, *delay));
                    let order = counter.fetch_add(1, Ordering::SeqCst);
                    (
                        Outcome {
                            invocation: Invocation::new(*node, format!("{node}@1")),
                            result,
                        },
                        order,
                    )
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for (outcome, order) in &results {
        finished.push((*order, outcome.invocation.node_id.clone()));
    }
    finished.sort_by_key(|(order, _)| *order);

    (
        results.into_iter().map(|(outcome, _)| outcome).collect(),
        finished.into_iter().map(|(_, node)| node).collect(),
    )
}

/// Ingest a sequence of outcomes and return a fingerprint of everything a replay must reproduce.
fn ingest(outcomes: &[Outcome]) -> (Vec<String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
    {
        let mut ingest = Ingest::new(&mut store, &cas, "run").unwrap();
        for outcome in outcomes {
            let stage: &LegacyStageOutput = outcome.result.as_ref().expect("reviewer succeeded");
            ingest
                .add_stage_output(&outcome.invocation.node_id, stage)
                .unwrap();
        }
    }

    let events: Vec<String> = store
        .replay("run")
        .unwrap()
        .into_iter()
        .map(|e| {
            format!(
                "{}#{} {} key={} source={}",
                e.event_type,
                e.sequence,
                e.correlation_id.unwrap_or_default(),
                e.payload["key"].as_str().unwrap_or_default(),
                e.payload["source"].as_str().unwrap_or_default()
            )
        })
        .collect();

    let ledger: Vec<String> = Ledger::rebuild(&store, "run")
        .unwrap()
        .findings()
        .into_iter()
        .map(|f| {
            format!(
                "{} {} {:?} r{} seen{} src={} reports={}",
                f.key,
                f.title,
                f.status,
                f.news_round,
                f.last_seen_round,
                f.source,
                f.reports.len()
            )
        })
        .collect();

    (events, ledger)
}

/// Delay patterns chosen to make the reviewers finish in different orders.
const PATTERNS: [[u64; 4]; 4] = [
    [10, 40, 70, 100],
    [100, 70, 40, 10],
    [70, 10, 100, 40],
    [40, 100, 10, 70],
];

#[test]
fn canonical_admission_is_identical_under_every_completion_order() {
    let mut fingerprints = Vec::new();
    let mut completion_orders = Vec::new();

    for delays in PATTERNS {
        let (outcomes, finished) = run_concurrently(&delays);
        assert!(
            outcomes.iter().all(|o| o.succeeded()),
            "every reviewer must have returned a result"
        );
        completion_orders.push(finished);
        fingerprints.push(ingest(&gather(outcomes)));
    }

    // The test proves nothing unless the completion orders really did differ.
    let distinct: std::collections::BTreeSet<_> = completion_orders.iter().collect();
    assert!(
        distinct.len() > 1,
        "reviewers finished in the same order every time; this test would be vacuous: {completion_orders:?}"
    );

    for (index, fingerprint) in fingerprints.iter().enumerate().skip(1) {
        assert_eq!(
            fingerprint, &fingerprints[0],
            "completion order {index} changed the run"
        );
    }

    // And the canonical owner is the pipeline's, not the machine's: the lowest node ID.
    let shared = fingerprints[0]
        .1
        .iter()
        .find(|row| row.contains("Everyone finds this one"))
        .unwrap();
    assert!(
        shared.contains("src=architecture"),
        "the shared finding should belong to the first node in canonical order: {shared}"
    );
    assert!(
        shared.contains("reports=4"),
        "all four reports stay attached: {shared}"
    );
}

#[test]
fn completion_order_really_is_order_dependent() {
    // The control. Same outcomes, admitted in the order they arrived rather than canonically.
    let (first, first_order) = run_concurrently(&PATTERNS[0]);
    let (second, second_order) = run_concurrently(&PATTERNS[1]);
    assert_ne!(
        first_order, second_order,
        "the two patterns must complete differently for this control to mean anything"
    );

    let by_completion = |outcomes: Vec<Outcome>, order: &[String]| -> Vec<Outcome> {
        let mut sorted = outcomes;
        sorted.sort_by_key(|o| {
            order
                .iter()
                .position(|n| *n == o.invocation.node_id)
                .unwrap_or(usize::MAX)
        });
        sorted
    };

    let a = ingest(&by_completion(first, &first_order));
    let b = ingest(&by_completion(second, &second_order));

    assert_ne!(
        a, b,
        "ingesting in completion order produced identical results, so the canonical barrier \
         would be proving nothing — check that the shared finding is still shared"
    );

    // Specifically: the shared finding changes owner, which is exactly the ledger's
    // first-reporter rule reaching through into scheduling.
    let owner = |fingerprint: &(Vec<String>, Vec<String>)| -> String {
        fingerprint
            .1
            .iter()
            .find(|row| row.contains("Everyone finds this one"))
            .unwrap()
            .split("src=")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .to_string()
    };
    assert_ne!(
        owner(&a),
        owner(&b),
        "the shared finding kept the same owner across different completion orders"
    );
}

/// A reviewer that fails must not vanish into an empty result — the gather order is stable for
/// failures too, and a failed reviewer is visible rather than silently absent.
#[test]
fn a_failed_reviewer_keeps_its_place() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let runner = CommandRunner::new(&cas, dir.path());

    let outcomes = gather(vec![
        Outcome {
            invocation: Invocation::new("tdd", "tdd@1"),
            result: runner.invoke(&reviewer("tdd", 0)),
        },
        Outcome {
            invocation: Invocation::new("architecture", "architecture@1"),
            result: runner.invoke(&Command::new(
                "/bin/sh",
                vec![Arg::literal("-c"), Arg::literal("echo nope >&2; exit 9")],
            )),
        },
    ]);

    assert_eq!(outcomes[0].invocation.node_id, "architecture");
    assert!(!outcomes[0].succeeded(), "the failure is still in the set");
    assert!(outcomes[1].succeeded());
}
