//! Grouping, fitting, progress and the stage marks, from Tasks recorded in a real Store.

use serde_json::json;

use super::*;

fn document(state: &Path, task_id: &str) -> Value {
    task_execution::inspection_document(state, task_id, true)
        .unwrap()
        .unwrap()
}

fn marks(state: &Path, document: &Value) -> Vec<(String, Mark)> {
    let mut cache = Cache::default();
    let stages = stages(document, &mut |id| node_of(state, id, &mut cache)).unwrap();
    let marks = stages
        .iter()
        .map(|stage| (stage_name(&stage.node), stage.mark));
    marks.collect()
}

fn stage(state: &Path, document: &Value, name: &str) -> Stage {
    let mut cache = Cache::default();
    let stages = stages(document, &mut |id| node_of(state, id, &mut cache)).unwrap();
    let found = stages
        .into_iter()
        .find(|stage| stage_name(&stage.node) == name);
    found.unwrap()
}

/// The document as it read while the Store held only its events up to `sequence`: the Store
/// is append-only, so a prefix of a finished Task's events is what its running self recorded.
fn running_at(document: &Value, sequence: u64) -> Value {
    let mut earlier = document.clone();
    let history = array(&document["history"]);
    let kept: Vec<Value> = history
        .iter()
        .filter(|event| event["sequence"].as_u64() <= Some(sequence))
        .cloned()
        .collect();
    let ids: Vec<&str> = kept
        .iter()
        .filter_map(|event| event["transition"]["change"]["record_id"].as_str())
        .collect();
    let records = array(&document["execution_records"])
        .iter()
        .filter(|entry| {
            let id = entry["artifact_id"].as_str().unwrap_or_default();
            ids.contains(&id)
        });
    earlier["execution_records"] = json!(records.cloned().collect::<Vec<_>>());
    earlier["history"] = json!(kept);
    earlier["phase"] = json!({"kind": "running"});
    earlier["run_reports"] = json!([]);
    let object = earlier.as_object_mut().unwrap();
    object.remove("result");
    object.remove("attempt_walls");
    object.remove("runtime_observations");
    earlier
}

/// The sequence of the first event recording an execution record of `kind`.
fn sequence_of(document: &Value, kind: &str, nth: usize) -> u64 {
    let mut kinds = BTreeMap::new();
    for entry in array(&document["execution_records"]) {
        let id = entry["artifact_id"].as_str().unwrap();
        kinds.insert(id, entry["record"]["kind"].as_str().unwrap());
    }
    let found = array(&document["history"]).iter().filter(|event| {
        let id = event["transition"]["change"]["record_id"].as_str();
        id.and_then(|id| kinds.get(id)) == Some(&kind)
    });
    found.clone().nth(nth).unwrap()["sequence"]
        .as_u64()
        .unwrap()
}

#[test]
fn tasks_group_by_phase_and_by_the_acceptance_a_finished_task_records() {
    let finished = json!({"kind": "finished", "result_id": "sha256:00"});
    let cases = [
        (json!({"kind": "submitted"}), None, State::Running),
        (json!({"kind": "running"}), None, State::Running),
        (
            json!({"kind": "waiting", "reason": "needs_input"}),
            None,
            State::Running,
        ),
        (json!({"kind": "ready"}), None, State::Awaiting),
        (
            json!({"kind": "waiting", "reason": "needs_plan_review"}),
            None,
            State::Awaiting,
        ),
        (finished.clone(), Some("satisfied"), State::Done),
        (finished.clone(), Some("unsatisfied"), State::Failed),
        (finished.clone(), Some("inconclusive"), State::Failed),
        (finished, None, State::Failed),
    ];
    for (phase, acceptance, expected) in cases {
        assert_eq!(
            state_of(&phase, acceptance),
            expected,
            "{phase} {acceptance:?}"
        );
    }
    let words: Vec<_> = State::ALL.iter().map(|state| state.word()).collect();
    assert_eq!(words, ["running", "awaiting approval", "done", "failed"]);
}

#[test]
fn a_bar_row_fits_its_width_cutting_the_outcome_first() {
    assert_eq!(fit("t-1", "verified", "100%", 19), "t-1  verified  100%");
    assert_eq!(fit("t-01", "verified", "50%", 17), "t-01  verif~  50%");
    assert_eq!(fit("pagination", "verified", "50%", 19), "pagination  50%");
    assert_eq!(
        fit("pagination-cli", "verified", "100%", 20),
        "pagination-cli  100%"
    );
    assert_eq!(
        fit("pagination-cli", "verified", "100%", 19),
        "pagination-c~  100%"
    );
    assert_eq!(
        fit("pagination-unfinished", "failed", "0%", 17),
        "pagination-u~  0%"
    );
    let progress = Progress {
        settled: 1,
        stages: 6,
    };
    assert_eq!(progress.percent(), 16);
    let none = Progress {
        settled: 0,
        stages: 0,
    };
    assert_eq!(none.percent(), 0);
    assert_eq!(duration(933), "933ms");
    assert_eq!(duration(1_250), "1.2s");
    assert_eq!(duration(123_000), "2m 03s");
    assert_eq!(duration(7_380_000), "2h 03m");
    assert_eq!(clock(1_790_263_060_692), "15:17:40Z");
    assert_eq!(short(Some("sha256:0123456789abcdef")), "01234567");
    assert_eq!(short(None), "-");
    assert_eq!(stage_name("root.nodes.check"), "check");
    assert_eq!(stage_name("root.inputs"), "inputs");
    assert_eq!(stage_name("root.nodes.review.nodes.bugs"), "review.bugs");
}

#[test]
fn stage_marks_and_progress_follow_the_recorded_task() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let names = ["inputs", "implement", "seal", "check", "evaluate", "accept"];
    let with = |marks: [Mark; 6]| -> Vec<(String, Mark)> {
        names
            .iter()
            .map(|name| name.to_string())
            .zip(marks)
            .collect()
    };

    // A satisfied Task: every stage of its graph order settled.
    let done = document(&state, "pagination-cli");
    assert_eq!(marks(&state, &done), with([Mark::Ok; 6]));
    let check = stage(&state, &done, "check");
    assert_eq!((check.attempts, check.max_attempts), (1, Some(1)));
    assert_eq!(check.tokens, Some(0));
    let wall = array(&done["attempt_walls"])[0]["elapsed_ms"].as_u64();
    assert_eq!(check.wall_ms, wall);
    assert_eq!(
        stage(&state, &done, "seal").tokens,
        None,
        "no Attempt settled"
    );

    // Its check failed: the run report suppressed the evaluator, which counts as settled.
    let failed = document(&state, "pagination-unfinished");
    let mut expected = [Mark::Ok; 6];
    expected[4] = Mark::Skipped;
    assert_eq!(marks(&state, &failed), with(expected));
    let mut cache = Cache::default();
    let summary = summary(&state, "pagination-unfinished", &mut cache).unwrap();
    assert_eq!(summary.acceptance.as_deref(), Some("unsatisfied"));
    assert_eq!(summary.progress.percent(), 100);

    // While the implementer ran: the inputs are published, the implementer runs its first of
    // one Attempt, and nothing after it was reached.
    let started = sequence_of(&done, "started", 0);
    let running = running_at(&done, started);
    let mut expected = [Mark::NotReached; 6];
    expected[0] = Mark::Ok;
    expected[1] = Mark::Running;
    assert_eq!(marks(&state, &running), with(expected));
    let implement = stage(&state, &running, "implement");
    assert_eq!((implement.attempts, implement.max_attempts), (1, Some(1)));
    let stages = stages(&running, &mut |id| node_of(&state, id, &mut cache)).unwrap();
    assert_eq!(Progress::of(&stages).percent(), 16);
    let row = stage_row(&implement).text();
    assert!(row.starts_with("  [..]  implement"), "{row}");
    assert!(row.ends_with("running, attempt 1/1"), "{row}");

    // The implementer's Attempt settled failed, and nothing retried it yet.
    let settled = sequence_of(&done, "settled", 0);
    let mut retrying = running_at(&done, settled);
    for entry in retrying["execution_records"].as_array_mut().unwrap() {
        if entry["record"]["kind"] == "settled" {
            entry["record"]["result"] = json!({"kind": "failed", "diagnostic_id": "sha256:00"});
        }
    }
    expected[1] = Mark::Failed;
    assert_eq!(marks(&state, &retrying), with(expected));
    let row = stage_row(&stage(&state, &retrying, "implement")).text();
    assert!(row.contains("[!!]  implement"), "{row}");
    assert!(row.ends_with("failed, attempt 1/1"), "{row}");

    // A report that failed a node before any Attempt marks it failed.
    let mut refused = failed.clone();
    let nodes = refused["run_reports"][0]["report"]["nodes"].as_array_mut();
    for node in nodes.unwrap() {
        if node["node"] == "root.nodes.evaluate" {
            node["outcome"] =
                json!({"kind": "failed", "diagnostic_id": "sha256:00", "class": "execution"});
        }
    }
    assert_eq!(
        marks(&state, &refused)[4],
        ("evaluate".to_owned(), Mark::Failed)
    );
}

#[test]
fn the_pane_reads_the_show_document_with_the_plan_and_its_graph() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    for task_id in ["pagination-cli", "pagination-unfinished"] {
        let mut explained = document(&state, task_id);
        let show = task_execution::inspection_document(&state, task_id, false);
        let show = show.unwrap().unwrap();
        let object = explained.as_object_mut().unwrap();
        assert!(object.remove("graph").is_some() && object.remove("plan").is_some());
        assert_eq!(explained, show);
    }
    // A Task the Store does not hold, and a directory with no Store, are no document.
    let missing = task_execution::inspection_document(&state, "absent", false);
    assert_eq!(missing, Ok(None));
    let nowhere = task_execution::inspection_document(&root.join("none"), "absent", false);
    assert_eq!(nowhere, Ok(None));
}

#[test]
fn only_the_current_plans_records_make_its_stages() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let done = document(&state, "pagination-cli");
    // A refresh installed a new plan with the same node names: none of the old plan's
    // records, Attempts, walls or charges belong to it.
    let mut replanned = done.clone();
    replanned["plan_id"] =
        json!("sha256:0000000000000000000000000000000000000000000000000000000000000000");
    replanned["run_reports"] = json!([]);
    replanned["phase"] = json!({"kind": "running"});
    let mut cache = Cache::default();
    let stages = stages(&replanned, &mut |id| node_of(&state, id, &mut cache)).unwrap();
    assert!(
        stages.iter().all(|stage| stage.mark == Mark::NotReached),
        "{stages:#?}"
    );
    assert!(
        stages
            .iter()
            .all(|stage| stage.attempts == 0 && stage.wall_ms.is_none())
    );
    assert!(stages.iter().all(|stage| stage.tokens.is_none()));
    assert_eq!(Progress::of(&stages).percent(), 0);
}

#[test]
fn a_scatter_completion_settles_its_parent_and_a_bare_invocation_is_not_running() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let done = document(&state, "pagination-cli");
    let started = sequence_of(&done, "started", 0);
    let running = running_at(&done, started);
    // The inputs node published its output without an Attempt; recorded instead as a dynamic
    // Scatter's completion of its parent, the node is settled just the same.
    let mut scattered = running.clone();
    for entry in scattered["execution_records"].as_array_mut().unwrap() {
        let record = &mut entry["record"];
        if record["kind"] == "published" && record["attempt_id"].is_null() {
            let output = record["output_id"].clone();
            *record = json!({"kind": "owned_children_completed",
                "child_set_id": "sha256:01", "output_id": output});
        }
    }
    assert_eq!(
        marks(&state, &scattered)[0],
        ("inputs".to_owned(), Mark::Ok)
    );
    // The implementer invoked but holding no reserved Attempt yet: not running.
    let mut invoked = running.clone();
    let kept: Vec<Value> = array(&running["execution_records"])
        .iter()
        .filter(|entry| {
            entry["record"]["attempt_id"].is_null() || entry["record"]["kind"] == "published"
        })
        .cloned()
        .collect();
    invoked["execution_records"] = json!(kept);
    assert_eq!(
        marks(&state, &invoked)[1],
        ("implement".to_owned(), Mark::NotReached)
    );
}

#[test]
fn a_finished_task_runs_until_it_finished_whatever_it_records_later() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let done = document(&state, "pagination-cli");
    let (first, end) = span_of(&done).unwrap();
    let finished = array(&done["history"])
        .iter()
        .find(|event| event["transition"]["change"]["kind"] == "finished")
        .unwrap()["transition"]["now_unix_ms"]
        .as_u64()
        .unwrap();
    assert_eq!(end, finished);
    // A delivery ten days later is history, not run time.
    let mut delivered = done.clone();
    let later = finished + 10 * 86_400_000;
    delivered["history"].as_array_mut().unwrap().push(json!({
        "sequence": 9999,
        "transition": {"writer": "cli", "epoch": 9, "now_unix_ms": later,
            "change": {"kind": "lease_released"}}
    }));
    assert_eq!(span_of(&delivered), Some((first, finished)));
}

#[test]
fn a_task_the_store_cannot_inspect_is_the_stores_refusal() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let unfinished = document(&state, "pagination-unfinished");
    // An invocation its records name is gone from the Store: the Task still lists, but the
    // pane cannot tell which node its records belong to.
    let invocation = array(&unfinished["execution_records"])
        .iter()
        .find_map(|entry| entry["record"]["invocation_id"].as_str())
        .unwrap()
        .trim_start_matches("sha256:")
        .to_owned();
    let object = state
        .join("cas/objects")
        .join(&invocation[..2])
        .join(&invocation[2..]);
    std::fs::remove_file(&object).unwrap();
    let mut cache = Cache::default();
    let store = read_store(&state, "state", None, &mut cache);
    // Whether listing or inspecting hits the gap, the Store is refused with the cause, and
    // no Task of it is grouped by a summary it could not read.
    let error = store.tasks.as_ref().err().expect("the Store is refused");
    assert!(error.contains(&invocation), "{error}");
    let items = store_items(&store, 0);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "! Store unreadable");
}

#[test]
fn a_history_row_opens_the_artifact_its_change_is_about() {
    let digest = |n: u8| format!("sha256:{}", format!("{n:02x}").repeat(32));
    let revoked = json!({"kind": "approval_revoked", "decision_id": digest(1),
        "reason": "stale", "revocation_id": digest(2)});
    assert_eq!(change_artifact(&revoked), Some(digest(2).as_str()));
    let integrated = json!({"kind": "review_integration_finished", "phase_id": digest(3),
        "report_id": digest(4), "integration_committed_event_id": digest(5)});
    assert_eq!(change_artifact(&integrated), Some(digest(4).as_str()));
    let finished = json!({"kind": "finished", "result_id": digest(6)});
    assert_eq!(change_artifact(&finished), Some(digest(6).as_str()));
    assert_eq!(change_artifact(&json!({"kind": "lease_released"})), None);
    assert_eq!(
        change_artifact(&json!({"kind": "opened", "revision_id": "not-a-digest"})),
        None
    );
}

#[test]
fn opening_a_task_the_store_can_no_longer_inspect_refuses_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let mut cache = Cache::default();
    let mut stores = vec![read_store(&state, "state", None, &mut cache)];
    assert!(stores[0].tasks.is_ok(), "the folder loaded");
    // After the folder loaded, the Task's revision is gone; opening it cannot be read.
    let unfinished = document(&state, "pagination-unfinished");
    let revision = unfinished["revision_id"]
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:");
    std::fs::remove_file(
        state
            .join("cas/objects")
            .join(&revision[..2])
            .join(&revision[2..]),
    )
    .unwrap();
    let error = Detail::read(&state, "pagination-unfinished", &mut cache)
        .err()
        .unwrap();
    refuse(&mut stores, &state, "pagination-unfinished", &error);
    let refused = stores[0].tasks.as_ref().err().unwrap();
    assert!(
        refused.starts_with("Task pagination-unfinished: "),
        "{refused}"
    );
    assert_eq!(store_items(&stores[0], 0)[0].label, "! Store unreadable");
}

#[test]
fn only_a_missing_store_is_empty() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    // Absent, or present and empty: no Tasks yet.
    assert_eq!(not_a_store(&root.join("absent")), None);
    std::fs::create_dir_all(root.join("empty")).unwrap();
    assert_eq!(not_a_store(&root.join("empty")), None);
    assert_eq!(
        task_execution::store_present(&root.join("empty")),
        Ok(false)
    );
    // Present with entries in another layout: refused, never "no Tasks".
    std::fs::create_dir_all(root.join("other/cas")).unwrap();
    let refusal = not_a_store(&root.join("other")).unwrap();
    assert!(refusal.contains("no events.sqlite"), "{refusal}");
    let mut cache = Cache::default();
    let store = read_store(&root.join("other"), "other", None, &mut cache);
    assert!(store.tasks.is_err());
    // A directory that may be searched but not listed is refused, not taken as empty.
    std::fs::create_dir_all(root.join("unlistable/cas")).unwrap();
    let search_only = std::os::unix::fs::PermissionsExt::from_mode(0o111);
    std::fs::set_permissions(root.join("unlistable"), search_only).unwrap();
    let refused = not_a_store(&root.join("unlistable"));
    let listable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(root.join("unlistable"), listable).unwrap();
    assert!(refused.is_some_and(|why| why.contains("unlistable")));
    // A dangling `events.sqlite` link is a Store that cannot be read.
    std::fs::create_dir_all(root.join("dangling")).unwrap();
    std::os::unix::fs::symlink("missing.sqlite", root.join("dangling/events.sqlite")).unwrap();
    let error = task_execution::store_present(&root.join("dangling")).unwrap_err();
    assert!(error.contains("link to nothing"), "{error}");
    let store = read_store(&root.join("dangling"), "dangling", None, &mut cache);
    assert!(store.tasks.is_err());
    // A Store the process may not traverse is an error, not an empty listing.
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root.join("hub-root"));
    let locked = std::os::unix::fs::PermissionsExt::from_mode(0o000);
    std::fs::set_permissions(&state, locked).unwrap();
    let listed = task_execution::list_common(&state);
    let open = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&state, open).unwrap();
    let error = listed.unwrap_err();
    assert!(error.contains("events.sqlite"), "{error}");
}

#[test]
fn every_number_the_task_pane_shows_is_the_show_documents() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let show = document(&state, "pagination-cli");
    let mut cache = Cache::default();
    let detail = Detail::read(&state, "pagination-cli", &mut cache).unwrap();
    let (rows, _) = detail.rows(0);
    let text: Vec<String> = rows.iter().map(Row::text).collect();
    let line = |prefix: &str| {
        text.iter()
            .find(|row| row.starts_with(prefix))
            .unwrap()
            .clone()
    };

    // TOKENS: chargeable as recorded, each component the sum over the Attempts' usage.
    let walls = array(&show["attempt_walls"]);
    // A component is the sum only when every Attempt's usage carries it; otherwise `-`.
    let sum = |key: &str| -> String {
        let values: Option<Vec<u128>> = walls
            .iter()
            .map(|wall| wall["usage"][key].as_str().and_then(|n| n.parse().ok()))
            .collect();
        match values {
            Some(values) if !values.is_empty() => values.iter().sum::<u128>().to_string(),
            _ => "-".to_owned(),
        }
    };
    let expected = format!(
        "TOKENS  chargeable {}  input {}  output {}  cache read {}  reasoning {}",
        show["chargeable_tokens"].as_str().unwrap(),
        sum("input_tokens"),
        sum("output_tokens"),
        sum("cache_read_tokens"),
        sum("reasoning_tokens"),
    );
    assert_eq!(line("TOKENS"), expected);

    // TIME and PLAN: from the first event to the `finished` transition.
    let history = array(&show["history"]);
    let at = |event: &Value| event["transition"]["now_unix_ms"].as_u64().unwrap();
    let finished = history
        .iter()
        .find(|event| event["transition"]["change"]["kind"] == "finished")
        .unwrap();
    let elapsed = duration(at(finished) - at(&history[0]));
    assert!(
        line("TIME").starts_with(&format!("TIME  wall {elapsed}  ")),
        "{}",
        line("TIME")
    );
    assert!(
        line("PLAN").ends_with(&format!("elapsed {elapsed}")),
        "{}",
        line("PLAN")
    );

    // PROGRESS: each stage's wall is its Attempts' recorded walls, and its charge the sum of
    // its settled Attempts' `charged_tokens`, both straight from the document.
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    let mut charged: BTreeMap<String, u128> = BTreeMap::new();
    for entry in array(&show["execution_records"]) {
        let record = &entry["record"];
        match record["kind"].as_str() {
            Some("reserved") => {
                let invocation = record["invocation_id"].as_str().unwrap();
                let node = node_of(&state, invocation, &mut cache).unwrap().node;
                owner.insert(record["attempt_id"].as_str().unwrap().to_owned(), node);
            }
            Some("settled") => {
                let node = owner[record["attempt_id"].as_str().unwrap()].clone();
                let n: u128 = record["charged_tokens"].as_str().unwrap().parse().unwrap();
                *charged.entry(node).or_default() += n;
            }
            _ => {}
        }
    }
    for stage in &detail.stages {
        let own: Vec<&Value> = walls
            .iter()
            .filter(|wall| wall["node_id"] == stage.node.as_str())
            .collect();
        let wall = (!own.is_empty()).then(|| {
            own.iter()
                .map(|wall| wall["elapsed_ms"].as_u64().unwrap())
                .sum::<u64>()
        });
        assert_eq!(stage.wall_ms, wall, "{}", stage.node);
        assert_eq!(
            stage.tokens,
            charged.get(&stage.node).copied(),
            "{}",
            stage.node
        );
        let row = stage_row(stage).text();
        if let Some(wall) = wall {
            assert!(row.contains(&duration(wall)), "{row}");
        }
        if let Some(tokens) = stage.tokens {
            assert!(row.contains(&format!("{tokens} tok")), "{row}");
        }
    }
    let settled = detail.stages.iter().filter(|stage| stage.closed).count();
    assert_eq!(
        line("PROGRESS"),
        format!("PROGRESS  {settled} / {} stages", detail.stages.len())
    );
}

#[test]
fn an_explicit_read_inspects_finished_tasks_again() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let mut cache = Cache::default();
    assert!(read_store(&state, "state", None, &mut cache).tasks.is_ok());
    assert!(!cache.finished.is_empty(), "finished summaries were cached");
    // A finished Task loses an artifact only its inspection reads.
    let done = document(&state, "pagination-cli");
    let plan = done["plan_id"]
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_owned();
    std::fs::remove_file(state.join("cas/objects").join(&plan[..2]).join(&plan[2..])).unwrap();
    // The live read's cache would still group it; an explicit read (load, `R`) clears it.
    cache.finished.clear();
    let store = read_store(&state, "state", None, &mut cache);
    assert!(
        store.tasks.is_err(),
        "the Store is refused once the Task cannot be inspected"
    );
}

#[test]
fn a_failed_attempt_with_a_retry_left_is_not_a_closed_stage() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let done = document(&state, "pagination-cli");
    let settled = sequence_of(&done, "settled", 0);
    let mut retrying = running_at(&done, settled);
    for entry in retrying["execution_records"].as_array_mut().unwrap() {
        if entry["record"]["kind"] == "settled" {
            entry["record"]["result"] = json!({"kind": "failed", "diagnostic_id": "sha256:00"});
        }
    }
    // The implementer may run twice: its first failure is shown failed, not closed.
    retrying["graph"]["allowances"]["root.nodes.implement"]["max_attempts"] = json!(2);
    let implement = stage(&state, &retrying, "implement");
    assert_eq!(implement.mark, Mark::Failed);
    assert!(!implement.closed, "a retry remains");
    // With one Attempt allowed, the same failure closes the stage.
    retrying["graph"]["allowances"]["root.nodes.implement"]["max_attempts"] = json!(1);
    assert!(stage(&state, &retrying, "implement").closed);
}

#[test]
fn a_refreshed_task_runs_until_its_last_finish() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let done = document(&state, "pagination-cli");
    let (first, end) = span_of(&done).unwrap();
    let mut refreshed = done.clone();
    let later = end + 60_000;
    let history = refreshed["history"].as_array_mut().unwrap();
    history.push(json!({"sequence": 9998, "transition": {"writer": "cli", "epoch": 9,
        "now_unix_ms": end + 1_000, "change": {"kind": "source_refreshed", "revision_id": "sha256:01"}}}));
    history.push(
        json!({"sequence": 9999, "transition": {"writer": "cli", "epoch": 9,
        "now_unix_ms": later, "change": {"kind": "finished", "result_id": "sha256:02"}}}),
    );
    assert_eq!(span_of(&refreshed), Some((first, later)));
}

#[test]
fn the_bar_groups_a_task_by_the_phase_its_inspection_read() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let mut cache = Cache::default();
    let store = read_store(&state, "state", None, &mut cache);
    let mut tasks = store.tasks.unwrap();
    let listed = tasks
        .iter_mut()
        .find(|task| task.task_id == "pagination-cli")
        .unwrap();
    // The list read still saw it running; the inspection read it finished and satisfied.
    listed.entry["phase"] = json!({"kind": "running"});
    listed.entry["outcome"] = serde_json::Value::Null;
    listed.entry["chargeable_tokens"] = json!("999");
    assert_eq!(listed.state(), State::Done);
    // Its outcome and charge come from that same read, not the older list entry.
    let show = document(&state, "pagination-cli");
    assert_eq!(
        listed.outcome(),
        show["result"]["domain_conclusion"].as_str().unwrap()
    );
    let row = folder_row(listed).text();
    assert!(
        row.contains(&format!(
            "{} tok",
            show["chargeable_tokens"].as_str().unwrap()
        )),
        "{row}"
    );
    assert!(!row.contains("999 tok"), "{row}");
}

#[test]
fn a_released_reservation_is_no_attempt_and_an_observed_charge_counts() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let done = document(&state, "pagination-cli");
    let base = stage(&state, &done, "implement");
    let records = done["execution_records"].as_array().unwrap();
    let reserved = records
        .iter()
        .find(|entry| {
            entry["record"]["kind"] == "reserved"
                && stage_name(
                    &node_of(
                        &state,
                        entry["record"]["invocation_id"].as_str().unwrap(),
                        &mut Cache::default(),
                    )
                    .unwrap()
                    .node,
                ) == "implement"
        })
        .unwrap()
        .clone();
    let attempt = reserved["record"]["attempt_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A reservation released before dispatch, then the real one: still one Attempt.
    let mut released = done.clone();
    let mut early = reserved.clone();
    early["record"]["attempt_id"] = json!("released-attempt");
    let list = released["execution_records"].as_array_mut().unwrap();
    let at = list.iter().position(|entry| entry == &reserved).unwrap();
    list.insert(
        at,
        json!({"artifact_id": "sha256:aa", "record":
        {"kind": "released", "attempt_id": "released-attempt", "reason": "recovery"}}),
    );
    list.insert(at, early);
    assert_eq!(
        stage(&state, &released, "implement").attempts,
        base.attempts
    );
    // A usage observation above the settlement's charge is the charge that counts.
    let mut observed = done.clone();
    let list = observed["execution_records"].as_array_mut().unwrap();
    let settled = list
        .iter()
        .position(|entry| {
            entry["record"]["kind"] == "settled"
                && entry["record"]["attempt_id"] == attempt.as_str()
        })
        .unwrap();
    list.insert(settled, json!({"artifact_id": "sha256:bb", "record": {"kind": "usage_observed",
        "attempt_id": attempt, "charged_tokens": "100", "usage_id": "sha256:cc", "raw_artifact_ids": []}}));
    assert_eq!(stage(&state, &observed, "implement").tokens, Some(100));
    // The same observation recorded after the settlement raises the charge too.
    let mut late = done.clone();
    late["execution_records"]
        .as_array_mut()
        .unwrap()
        .push(json!({"artifact_id": "sha256:dd",
        "record": {"kind": "usage_observed", "attempt_id": attempt, "charged_tokens": "100",
        "usage_id": "sha256:ee", "raw_artifact_ids": []}}));
    assert_eq!(stage(&state, &late, "implement").tokens, Some(100));
}

#[test]
fn a_failed_report_of_an_unfinished_task_leaves_a_retryable_stage_open() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let mut failed = document(&state, "pagination-unfinished");
    let nodes = failed["run_reports"][0]["report"]["nodes"]
        .as_array_mut()
        .unwrap();
    for node in nodes {
        if node["node"] == "root.nodes.evaluate" {
            node["outcome"] =
                json!({"kind": "failed", "diagnostic_id": "sha256:00", "class": "execution"});
        }
    }
    failed["phase"] = json!({"kind": "running"});
    failed["graph"]["allowances"]["root.nodes.evaluate"]["max_attempts"] = json!(3);
    let evaluate = stage(&state, &failed, "evaluate");
    assert_eq!(evaluate.mark, Mark::Failed);
    assert!(!evaluate.closed, "the plan may run it again");
}

#[test]
fn a_record_newer_than_the_last_report_decides_the_stage() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let mut retried = document(&state, "pagination-cli");
    retried["phase"] = json!({"kind": "running"});
    let report = &mut retried["run_reports"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["report"];
    for node in report["nodes"].as_array_mut().unwrap() {
        if node["node"] == "root.nodes.check" {
            node["outcome"] =
                json!({"kind": "failed", "diagnostic_id": "sha256:00", "class": "execution"});
        }
    }
    // The report was written before the check's records: the recorded success decides.
    report["through_sequence"] = json!(1);
    assert_eq!(stage(&state, &retried, "check").mark, Mark::Ok);
    // Written after them, the report's failure decides.
    let report = &mut retried["run_reports"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["report"];
    report["through_sequence"] = json!(1_000_000);
    assert_eq!(stage(&state, &retried, "check").mark, Mark::Failed);
    // A later report that suppressed it shows it skipped, over the older success.
    let report = &mut retried["run_reports"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["report"];
    for node in report["nodes"].as_array_mut().unwrap() {
        if node["node"] == "root.nodes.check" {
            node["outcome"] = json!({"kind": "suppressed"});
        }
    }
    assert_eq!(stage(&state, &retried, "check").mark, Mark::Skipped);
}

#[test]
fn opening_a_task_rebuilds_its_bar_row_from_the_same_read() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let mut cache = Cache::default();
    let mut pane = TasksPane {
        targets: vec![Target {
            dir: state.clone(),
            shown: "state".into(),
            repo: None,
        }],
        ..TasksPane::default()
    };
    pane.stores = vec![read_store(&state, "state", None, &mut cache)];
    // The bar read the Task while it still ran, with an old outcome and charge.
    for task in pane.stores[0].tasks.as_mut().unwrap() {
        if task.task_id == "pagination-cli" {
            let summary = task.summary.as_mut().unwrap();
            summary.phase = json!({"kind": "running"});
            summary.outcome = None;
            summary.chargeable = Some("1".into());
        }
    }
    pane.open(Some("pagination-cli"));
    let (_, task) = pane.task("pagination-cli").unwrap();
    assert_eq!(task.state(), State::Done);
    let show = document(&state, "pagination-cli");
    assert_eq!(
        task.outcome(),
        show["result"]["domain_conclusion"].as_str().unwrap()
    );
    assert!(folder_row(task).text().contains(&format!(
        "{} tok",
        show["chargeable_tokens"].as_str().unwrap()
    )));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
fn one_stores_cached_summary_never_stands_in_for_anothers() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, first) = crate::tui::tests::hub_with_tasks(&root);
    // A second Store holding the same finished results, as a copied Task state would.
    let second = root.join("second-store");
    copy_dir(&first, &second);
    let mut cache = Cache::default();
    let store = read_store(&first, "first", None, &mut cache);
    assert!(store.tasks.is_ok());
    // What the first Store's summaries say, altered, stays with the first Store.
    for summary in cache.finished.values_mut() {
        summary.chargeable = Some("424242".into());
    }
    let other = read_store(&second, "second", None, &mut cache);
    for task in other.tasks.as_ref().unwrap() {
        let summary = task.summary.as_ref().unwrap();
        assert_ne!(
            summary.chargeable.as_deref(),
            Some("424242"),
            "{}",
            task.task_id
        );
    }
    // And the second Store's reads fail closed on their own: a gap there is its refusal.
    let unfinished = document(&second, "pagination-unfinished");
    let invocation = array(&unfinished["execution_records"])
        .iter()
        .find_map(|entry| entry["record"]["invocation_id"].as_str())
        .unwrap()
        .trim_start_matches("sha256:")
        .to_owned();
    let object = second
        .join("cas/objects")
        .join(&invocation[..2])
        .join(&invocation[2..]);
    std::fs::remove_file(object).unwrap();
    cache.finished.clear();
    assert!(
        read_store(&second, "second", None, &mut cache)
            .tasks
            .is_err()
    );
    assert!(read_store(&first, "first", None, &mut cache).tasks.is_ok());
}
