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

#[test]
fn the_newest_rounds_reviewers_take_their_latest_attempts_outcome() {
    let accounting = json!([
        {"attempts": [
            attempt("correctness", 1, 1, "selected", "10"),
            attempt("bugs", 1, 1, "failed", "5"),
            // The Round's second epoch: only its Attempts are the nodes' state.
            attempt("correctness", 1, 2, "failed", "7"),
            attempt("bugs", 1, 2, "selected", "3"),
            attempt("correctness", 1, 2, "running", "0"),
            {"category": "provider", "outcome": "selected", "chargeable_tokens": "99"},
        ]},
    ]);
    let (round, found) = nodes(&accounting);
    assert_eq!(round, Some((1, 2)));
    let states: Vec<(&str, &str, u128)> = found
        .iter()
        .map(|node| (node.node.as_str(), node.outcome.as_str(), node.tokens))
        .collect();
    assert_eq!(
        states,
        [("correctness", "running", 7), ("bugs", "selected", 3)]
    );
    assert_eq!(found[0].wall_ms, Some(2_000));
    assert_eq!(found[0].tone(), Some(Tone::Active));
    assert_eq!(found[1].tone(), Some(Tone::Ok));
    let failed = NodeState {
        outcome: "failed".into(),
        ..found[1].clone()
    };
    assert_eq!(failed.tone(), Some(Tone::Fail));
    // No Attempt yet: no Round and no node.
    assert_eq!(nodes(&json!([])), (None, Vec::new()));
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
