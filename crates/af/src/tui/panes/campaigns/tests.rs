//! The node chips and the sources, from the documents `af review report --json` prints and the
//! Store registry.

use serde_json::json;

use super::*;

fn attempt(node: &str, round: u64, epoch: u64, outcome: &str, tokens: &str) -> Value {
    json!({
        "category": "business",
        "review_node": node,
        "node": format!("root.nodes.{node}"),
        "round": round,
        "epoch": epoch,
        "outcome": outcome,
        "chargeable_tokens": tokens,
        "wall": {"started_unix_ms": 1, "elapsed_ms": 1_000},
    })
}

fn attempt_at(node: &str, id: &str, round: u64, outcome: &str, started: Option<u64>) -> Value {
    let mut value = attempt(node, round, 1, outcome, "1");
    value["attempt_id"] = json!(id);
    match started {
        Some(at) => value["wall"]["started_unix_ms"] = json!(at),
        None => {
            value.as_object_mut().unwrap().remove("wall");
        }
    }
    value
}

#[test]
fn the_newest_rounds_reviewers_take_their_latest_attempts_outcome() {
    let accounting = json!([
        {"attempts": [
            attempt("correctness", 1, 1, "selected", "10"),
            attempt("bugs", 1, 1, "failed", "5"),
            // The Round's second epoch: only its Attempts are the nodes' state.
            attempt("correctness", 1, 2, "failed", "7"),
            attempt("bugs", 1, 2, "selected", "3"),
            {"category": "business", "review_node": "correctness", "round": 1, "epoch": 2,
             "outcome": "running", "chargeable_tokens": "0"},
            {"category": "provider", "outcome": "selected", "chargeable_tokens": "99"},
        ]},
    ]);
    let found = nodes(&accounting, Some((1, 2)));
    let states: Vec<(&str, &str, u128)> = found
        .iter()
        .map(|node| (node.node.as_str(), node.outcome.as_str(), node.tokens))
        .collect();
    assert_eq!(
        states,
        [("correctness", "running", 7), ("bugs", "selected", 3)]
    );
    assert_eq!(found[0].wall_ms, Some(1_000));
    assert_eq!(found[0].tone(), Some(Tone::Active));
    assert_eq!(found[1].tone(), Some(Tone::Ok));
    let failed = NodeState {
        outcome: "failed".into(),
        ..found[1].clone()
    };
    assert_eq!(failed.tone(), Some(Tone::Fail));
    // No Round yet: no node.
    assert_eq!(nodes(&json!([]), Some((1, 1))), Vec::new());
    assert_eq!(nodes(&accounting, None), Vec::new());
}

/// Round 1 closed with its reviewers' outcomes; Round 2 started and has no Attempt yet. The
/// folder shows Round 2 without chips, never Round 1's outcomes.
#[test]
fn an_open_round_without_attempts_shows_no_earlier_rounds_chips() {
    let accounting = json!([
        {"attempts": [
            attempt("bugs", 1, 1, "selected", "10"),
            attempt("correctness", 1, 1, "failed", "5"),
        ]},
    ]);
    assert_eq!(nodes(&accounting, Some((2, 1))), Vec::new());
    assert_eq!(nodes(&accounting, Some((1, 1))).len(), 2);
}

/// The accounting lists Attempts by their hash-derived ID, as `af review report` produces
/// them: here the failed Attempt's ID sorts after its successful retry's. The chip is the
/// retry's, the Attempt that started last.
#[test]
fn a_retry_that_succeeded_is_the_chip_whatever_its_id_sorts_as() {
    // The producer's order: by Attempt ID.
    let accounting = json!([
        {"attempts": [
            attempt_at("bugs", "sha256:0000", 1, "selected", Some(2_000)),
            attempt_at("bugs", "sha256:ffff", 1, "failed", Some(1_000)),
        ]},
    ]);
    let found = nodes(&accounting, Some((1, 1)));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].outcome, "selected");
    // A retry still running has no wall yet: it is the newest.
    let running = json!([
        {"attempts": [
            attempt_at("bugs", "sha256:0000", 1, "running", None),
            attempt_at("bugs", "sha256:ffff", 1, "failed", Some(1_000)),
        ]},
    ]);
    assert_eq!(nodes(&running, Some((1, 1)))[0].outcome, "running");
}

/// Only a missing log means "no Campaign here"; a log that cannot be looked at is a problem
/// that names the path and the error.
#[test]
fn an_unreadable_campaign_store_is_a_problem_not_left_out() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let places = Places {
        home: None,
        default_root: None,
    };
    let empty = temp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(read_dir(&places, &empty).is_none(), "no Store, no Campaign");
    let locked = temp.path().join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("events.sqlite"), b"").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let read = read_dir(&places, &locked);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Root reads through any mode; there the Store is simply read (and refused as no Store).
    if nix::unistd::geteuid().is_root() {
        return;
    }
    let Some(Err(problem)) = read else {
        panic!("an unreadable Store was left out");
    };
    assert!(
        problem.reason.contains("events.sqlite cannot be read")
            && problem.reason.contains("ermission denied"),
        "{}",
        problem.reason
    );
    assert_eq!(problem.state, locked);
}

#[test]
fn the_sources_are_the_default_roots_and_every_registered_review_store() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let repo = crate::tui::tests::hub_repo(&root);
    let scope = crate::tui::tests::hub_scope(&root, &repo);
    let review = root.join("state/af/review");
    let (only, warning) = sources(&scope);
    assert_eq!(
        only,
        [
            Source::Root(review.join("campaigns")),
            Source::Local(review.join("local"))
        ]
    );
    assert_eq!(warning, None);
    let registry = registry::location(&root.join("state"));
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    let entry = |path: &str, kind: &str| {
        format!(
            "\n[[store]]\npath = \"{path}\"\nkind = \"{kind}\"\nfirst_use_unix_ms = 1\n\
             last_use_unix_ms = 1\n"
        )
    };
    let text = format!(
        "version = 2\n{}{}{}",
        entry("/elsewhere/one", "review"),
        entry("/elsewhere/root", "review_root"),
        entry("/elsewhere/tasks", "task"),
    );
    std::fs::write(&registry, text).unwrap();
    let (all, _) = sources(&scope);
    assert_eq!(
        all[2..],
        [
            Source::Store(PathBuf::from("/elsewhere/one")),
            Source::Root(PathBuf::from("/elsewhere/root")),
        ]
    );
    // A registry this release cannot read is said, and the default roots are still read.
    std::fs::write(&registry, "version = 1\n").unwrap();
    let (some, warning) = sources(&scope);
    assert_eq!(some.len(), 2);
    assert!(warning.unwrap().contains("version 1"));
}

/// A load reads every source on a thread and lists nothing until the event loop collects it:
/// the bar shows the folder with a spinner meanwhile (ADR-0239).
#[test]
fn a_load_lists_its_sources_behind_a_spinner_never_on_the_callers_thread() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let repo = crate::tui::tests::hub_repo(&root);
    let scope = crate::tui::tests::hub_scope(&root, &repo);
    let mut pane = CampaignsPane::default();
    pane.load(&scope).unwrap();
    assert!(pane.busy().is_some(), "the read runs behind the folder");
    assert!(
        pane.groups.is_empty(),
        "nothing is listed before the read is collected"
    );
    pane.settle();
    assert!(pane.busy().is_none());
    assert_eq!(
        pane.groups.len(),
        pane.sources.len(),
        "one group per source, in order"
    );
}
