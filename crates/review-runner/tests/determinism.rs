//! Deterministic replay under controlled completion order.
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

use std::sync::mpsc;
use std::time::Duration;

use review_core::LegacyStageOutput;
use review_core::{Arg, Command};
use review_runner::{CommandRunner, Invocation, Outcome, gather};
use review_store::{Cas, EventStore, Ingest, LedgerProjection};

/// A reviewer whose output can be released explicitly by the test coordinator.
fn reviewer(name: &str, release: Option<usize>) -> Command {
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
                "{}cat <<'EOF'\n{json}\nEOF",
                release.map_or_else(String::new, |rank| format!(
                    "while [ ! -f release-{rank} ]; do sleep 0.01; done; "
                ))
            )),
        ],
    )
}

const NODES: [&str; 4] = ["architecture", "performance", "security", "tdd"];

/// Run every reviewer concurrently, returning outcomes plus the order they actually finished in.
fn run_concurrently(ranks: &[usize; 4]) -> (Vec<Outcome>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let runner = CommandRunner::new(&cas, dir.path());
    let (sender, receiver) = mpsc::channel();

    let outcomes = std::thread::scope(|scope| {
        for (node, rank) in NODES.iter().zip(ranks) {
            let runner = &runner;
            let sender = sender.clone();
            scope.spawn(move || {
                let result = runner.invoke(&reviewer(node, Some(*rank)));
                sender
                    .send(Outcome {
                        invocation: Invocation::new(*node, format!("{node}@1")),
                        result,
                    })
                    .unwrap();
            });
        }
        drop(sender);
        let mut outcomes = Vec::new();
        // All reviewers may run concurrently, but the next output is released only after
        // the previous invocation actually returns. Host load cannot reorder the control.
        for rank in 0..NODES.len() {
            std::fs::write(dir.path().join(format!("release-{rank}")), b"ready").unwrap();
            let outcome = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
            assert_eq!(
                outcome.invocation.node_id,
                NODES[ranks.iter().position(|r| *r == rank).unwrap()]
            );
            outcomes.push(outcome);
        }
        outcomes
    });
    let finished = outcomes
        .iter()
        .map(|o| o.invocation.node_id.clone())
        .collect();
    (outcomes, finished)
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

    let ledger: Vec<String> = LedgerProjection::rebuild(&store, &cas, "run")
        .unwrap()
        .ledger()
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

/// Each node's explicitly coordinated completion rank in four different permutations.
const PATTERNS: [[usize; 4]; 4] = [[0, 1, 2, 3], [3, 2, 1, 0], [2, 0, 3, 1], [1, 3, 0, 2]];

#[test]
fn canonical_admission_is_identical_under_every_completion_order() {
    let mut fingerprints = Vec::new();
    let mut completion_orders = Vec::new();

    for ranks in PATTERNS {
        let (outcomes, finished) = run_concurrently(&ranks);
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
            result: runner.invoke(&reviewer("tdd", None)),
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
