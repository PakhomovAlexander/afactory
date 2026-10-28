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
/// the bar shows the folder with a spinner meanwhile (ADR-0240).
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
    assert_eq!(pane.started(), 0, "the load started no read");
    pane.poll();
    assert_eq!(pane.started(), 1, "the first poll after it does");
    pane.settle();
    assert!(pane.busy().is_none());
    assert_eq!(
        pane.groups.len(),
        pane.sources.len(),
        "one group per source, in order"
    );
}

/// A Store one source listed as a Campaign and another source now reports unreadable (its
/// parent directory registered as a root meanwhile, read first): the refusal is the Store's one
/// representation the moment it arrives, before the Store's own source answers, and the opened
/// Campaign is closed with it; the later source lists it no second time.
#[test]
fn a_refusal_of_a_store_another_source_listed_unlists_its_campaign_at_once() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let audit = root.join("audit");
    crate::option_tests::write_campaign_rounds(&audit, "audit", &[(1, 1)]);
    let places = Places {
        home: None,
        default_root: None,
    };
    let mut pane = CampaignsPane {
        places: Some(places.clone()),
        sources: vec![Source::Store(audit.clone())],
        ..CampaignsPane::default()
    };
    let sender = pane.fed();
    let group = read_source(&places, &Source::Store(audit.clone()));
    sender.send(Read::Group(group)).unwrap();
    sender.send(Read::Done { report: None }).unwrap();
    assert!(pane.poll());
    let id = audit.display().to_string();
    assert!(pane.campaign(&id).is_some());
    pane.selected = Some(id.clone());
    pane.sources = vec![Source::Root(root.clone()), Source::Store(audit.clone())];
    let sender = pane.fed();
    let problem = Problem {
        state: audit.clone(),
        shown: id.clone(),
        reason: "events.sqlite cannot be read".to_owned(),
        release: None,
    };
    sender
        .send(Read::Group(Group {
            source: Source::Root(root.clone()),
            shown: root.display().to_string(),
            campaigns: Vec::new(),
            problems: vec![problem],
            error: None,
        }))
        .unwrap();
    assert!(pane.poll());
    assert!(pane.busy().is_some(), "the Store's own source is unread");
    assert!(
        pane.campaign(&id).is_none(),
        "the refusal is its one representation"
    );
    assert!(pane.selected.is_none(), "the opened Campaign went with it");
    let ids: Vec<String> = pane.items().into_iter().map(|item| item.id).collect();
    assert_eq!(ids, [format!("{UNREADABLE}{id}")]);
    sender
        .send(Read::Group(Group {
            source: Source::Store(audit.clone()),
            shown: id.clone(),
            campaigns: Vec::new(),
            problems: Vec::new(),
            error: None,
        }))
        .unwrap();
    sender.send(Read::Done { report: None }).unwrap();
    assert!(pane.poll());
    let ids: Vec<String> = pane.items().into_iter().map(|item| item.id).collect();
    assert_eq!(ids, [format!("{UNREADABLE}{id}")], "listed once, as before");
}

/// A Campaign a root listed stays listed and opened while that root fails to list and the
/// Store's own source, read after it, is unread; when that source answers, the Campaign is
/// listed there, once. The groups follow the source order whenever it changes.
#[test]
fn a_campaign_stays_listed_once_while_its_sources_answer_in_turn() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let audit = root.join("audit");
    crate::option_tests::write_campaign_rounds(&audit, "audit", &[(1, 1)]);
    let places = Places {
        home: None,
        default_root: None,
    };
    let id = audit.display().to_string();
    let campaign = || {
        read_source(&places, &Source::Store(audit.clone()))
            .campaigns
            .remove(0)
    };
    let store_group = |campaigns: Vec<Campaign>| Group {
        source: Source::Store(audit.clone()),
        shown: id.clone(),
        campaigns,
        problems: Vec::new(),
        error: None,
    };
    let root_group = |campaigns: Vec<Campaign>, error: Option<&str>| Group {
        source: Source::Root(root.clone()),
        shown: root.display().to_string(),
        campaigns,
        problems: Vec::new(),
        error: error.map(str::to_owned),
    };
    let listed = |pane: &CampaignsPane| -> Vec<String> {
        pane.items().into_iter().map(|item| item.id).collect()
    };
    let mut pane = CampaignsPane {
        places: Some(places.clone()),
        sources: vec![Source::Root(root.clone()), Source::Store(audit.clone())],
        ..CampaignsPane::default()
    };
    // The root lists the Store first; its own source, read after, lists it no second time.
    let sender = pane.fed();
    sender
        .send(Read::Group(root_group(vec![campaign()], None)))
        .unwrap();
    sender.send(Read::Group(store_group(Vec::new()))).unwrap();
    sender.send(Read::Done { report: None }).unwrap();
    assert!(pane.poll());
    assert_eq!(listed(&pane), [id.clone()]);
    pane.selected = Some(id.clone());
    // The root cannot be listed this time, and the Store's own source is still unread.
    let sender = pane.fed();
    sender
        .send(Read::Group(root_group(
            Vec::new(),
            Some("permission denied"),
        )))
        .unwrap();
    assert!(pane.poll());
    assert!(
        pane.campaign(&id).is_some(),
        "listed until a source answers for it"
    );
    assert_eq!(pane.selected.as_deref(), Some(id.as_str()));
    assert_eq!(listed(&pane), [id.clone()]);
    sender
        .send(Read::Group(store_group(vec![campaign()])))
        .unwrap();
    assert!(pane.poll());
    assert_eq!(listed(&pane), [id.clone()], "once, under its own source");
    let holder = pane
        .groups
        .iter()
        .position(|group| group.campaigns.iter().any(|c| c.state == audit));
    assert_eq!(holder, Some(1));
    sender.send(Read::Done { report: None }).unwrap();
    assert!(pane.poll());
    assert_eq!(pane.selected.as_deref(), Some(id.as_str()));
    assert_eq!(listed(&pane), [id.clone()]);
    // The sources in the other order: the groups follow.
    pane.sources = vec![Source::Store(audit.clone()), Source::Root(root.clone())];
    let sender = pane.fed();
    sender
        .send(Read::Group(store_group(vec![campaign()])))
        .unwrap();
    sender
        .send(Read::Group(root_group(Vec::new(), None)))
        .unwrap();
    sender.send(Read::Done { report: None }).unwrap();
    assert!(pane.poll());
    let order: Vec<&Path> = pane
        .groups
        .iter()
        .map(|group| group.source.path())
        .collect();
    assert_eq!(order, [audit.as_path(), root.as_path()]);
    assert_eq!(pane.selected.as_deref(), Some(id.as_str()));
}

/// A read's report is taken for the opening it was started under: the Campaign opened again
/// meanwhile (the folder between) read its report afresh, and a report read under the earlier
/// opening never stands over that one, a refusal included.
#[test]
fn a_reads_report_from_an_earlier_opening_never_stands_over_a_newer_one() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let audit = root.join("audit");
    crate::option_tests::write_campaign_rounds(&audit, "audit", &[(1, 1)]);
    let places = Places {
        home: None,
        default_root: None,
    };
    let mut pane = CampaignsPane {
        places: Some(places.clone()),
        sources: vec![Source::Store(audit.clone())],
        ..CampaignsPane::default()
    };
    let sender = pane.fed();
    let group = read_source(&places, &Source::Store(audit.clone()));
    sender.send(Read::Group(group)).unwrap();
    sender.send(Read::Done { report: None }).unwrap();
    assert!(pane.poll());
    let id = audit.display().to_string();
    pane.open(Some(&id));
    let report_of = |pane: &CampaignsPane| pane.detail.as_ref().map(|detail| detail.report.is_ok());
    assert_eq!(report_of(&pane), Some(true));
    // An explicit read started under this opening read the group and the report before the
    // Store went aside; the Campaign is opened again before that read is collected, and its
    // report cannot be read now.
    let sender = pane.fed();
    let group = read_source(&places, &Source::Store(audit.clone()));
    let report = crate::browse_report(&audit, "audit");
    assert!(report.is_ok());
    std::fs::rename(&audit, root.join("aside")).unwrap();
    pane.open(None);
    pane.open(Some(&id));
    assert_eq!(
        report_of(&pane),
        Some(false),
        "reopened: the report refuses"
    );
    sender.send(Read::Group(group)).unwrap();
    sender
        .send(Read::Done {
            report: Some((audit.clone(), report)),
        })
        .unwrap();
    assert!(pane.poll());
    assert_eq!(pane.selected.as_deref(), Some(id.as_str()));
    assert_eq!(
        report_of(&pane),
        Some(false),
        "the refusal read under this opening stands over the earlier opening's report"
    );
}
