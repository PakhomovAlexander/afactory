//! Grouping, fitting, progress and the stage marks, from Tasks recorded in a real Store.

use serde_json::json;

use super::*;

#[test]
fn a_task_awaiting_approval_needs_a_person_as_a_failed_one_does() {
    assert_eq!(State::Running.tone(), Tone::Active);
    assert_eq!(State::Done.tone(), Tone::Ok);
    assert_eq!(State::Awaiting.tone(), Tone::Fail);
    assert_eq!(State::Failed.tone(), Tone::Fail);
}

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
    assert_eq!(fit("t-01", "verified", "50%", 17), "t-01  veri~   50%");
    // Wider than the row needs: padded so the progress ends at the bar's edge.
    let wide = fit("t-01", "verified", "50%", 30);
    assert_eq!(
        (wide.len(), wide.ends_with("verified             50%")),
        (30, true)
    );
    assert_eq!(
        fit("pagination", "verified", "50%", 19),
        "pagination      50%"
    );
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
        "pagination~    0%"
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
    let row = stage_row(&stage(&state, &retrying, "implement"));
    // The mark is the chip; the rest of the row is plain text on the terminal's ground.
    let fail = Paint::Chip(Tone::Fail);
    let paints: Vec<Paint> = row.spans.iter().map(|span| span.paint).collect();
    assert_eq!(paints, [Paint::Plain, fail, Paint::Plain]);
    let row = row.text();
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
    listed.entry["state"] =
        json!({"phase": "running", "outcome": null, "cause": null, "next_action": null});
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
    // A settlement with unknown usage (ADR-0143) reads as unknown beside the charge, never as
    // spend; a later observation makes it known, even one that reported 0 tokens.
    let mut unknown = done.clone();
    // Replay refuses an unknown settlement after any observation of its Attempt, so the
    // unknown case records none.
    unknown["execution_records"]
        .as_array_mut()
        .unwrap()
        .retain(|entry| {
            entry["record"]["kind"] != "usage_observed"
                || entry["record"]["attempt_id"] != attempt.as_str()
        });
    for entry in unknown["execution_records"].as_array_mut().unwrap() {
        if entry["record"]["kind"] == "settled" && entry["record"]["attempt_id"] == attempt.as_str()
        {
            entry["record"]["charged_tokens"] = json!("0");
            entry["record"]["unknown_usage"] = json!({"cause": "capacity"});
        }
    }
    let implement = stage(&state, &unknown, "implement");
    assert_eq!((implement.tokens, implement.unknown), (Some(0), 1));
    let row = stage_row(&implement).text();
    assert!(row.contains("0 tok (+1 unknown)"), "{row}");
    let mut zero = unknown.clone();
    zero["execution_records"]
        .as_array_mut()
        .unwrap()
        .push(json!({"artifact_id": "sha256:ab",
        "record": {"kind": "usage_observed", "attempt_id": attempt, "charged_tokens": "0",
        "usage_id": "sha256:ee", "raw_artifact_ids": []}}));
    let implement = stage(&state, &zero, "implement");
    assert_eq!((implement.tokens, implement.unknown), (Some(0), 0));
    let row = stage_row(&implement).text();
    assert!(!row.contains("unknown"), "{row}");
    unknown["execution_records"]
        .as_array_mut()
        .unwrap()
        .push(json!({"artifact_id": "sha256:ff",
        "record": {"kind": "usage_observed", "attempt_id": attempt, "charged_tokens": "100",
        "usage_id": "sha256:ee", "raw_artifact_ids": []}}));
    let implement = stage(&state, &unknown, "implement");
    assert_eq!((implement.tokens, implement.unknown), (Some(100), 0));
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
            origin: Origin::Scope,
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

#[test]
fn a_long_task_id_leaves_room_for_the_goal() {
    let id = "a-very-long-task-identifier-with-fifty-three-characters";
    let title = task_title(
        id,
        "implement",
        "VISIBLE-GOAL-MARKER and more words after it",
    );
    assert!(title.len() <= MAIN, "{title}");
    assert!(title.contains("VISIBLE-GOAL-MA"), "{title}");
    // A short id is left whole.
    let title = task_title("pagination-cli", "implement", "fix the page size");
    assert_eq!(
        title,
        "TASK  pagination-cli  implement: \"fix the page size\""
    );
}

#[test]
fn the_latest_settlement_decides_and_a_recoverable_suppression_stays_open() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let done = document(&state, "pagination-cli");
    // A newer settlement of the check fails after its earlier success: the stage is failed,
    // and with an Attempt left it is not closed.
    let mut retried = done.clone();
    retried["phase"] = json!({"kind": "running"});
    retried["run_reports"] = json!([]);
    retried["graph"]["allowances"]["root.nodes.check"]["max_attempts"] = json!(3);
    let records = retried["execution_records"].as_array().unwrap().clone();
    let settled = records
        .iter()
        .filter(|entry| entry["record"]["kind"] == "settled")
        .find(|entry| {
            let attempt = entry["record"]["attempt_id"].as_str().unwrap();
            records.iter().any(|reserved| {
                reserved["record"]["kind"] == "reserved"
                    && reserved["record"]["attempt_id"] == attempt
                    && stage_name(
                        &node_of(
                            &state,
                            reserved["record"]["invocation_id"].as_str().unwrap(),
                            &mut Cache::default(),
                        )
                        .unwrap()
                        .node,
                    ) == "check"
            })
        })
        .unwrap()
        .clone();
    let mut later = settled.clone();
    later["record"]["result"] = json!({"kind": "failed", "diagnostic_id": "sha256:00"});
    retried["execution_records"]
        .as_array_mut()
        .unwrap()
        .push(later);
    let check = stage(&state, &retried, "check");
    assert_eq!(check.mark, Mark::Failed);
    assert!(!check.closed);
    // A node suppressed for a missing upstream, in an unfinished Task, is skipped but open;
    // a branch not selected is closed.
    let mut waiting = document(&state, "pagination-unfinished");
    waiting["phase"] = json!({"kind": "waiting", "reason": "recovery"});
    let nodes = waiting["run_reports"][0]["report"]["nodes"]
        .as_array_mut()
        .unwrap();
    for node in nodes {
        if node["node"] == "root.nodes.evaluate" {
            node["outcome"] = json!({"kind": "suppressed", "reason": "upstream_missing"});
        }
    }
    let evaluate = stage(&state, &waiting, "evaluate");
    assert_eq!(evaluate.mark, Mark::Skipped);
    assert!(!evaluate.closed, "it runs when the upstream recovers");
    let nodes = waiting["run_reports"][0]["report"]["nodes"]
        .as_array_mut()
        .unwrap();
    for node in nodes {
        if node["node"] == "root.nodes.evaluate" {
            node["outcome"] = json!({"kind": "suppressed", "reason": "branch_not_selected"});
        }
    }
    assert!(stage(&state, &waiting, "evaluate").closed);
}

#[test]
fn a_stage_named_nodes_keeps_its_name() {
    assert_eq!(stage_name("root.nodes.implement"), "implement");
    assert_eq!(
        stage_name("root.nodes.review.nodes.checks"),
        "review.checks"
    );
    assert_eq!(stage_name("root.nodes.nodes"), "nodes");
    assert_eq!(stage_name("root.nodes.review.nodes.nodes"), "review.nodes");
    assert_eq!(stage_name("root.providers.admit0"), "providers.admit0");
    assert_eq!(stage_name("root.inputs"), "inputs");
}

/// ADR-0135: a collected Task is listed by `af task list` with its tombstone, but the browser
/// has nothing left to inspect for it; the Store stays readable and shows the other Tasks.
#[test]
fn a_collected_task_leaves_the_store_readable_and_is_not_opened() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let mut store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
    let collected = store.apply_task_collection(&cas, 0, 1, false).unwrap();
    assert_eq!(collected.tombstoned, ["pagination-cli"]);
    drop(store);

    let listed = task_execution::list_common(&state).unwrap();
    assert_eq!(listed.len(), 2, "the collected Task is still listed");
    let mut cache = Cache::default();
    let read = read_store(&state, "state", None, &mut cache);
    let tasks = read.tasks.unwrap();
    let ids: Vec<&str> = tasks.iter().map(|task| task.task_id.as_str()).collect();
    assert_eq!(ids, ["pagination-unfinished"]);
}

#[test]
fn r_prefills_the_run_of_the_shown_plan_and_d_only_a_verified_tasks_delivery() {
    let run = prefill(
        Key::Char('r'),
        "t-1",
        Some("sha256:ab"),
        State::Failed,
        None,
    );
    assert_eq!(
        run,
        Some(Ok("task run t-1 --confirm-plan sha256:ab".to_owned()))
    );
    let run = prefill(Key::Char('r'), "t-1", None, State::Done, None);
    assert_eq!(
        run,
        Some(Err("Task t-1 records no plan to confirm".to_owned()))
    );
    // The confirmation is left to type: the line ends at `--confirm `.
    let deliver = prefill(Key::Char('D'), "t-1", Some("sha256:ab"), State::Done, None);
    let expected = "task deliver t-1 --branch af/t-1 --worktree ../t-1 --confirm ";
    assert_eq!(deliver, Some(Ok(expected.to_owned())));
    for (state, why) in [
        (State::Running, "is still running"),
        (State::Awaiting, "is awaiting approval"),
        (State::Failed, "finished without satisfying its acceptance"),
    ] {
        let refused = prefill(Key::Char('D'), "t-1", Some("sha256:ab"), state, None);
        let expected = format!("only a verified Task can be delivered: t-1 {why}");
        assert_eq!(refused, Some(Err(expected)));
    }
    // A Task id the shell would split is quoted, so the line parses back to the same words.
    let quoted = prefill(Key::Char('D'), "a b", None, State::Done, None)
        .unwrap()
        .unwrap();
    let words = shell_words::split(&quoted).unwrap();
    assert_eq!(words[2], "a b");
    assert_eq!(words[4], "af/a b");
    assert_eq!(
        prefill(Key::Char('x'), "t-1", None, State::Done, None),
        None
    );
    // A Store a command reaches only by naming it is named after the Task id.
    let store = Path::new("/elsewhere/my state");
    let run = prefill(
        Key::Char('r'),
        "t-1",
        Some("sha256:ab"),
        State::Failed,
        Some(store),
    );
    let expected = "task run t-1 --state '/elsewhere/my state' --confirm-plan sha256:ab";
    assert_eq!(run, Some(Ok(expected.to_owned())));
    let deliver = prefill(Key::Char('D'), "t-1", None, State::Done, Some(store));
    let words = shell_words::split(&deliver.unwrap().unwrap()).unwrap();
    assert_eq!(
        words[..5],
        ["task", "deliver", "t-1", "--state", "/elsewhere/my state"]
    );
}

#[test]
fn stores_an_earlier_release_wrote_are_listed_once_and_other_refusals_each() {
    let old = format!(
        "event store: unknown review-kernel event type: TaskTransition@1; {}; start a new \
         Campaign or Task",
        review_core::event::ANOTHER_RELEASE
    );
    let store = |name: &str, tasks: Result<Vec<Listed>, String>| Store {
        dir: PathBuf::from(format!("/home/me/.local/state/af/task/local/{name}")),
        shown: format!("~/.local/state/af/task/local/{name}"),
        repo: Some(name.to_owned()),
        origin: Origin::Repository,
        release:
            matches!(&tasks, Err(error) if error.contains(review_core::event::ANOTHER_RELEASE))
                .then(|| {
                    review_core::store_writer::StoreProvenance::OtherReleases(vec!["0.11.0".into()])
                }),
        tasks,
    };
    let mut pane = TasksPane {
        title: "TASKS  user".to_owned(),
        stores: vec![
            store("0000000000000001", Err(old.clone())),
            store("0000000000000002", Err("permission denied".to_owned())),
            store("0000000000000003", Err(old.clone())),
            store("0000000000000004", Ok(Vec::new())),
        ],
        ..TasksPane::default()
    };
    // The bar: one entry for both old Stores, after the others; each other Store as before.
    let labels: Vec<String> = pane.items().into_iter().map(|item| item.label).collect();
    assert_eq!(
        labels,
        [
            "0000000000000002/ (0)",
            "0000000000000004/ (0)",
            "! 2 old Stores",
        ]
    );
    // The folder: one line for them, and the other refusal still named with its cause.
    pane.rebuild();
    let rows: Vec<String> = pane.rows.iter().map(Row::text).collect();
    assert_eq!(
        rows[2..6],
        [
            "2 Stores written by other releases: af 0.11.0 (2); run them with",
            "AF_VERSION=0.11.0 af",
            "They are Task Stores in",
            "  ~/.local/state/af/task/local",
        ]
    );
    assert!(rows[2..6].iter().all(|row| row.len() <= MAIN), "{rows:#?}");
    let at = rows
        .iter()
        .position(|row| row == "~/.local/state/af/task/local/0000000000000002:")
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert_eq!(
        rows[at + 1],
        " error  this Store cannot be read: permission denied"
    );
    assert!(
        !rows.iter().any(|row| row.contains("TaskTransition")),
        "{rows:#?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("written by other releases"))
            .count(),
        1
    );
}

#[test]
fn a_long_state_path_keeps_every_row_within_the_pane() {
    let root = format!(
        "/private/var/folders/{}/state/af/task/local",
        "x".repeat(60)
    );
    let store = |name: &str, tasks: Result<Vec<Listed>, String>| Store {
        dir: PathBuf::from(format!("{root}/{name}")),
        shown: format!("{root}/{name}"),
        repo: Some(name.to_owned()),
        origin: Origin::Repository,
        release:
            matches!(&tasks, Err(error) if error.contains(review_core::event::ANOTHER_RELEASE))
                .then(|| {
                    review_core::store_writer::StoreProvenance::OtherReleases(vec!["0.11.0".into()])
                }),
        tasks,
    };
    let old = format!(
        "unknown event type; {}",
        review_core::event::ANOTHER_RELEASE
    );
    let mut pane = TasksPane {
        title: "TASKS  user".to_owned(),
        stores: vec![
            store("0000000000000001", Err(old)),
            store("0000000000000002", Err("file is not a database".to_owned())),
        ],
        ..TasksPane::default()
    };
    pane.rebuild();
    let rows: Vec<String> = pane.rows.iter().map(Row::text).collect();
    assert!(
        rows.iter().all(|row| row.chars().count() <= MAIN),
        "{rows:#?}"
    );
    // The whole location is there, across rows, and the other refusal's cause is on its own.
    let location: String = rows[3..].iter().map(|row| row.trim()).collect();
    assert!(location.contains("state/af/task/local"), "{rows:#?}");
    assert!(
        rows.iter()
            .any(|row| row == " error  this Store cannot be read: file is not a database"),
        "{rows:#?}"
    );
}

#[test]
fn bounded_rows_split_at_spaces_and_inside_long_words() {
    assert_eq!(bounded("short", 10, "  "), ["  short"]);
    assert_eq!(bounded("one two three", 9, ""), ["one two", "three"]);
    assert_eq!(bounded("abcdefghij", 4, ""), ["abcd", "efgh", "ij"]);
    assert_eq!(bounded("ab cdefgh", 6, "- "), ["- ab", "- cdef", "- gh"]);
}

/// Only the event log's own refusal marks a Store as another release's: a Store refused before
/// it is read, whose path happens to hold the same words, keeps its own cause.
#[test]
fn a_path_holding_the_marker_is_not_an_old_store() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join(review_core::event::ANOTHER_RELEASE);
    std::os::unix::fs::symlink(temp.path().join("gone"), &dir).unwrap();
    let store = read_store(
        &dir,
        "shown",
        Some("repo".to_owned()),
        &mut Cache::default(),
    );
    let Err(why) = &store.tasks else {
        panic!("a link to nothing is refused");
    };
    assert!(why.contains("link to nothing"), "{why}");
    assert!(!earlier(&store));
}

/// A Task Store this release cannot replay: its log holds an event type no release of today
/// writes, and its writer marker names `release`, or none when `None` (written before markers).
fn foreign_store(dir: &Path, release: Option<&str>) {
    std::fs::create_dir_all(dir.join("cas/objects")).unwrap();
    let path = dir.join("events.sqlite");
    drop(review_store::EventStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DELETE FROM store_writers;
         INSERT INTO events (run_id, sequence, event_id, type, occurred_at, artifact_refs,
                             payload)
         VALUES ('task:t-1', 0, 'e', 'TaskTransition@9', '1970-01-01T00:00:00Z', '[]', '{}');",
    )
    .unwrap();
    if let Some(release) = release {
        conn.execute(
            "INSERT INTO store_writers (af_version, record) VALUES (?1, '{}')",
            [release],
        )
        .unwrap();
    }
}

/// Stores another release wrote, read from disk as the browser reads them: each is named by
/// the release its marker names, or as an older one without a marker, and the pane lists them
/// in one row with the command that runs them.
#[test]
fn stores_other_releases_wrote_are_named_by_their_writer_marker() {
    let temp = tempfile::tempdir().unwrap();
    let newer = temp.path().join("1111111111111111");
    let unmarked = temp.path().join("2222222222222222");
    foreign_store(&newer, Some("99.1.0"));
    foreign_store(&unmarked, None);
    let mut cache = Cache::default();
    let read = |dir: &Path, cache: &mut Cache| read_store(dir, "shown/x", None, cache);
    let newer_store = read(&newer, &mut cache);
    assert_eq!(
        newer_store.release,
        Some(review_core::store_writer::StoreProvenance::OtherReleases(
            vec!["99.1.0".into()]
        ))
    );
    let unmarked_store = read(&unmarked, &mut cache);
    assert_eq!(
        unmarked_store.release,
        Some(review_core::store_writer::StoreProvenance::Unmarked)
    );
    let mut pane = TasksPane {
        title: "TASKS  user".to_owned(),
        stores: vec![newer_store, unmarked_store],
        ..TasksPane::default()
    };
    pane.rebuild();
    let rows: Vec<String> = pane.rows.iter().map(Row::text).collect();
    let joined = rows
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains(
            "2 Stores written by other releases; run each under the release that wrote it: \
             AF_VERSION=99.1.0 af (1 Store) 1 Store by an older af release with no version \
             marker: finish it under its own release"
        ),
        "{rows:#?}"
    );
    assert!(!joined.contains("TaskTransition@9"), "{rows:#?}");
    let labels: Vec<String> = pane.items().into_iter().map(|item| item.label).collect();
    assert_eq!(labels, ["! 2 old Stores"]);
}

/// Two registered Task Stores that cannot be resolved (here below a directory this process may
/// not enter) are two Stores, not one: each is listed, each with its own refusal, and so is the
/// scope's own Store, absent.
#[test]
fn unavailable_registered_stores_are_each_their_own_row() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let repo = crate::tui::tests::hub_repo(&root);
    let scope = crate::tui::tests::hub_scope(&root, &repo);
    let locked = root.join("locked");
    let unavailable = [locked.join("one"), locked.join("two")];
    for path in &unavailable {
        std::fs::create_dir_all(path).unwrap();
    }
    let registry = crate::storage::registry::location(&root.join("state"));
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    let mut text = String::from("version = 2\n");
    for path in &unavailable {
        text.push_str(&format!(
            "\n[[store]]\npath = {:?}\nkind = \"task\"\nfirst_use_unix_ms = 1\nlast_use_unix_ms = 1\n",
            path.display().to_string()
        ));
    }
    std::fs::write(&registry, text).unwrap();
    let own = targets(&scope).unwrap();
    assert_eq!(own.len(), 1);
    assert!(!own[0].dir.exists(), "the scope's own Store is absent");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (explicit, warning) = explicit_targets(&scope, &[], &own);
    let mut cache = Cache::default();
    let refusals: Vec<bool> = explicit
        .iter()
        .map(|target| read_target(target, &mut cache).tasks.is_err())
        .collect();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(warning, None);
    let dirs: Vec<&PathBuf> = explicit.iter().map(|target| &target.dir).collect();
    assert_eq!(dirs, [&unavailable[0], &unavailable[1]]);
    if !nix::unistd::geteuid().is_root() {
        assert_eq!(refusals, [true, true], "each its own refusal");
    }
    // Missing paths are two Stores too, and two spellings of one directory are one.
    assert!(!same_store(&root.join("gone/one"), &root.join("gone/two")));
    assert!(!same_store(&root.join("gone/one"), &own[0].dir));
    assert!(same_store(&root.join("gone/one"), &root.join("gone/one")));
    assert!(same_store(&locked, &root.join("locked/../locked")));
}

/// A load starts the read of every Store on a thread and lists nothing until the event loop
/// collects it: the bar shows the folder with a spinner meanwhile, so the browser's first frame
/// never waits on a Store (ADR-0240).
#[test]
fn a_load_lists_its_stores_behind_a_spinner_never_on_the_callers_thread() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (repo, _state) = crate::tui::tests::hub_with_tasks(&root);
    let scope = crate::tui::tests::hub_scope(&root, &repo);
    let mut pane = TasksPane::default();
    pane.load(&scope).unwrap();
    assert!(pane.busy().is_some(), "the read runs behind the folder");
    assert!(
        pane.items().is_empty(),
        "nothing is listed before the read is collected"
    );
    pane.settle();
    assert!(pane.busy().is_none());
    let labels: Vec<String> = pane.items().into_iter().map(|item| item.label).collect();
    assert!(
        labels.iter().any(|label| label.starts_with("done/")),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|label| label.starts_with("failed/")),
        "{labels:?}"
    );
}

/// Stores are listed one by one as each is read, in target order, while the rest are still
/// read: the folder fills, and the spinner stays until the last Store.
#[test]
fn stores_are_listed_one_by_one_as_each_is_read() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    // Two targets, each its own directory: the second reads the same Store through a link.
    let two = root.join("two");
    std::os::unix::fs::symlink(&state, &two).unwrap();
    let target = |repo: &str| Target {
        dir: if repo == "two" {
            two.clone()
        } else {
            state.clone()
        },
        shown: repo.to_owned(),
        repo: Some(repo.to_owned()),
        origin: Origin::Registered,
    };
    let mut pane = TasksPane {
        targets: vec![target("one"), target("two")],
        ..TasksPane::default()
    };
    let sender = pane.fed();
    let mut cache = Cache::default();
    let store = |repo: &str, cache: &mut Cache| read_target(&target(repo), cache);
    let groups = |pane: &TasksPane| -> Vec<String> {
        pane.items()
            .into_iter()
            .filter(|item| item.children.is_some())
            .map(|item| item.label)
            .collect()
    };
    sender
        .send(Read::Store {
            store: Box::new(store("one", &mut cache)),
            detail: None,
        })
        .unwrap();
    assert!(pane.poll(), "the first Store changed the rows");
    assert_eq!(
        groups(&pane),
        ["one/ (2)"],
        "the second Store is not read yet"
    );
    assert!(pane.busy().is_some(), "the read goes on");
    assert!(!pane.poll(), "nothing more arrived");
    sender
        .send(Read::Store {
            store: Box::new(store("two", &mut cache)),
            detail: None,
        })
        .unwrap();
    assert!(pane.poll());
    assert_eq!(groups(&pane), ["one/ (2)", "two/ (2)"]);
    assert!(
        pane.busy().is_some(),
        "every Store listed, the read is not over until its last message"
    );
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert!(pane.busy().is_none(), "the last message ends the read");
    assert!(!pane.cache.finished.is_empty(), "the read's cache is kept");
    // A second read replaces each Store in its place, the first before the second, and the
    // second stays listed while its own read is still to come.
    let sender = pane.fed();
    sender
        .send(Read::Store {
            store: Box::new(store("one", &mut cache)),
            detail: None,
        })
        .unwrap();
    assert!(pane.poll());
    assert_eq!(groups(&pane), ["one/ (2)", "two/ (2)"]);
    assert!(pane.busy().is_some());
    // The reader gone (a load dropped it): the read ends with what it listed.
    drop(sender);
    assert!(!pane.poll());
    assert!(pane.busy().is_none());
}

/// The opened Task's detail travels with its Store's message, so the bar row and the main
/// pane come from one read; a detail read for a Task opened earlier is dropped, its Store
/// still listed.
#[test]
fn the_opened_tasks_detail_comes_with_its_stores_message() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let target = Target {
        dir: state.clone(),
        shown: "state".to_owned(),
        repo: None,
        origin: Origin::Scope,
    };
    let mut pane = TasksPane {
        loaded: vec![target.clone()],
        targets: vec![target.clone()],
        ..TasksPane::default()
    };
    let mut cache = Cache::default();
    let message = |cache: &mut Cache| Read::Store {
        store: Box::new(read_target(&target, cache)),
        detail: Some(Detail::read(&state, "pagination-cli", cache)),
    };
    // Read for the Task opened now: listed, and shown.
    pane.selected = Some("pagination-cli".to_owned());
    pane.opened = true;
    let sender = pane.fed();
    sender.send(message(&mut cache)).unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert!(matches!(&pane.detail, Some(Ok(detail)) if detail.task_id == "pagination-cli"));
    assert!(pane.task("pagination-cli").is_some());
    // Read for a Task that was opened when the read started, then left: its Store is listed,
    // its detail is not shown.
    pane.detail = None;
    let sender = pane.fed();
    pane.selected = None;
    sender.send(message(&mut cache)).unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert!(
        pane.detail.is_none(),
        "a detail for another Task is dropped"
    );
    assert!(
        pane.task("pagination-cli").is_some(),
        "its Store is listed all the same"
    );
}

/// A refused Store is listed with its refusal the moment it is read, while the Stores after it
/// are still unread.
#[test]
fn a_refused_store_is_listed_before_the_stores_after_it() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    // Entries but no event log: not a Store this release reads.
    let broken = root.join("broken");
    std::fs::create_dir_all(broken.join("cas")).unwrap();
    let target = |dir: &Path, shown: &str| Target {
        dir: dir.to_path_buf(),
        shown: shown.to_owned(),
        repo: Some(shown.to_owned()),
        origin: Origin::Registered,
    };
    let mut pane = TasksPane {
        targets: vec![target(&broken, "broken"), target(&state, "good")],
        ..TasksPane::default()
    };
    let sender = pane.fed();
    let mut cache = Cache::default();
    let labels = |pane: &TasksPane| -> Vec<String> {
        let mut labels = Vec::new();
        for item in pane.items() {
            labels.push(item.label.clone());
            for child in item.children.into_iter().flatten() {
                labels.push(child.label);
            }
        }
        labels
    };
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target(&broken, "broken"), &mut cache)),
            detail: None,
        })
        .unwrap();
    assert!(pane.poll());
    let listed = labels(&pane);
    assert!(
        listed.iter().any(|label| label == "! Store unreadable"),
        "{listed:?}"
    );
    assert!(
        !listed.iter().any(|label| label.starts_with("good/")),
        "the Store after it is still unread: {listed:?}"
    );
    assert!(pane.busy().is_some());
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target(&state, "good"), &mut cache)),
            detail: None,
        })
        .unwrap();
    sender.send(Read::Done(cache)).unwrap();
    assert!(pane.poll());
    let listed = labels(&pane);
    assert!(listed.iter().any(|label| label == "! Store unreadable"));
    assert!(
        listed.iter().any(|label| label == "good/ (2)"),
        "{listed:?}"
    );
    assert!(pane.busy().is_none());
}

/// A Task whose Store refuses when it is opened: a read in flight, which may hold that Store
/// as it stood before the refusal, is dropped and started over, so nothing read before the
/// refusal lists the Task again over it.
#[test]
fn a_refusal_taken_on_open_restarts_a_read_in_flight() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let target = Target {
        dir: state.clone(),
        shown: "state".to_owned(),
        repo: None,
        origin: Origin::Scope,
    };
    let mut pane = TasksPane {
        loaded: vec![target.clone()],
        targets: vec![target.clone()],
        ..TasksPane::default()
    };
    // Listed once, so the Task can be opened.
    let mut cache = Cache::default();
    let sender = pane.fed();
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target, &mut cache)),
            detail: None,
        })
        .unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert!(pane.task("pagination-cli").is_some());
    // A read in flight has the Store as it stands now, readable, queued but not collected.
    let sender = pane.fed();
    let before = read_target(&target, &mut cache);
    assert!(before.tasks.is_ok());
    sender
        .send(Read::Store {
            store: Box::new(before),
            detail: None,
        })
        .unwrap();
    // The finished Task's revision goes; opening it refuses the Store.
    let done = document(&state, "pagination-cli");
    let revision = done["revision_id"]
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
    pane.open(Some("pagination-cli"));
    assert!(
        pane.selected.is_none(),
        "the refused Store opens as the folder"
    );
    assert!(pane.stores[0].tasks.is_err(), "the Store is refused");
    // The queued message is never collected: its read was dropped and a new one started.
    assert!(
        sender.send(Read::Done(cache.clone())).is_err(),
        "the read in flight was dropped"
    );
    assert!(pane.busy().is_some(), "a read started over");
    // Listed readable by hand meanwhile: what the restarted read delivers for the Store is
    // what stands once it is collected, and it was read after the refusal.
    pane.stores[0].tasks = Ok(Vec::new());
    pane.settle();
    assert!(
        pane.stores[0].tasks.is_err(),
        "read after the refusal, the Store refuses again: {:?}",
        pane.stores[0].tasks.as_ref().err()
    );
    assert!(pane.task("pagination-cli").is_none());
}

/// The user scope's repositories' Stores are a walk of the state root: the read discovers them
/// on its thread, so a load resolves no target of theirs and the browser opens before the walk
/// (ADR-0240). The targets, and then the Stores, are what the read answers.
#[test]
fn the_user_scopes_stores_are_discovered_by_the_read_never_by_the_load() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let scope = crate::tui::tests::user_scope(&root);
    let mut pane = TasksPane::default();
    pane.load(&scope).unwrap();
    assert!(
        pane.targets.is_empty() && pane.stores.is_empty() && pane.error.is_none(),
        "the load resolved {} targets and listed {} Stores",
        pane.targets.len(),
        pane.stores.len()
    );
    assert!(
        pane.busy().is_some(),
        "the read discovers the repositories' Stores"
    );
    pane.settle();
    let dirs: Vec<&Path> = pane
        .targets
        .iter()
        .map(|target| target.dir.as_path())
        .collect();
    assert_eq!(dirs, [state.as_path()]);
    assert_eq!(pane.targets[0].origin, Origin::Repository);
    assert_eq!(pane.stores.len(), 1);
    let listed = pane.stores[0].tasks.as_ref().unwrap();
    assert!(listed.iter().any(|task| task.task_id == "pagination-cli"));
}

/// A repository's Store the user scope discovered and that went between two reads: the read
/// after answers without it, so it goes when that read is done, never to come back as a
/// target of the load's own, while the Store that stays is listed as before.
#[test]
fn a_discovered_store_that_went_between_two_reads_goes_when_the_read_is_done() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let other = state.parent().unwrap().join("other");
    crate::tui::tests::copy_store(&state, &other);
    let scope = crate::tui::tests::user_scope(&root);
    let mut pane = TasksPane::default();
    pane.load(&scope).unwrap();
    pane.settle();
    let mut dirs: Vec<&Path> = pane
        .targets
        .iter()
        .map(|target| target.dir.as_path())
        .collect();
    dirs.sort();
    let mut both = [state.as_path(), other.as_path()];
    both.sort();
    assert_eq!(dirs, both);
    assert_eq!(pane.stores.len(), 2);
    assert!(
        pane.loaded.is_empty(),
        "nothing registered: the load resolves no target"
    );
    std::fs::remove_dir_all(&other).unwrap();
    pane.read_now();
    pane.settle();
    let dirs: Vec<&Path> = pane
        .targets
        .iter()
        .map(|target| target.dir.as_path())
        .collect();
    assert_eq!(
        dirs,
        [state.as_path()],
        "the read answers without the Store that went"
    );
    assert_eq!(pane.stores.len(), 1);
    assert_eq!(pane.stores[0].dir, state);
    assert!(pane.stores[0].tasks.is_ok());
    assert!(
        pane.loaded.is_empty(),
        "a discovered Store is never a target of the load's"
    );
}

/// The user scope's walk of the state root fails after a read that discovered a Store (the
/// directory of the repositories' Stores is no directory any more): the read answers the
/// load's targets alone, so the Store discovered before goes when the read is done, with its
/// opened Task, and the folder shows why.
#[test]
fn a_failed_discovery_unlists_the_stores_it_cannot_answer_for_when_the_read_is_done() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let scope = crate::tui::tests::user_scope(&root);
    let mut pane = TasksPane::default();
    pane.load(&scope).unwrap();
    pane.settle();
    assert_eq!(pane.stores.len(), 1);
    let id = TasksPane::id_of(&pane.stores[0].repo, "pagination-cli");
    pane.open(Some(&id));
    assert_eq!(pane.selected.as_deref(), Some(id.as_str()));
    assert!(pane.detail.as_ref().is_some_and(Result::is_ok));
    let local = state.parent().unwrap().to_path_buf();
    std::fs::remove_dir_all(&local).unwrap();
    std::fs::write(&local, "").unwrap();
    pane.read_now();
    assert!(pane.busy().is_some());
    pane.settle();
    assert!(pane.error.is_some(), "the folder says why the walk failed");
    assert!(
        pane.targets.is_empty(),
        "the read answered the load's targets alone"
    );
    assert!(
        pane.stores.is_empty(),
        "the Store discovered before went with the read"
    );
    assert!(pane.selected.is_none() && pane.detail.is_none());
    assert!(pane.loaded.is_empty());
}

/// A Task opened while an explicit read is in flight, started before for no Task: the read
/// answers its Store without a detail for it, and the pane reads the detail then, so the
/// detail the Task was opened with never stands over the Store's newer answer. Here that
/// detail can no longer be read: the Store refuses, as it would on the reader's thread.
#[test]
fn a_task_opened_during_a_read_has_its_detail_read_again_when_its_store_answers() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let target = Target {
        dir: state.clone(),
        shown: "state".to_owned(),
        repo: None,
        origin: Origin::Scope,
    };
    let mut pane = TasksPane {
        loaded: vec![target.clone()],
        targets: vec![target.clone()],
        ..TasksPane::default()
    };
    let mut cache = Cache::default();
    let sender = pane.fed();
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target, &mut cache)),
            detail: None,
        })
        .unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    // An explicit read in flight, started for no Task; the Task is opened meanwhile, and the
    // read goes on (a load's read lists every Store).
    let sender = pane.fed();
    pane.open(Some("pagination-cli"));
    assert!(pane.detail.as_ref().is_some_and(Result::is_ok));
    assert!(pane.busy().is_some(), "the explicit read goes on");
    // The read answers the Store as it stood, readable, with no detail for the Task; the
    // Task's revision goes before that answer is collected.
    let store = read_target(&target, &mut cache);
    assert!(store.tasks.is_ok());
    let done = document(&state, "pagination-cli");
    let revision = done["revision_id"]
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
    sender
        .send(Read::Store {
            store: Box::new(store),
            detail: None,
        })
        .unwrap();
    assert!(pane.poll());
    assert!(
        pane.stores[0].tasks.is_err(),
        "the detail read when the Store answered refuses it: {:?}",
        pane.stores[0].tasks.as_ref().err()
    );
    assert!(pane.selected.is_none() && pane.detail.is_none());
}

/// A load records the read and starts none: the first poll after it, which the event loop
/// reaches after the frame that follows the load, starts the read on its thread. No Store is
/// read before the browser's first frame, on any thread.
#[test]
fn a_load_starts_no_read_the_first_poll_after_it_does() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (repo, _state) = crate::tui::tests::hub_with_tasks(&root);
    let scope = crate::tui::tests::hub_scope(&root, &repo);
    let mut pane = TasksPane::default();
    pane.load(&scope).unwrap();
    assert_eq!(pane.started(), 0, "the load started no read");
    assert!(pane.busy().is_some(), "the folder shows the read to come");
    assert!(pane.stores.is_empty());
    pane.poll();
    assert_eq!(pane.started(), 1, "the first poll starts the read");
    pane.settle();
    assert!(pane.task("pagination-cli").is_some());
}

/// A read's detail is taken for the opening it was started under: the Task opened again
/// meanwhile (the folder between, or `R`'s reopening) was inspected afresh, and a detail read
/// under the earlier opening never stands over that inspection; the Task is inspected again
/// when its Store answers instead.
#[test]
fn a_reads_detail_from_an_earlier_opening_never_stands_over_a_newer_inspection() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let target = Target {
        dir: state.clone(),
        shown: "state".to_owned(),
        repo: None,
        origin: Origin::Scope,
    };
    let mut pane = TasksPane {
        loaded: vec![target.clone()],
        targets: vec![target.clone()],
        ..TasksPane::default()
    };
    let mut cache = Cache::default();
    let sender = pane.fed();
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target, &mut cache)),
            detail: None,
        })
        .unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    pane.open(Some("pagination-cli"));
    let goal = pane.detail.as_ref().unwrap().as_ref().unwrap().goal.clone();
    // An explicit read started under this opening: its detail, marked here, is the one shown.
    let sender = pane.fed();
    let mut read = Detail::read(&state, "pagination-cli", &mut cache).unwrap();
    read.goal = "read under this opening".to_owned();
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target, &mut cache)),
            detail: Some(Ok(read)),
        })
        .unwrap();
    assert!(pane.poll());
    let shown = &pane.detail.as_ref().unwrap().as_ref().unwrap().goal;
    assert_eq!(shown, "read under this opening");
    // Another explicit read started under this opening; the Task is opened again before its
    // message is collected: the message's detail is from an earlier opening now, and the
    // Task's own inspection stands.
    let sender = pane.fed();
    let mut read = Detail::read(&state, "pagination-cli", &mut cache).unwrap();
    read.goal = "read under an earlier opening".to_owned();
    pane.open(None);
    pane.open(Some("pagination-cli"));
    assert_eq!(pane.detail.as_ref().unwrap().as_ref().unwrap().goal, goal);
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target, &mut cache)),
            detail: Some(Ok(read)),
        })
        .unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert_eq!(pane.selected.as_deref(), Some("pagination-cli"));
    let shown = &pane.detail.as_ref().unwrap().as_ref().unwrap().goal;
    assert_eq!(shown, &goal, "the newer inspection stands");
}

/// The user scope's read answers the targets first, the discovered Stores among them: the
/// Stores retained from the last read follow that order the moment it arrives, before the
/// read answers for any Store. A reload lists the registered Store first, since the load
/// resolves no discovered target; the discovered Store returns to its place with the targets.
#[test]
fn a_targets_message_reorders_the_retained_stores_before_any_store_answers() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let registered = root.join("elsewhere/registered");
    crate::tui::tests::copy_store(&state, &registered);
    let discovered = Target {
        dir: state.clone(),
        shown: "repository".to_owned(),
        repo: Some("repository".to_owned()),
        origin: Origin::Repository,
    };
    let explicit = Target {
        dir: registered.clone(),
        shown: "~/elsewhere/registered".to_owned(),
        repo: Some("~/elsewhere/registered".to_owned()),
        origin: Origin::Registered,
    };
    let mut pane = TasksPane {
        loaded: vec![explicit.clone()],
        targets: vec![discovered.clone(), explicit.clone()],
        ..TasksPane::default()
    };
    let mut cache = Cache::default();
    let dirs = |pane: &TasksPane| -> Vec<PathBuf> {
        pane.stores.iter().map(|store| store.dir.clone()).collect()
    };
    let sender = pane.fed();
    for target in [&discovered, &explicit] {
        sender
            .send(Read::Store {
                store: Box::new(read_target(target, &mut cache)),
                detail: None,
            })
            .unwrap();
    }
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert_eq!(dirs(&pane), [state.clone(), registered.clone()]);
    // A reload: the load resolves the registered target alone, and the retained Stores
    // follow; the read then answers the targets, the discovered Store first again.
    pane.targets = pane.loaded.clone();
    pane.order();
    assert_eq!(dirs(&pane), [registered.clone(), state.clone()]);
    let sender = pane.fed();
    sender
        .send(Read::Targets(Ok(vec![
            discovered.clone(),
            explicit.clone(),
        ])))
        .unwrap();
    assert!(pane.poll());
    assert!(pane.busy().is_some(), "no Store answered yet");
    assert_eq!(
        dirs(&pane),
        [state.clone(), registered.clone()],
        "the retained Stores follow the targets the read answered"
    );
    for target in [&discovered, &explicit] {
        sender
            .send(Read::Store {
                store: Box::new(read_target(target, &mut cache)),
                detail: None,
            })
            .unwrap();
    }
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert_eq!(dirs(&pane), [state, registered]);
}

/// A target list that changed within the scope (a Store registered meanwhile, `:state DIR`)
/// keeps the Stores read so far listed, in the order of the targets as they stand, each until
/// its own read answers, and a Store of a target no longer named until the read is done: an
/// opened Task is never reported gone because its Store is not read yet.
#[test]
fn a_changed_target_list_keeps_a_store_listed_until_its_read_answers() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let own = Target {
        dir: state.clone(),
        shown: "state".to_owned(),
        repo: None,
        origin: Origin::Scope,
    };
    let other = Target {
        dir: root.join("elsewhere"),
        shown: "elsewhere".to_owned(),
        repo: Some("elsewhere".to_owned()),
        origin: Origin::Registered,
    };
    let mut pane = TasksPane {
        targets: vec![own.clone()],
        ..TasksPane::default()
    };
    let mut cache = Cache::default();
    let message = |target: &Target, cache: &mut Cache| Read::Store {
        store: Box::new(read_target(target, cache)),
        detail: None,
    };
    let sender = pane.fed();
    sender.send(message(&own, &mut cache)).unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    pane.open(Some("pagination-cli"));
    assert_eq!(pane.selected.as_deref(), Some("pagination-cli"));
    // Another Store comes first now, and its read answers before the own Store's.
    pane.targets = vec![other.clone(), own.clone()];
    let sender = pane.fed();
    sender.send(message(&other, &mut cache)).unwrap();
    assert!(pane.poll());
    let dirs: Vec<&Path> = pane
        .stores
        .iter()
        .map(|store| store.dir.as_path())
        .collect();
    assert_eq!(dirs, [other.dir.as_path(), own.dir.as_path()]);
    assert_eq!(
        pane.selected.as_deref(),
        Some("pagination-cli"),
        "opened still, its Store unread"
    );
    assert!(pane.busy().is_some());
    sender.send(message(&own, &mut cache)).unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert_eq!(pane.selected.as_deref(), Some("pagination-cli"));
    assert_eq!(pane.stores.len(), 2);
    // The two targets in the other order: the listed Stores follow, the opened Task stays.
    pane.targets = vec![own.clone(), other.clone()];
    let sender = pane.fed();
    sender.send(message(&own, &mut cache)).unwrap();
    sender.send(message(&other, &mut cache)).unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    let dirs: Vec<&Path> = pane
        .stores
        .iter()
        .map(|store| store.dir.as_path())
        .collect();
    assert_eq!(dirs, [own.dir.as_path(), other.dir.as_path()]);
    assert_eq!(pane.selected.as_deref(), Some("pagination-cli"));
    // A target no longer named keeps its Store until the read is done, then it goes.
    pane.targets = vec![other.clone()];
    let sender = pane.fed();
    sender.send(message(&other, &mut cache)).unwrap();
    assert!(pane.poll());
    assert!(
        pane.task("pagination-cli").is_some(),
        "its Store is still listed"
    );
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    assert!(pane.task("pagination-cli").is_none());
    assert!(pane.selected.is_none(), "the folder shows");
    assert_eq!(pane.stores.len(), 1);
}

/// A HISTORY artifact the Store cannot give back refuses the Store on the key loop, as opening
/// a Task that cannot be inspected does, and a read in flight starts over the same way: a Store
/// message read before the refusal never lists the Task again over it.
#[test]
fn a_refusal_taken_on_a_history_row_restarts_a_read_in_flight() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (_repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let target = Target {
        dir: state.clone(),
        shown: "state".to_owned(),
        repo: None,
        origin: Origin::Scope,
    };
    let mut pane = TasksPane {
        loaded: vec![target.clone()],
        targets: vec![target.clone()],
        ..TasksPane::default()
    };
    let mut cache = Cache::default();
    let sender = pane.fed();
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target, &mut cache)),
            detail: None,
        })
        .unwrap();
    sender.send(Read::Done(cache.clone())).unwrap();
    assert!(pane.poll());
    pane.open(Some("pagination-cli"));
    assert!(pane.detail.as_ref().is_some_and(Result::is_ok));
    // The artifact of the first HISTORY row goes before Enter reads it.
    let show = task_execution::inspection_document(&state, "pagination-cli", false)
        .unwrap()
        .unwrap();
    let first = change_artifact(&show["history"][0]["transition"]["change"])
        .unwrap()
        .trim_start_matches("sha256:")
        .to_owned();
    let row = pane
        .history
        .iter()
        .find(|(_, id)| id.ends_with(&first))
        .map(|(row, _)| *row)
        .expect("the HISTORY row of the first change");
    std::fs::remove_file(
        state
            .join("cas/objects")
            .join(&first[..2])
            .join(&first[2..]),
    )
    .unwrap();
    // An explicit reread is in flight, with the Store as it stood, readable, queued.
    let sender = pane.fed();
    sender
        .send(Read::Store {
            store: Box::new(read_target(&target, &mut cache)),
            detail: None,
        })
        .unwrap();
    assert!(
        pane.key(Key::Enter, row).is_err(),
        "the artifact cannot be given back"
    );
    assert!(
        pane.selected.is_none() && pane.stores[0].tasks.is_err(),
        "the Store is refused"
    );
    // The queued message is never collected: its read was dropped and a new one started.
    assert!(
        sender.send(Read::Done(cache.clone())).is_err(),
        "the read in flight was dropped"
    );
    assert!(pane.busy().is_some(), "a read started over");
    // Listed readable by hand meanwhile: what the restarted read delivers for the Store is
    // what stands once it is collected, and it was read after the refusal.
    pane.stores[0].tasks = Ok(Vec::new());
    pane.settle();
    assert!(pane.selected.is_none());
    assert!(
        pane.stores[0].tasks.is_err(),
        "read after the refusal, the Store refuses again: {:?}",
        pane.stores[0].tasks.as_ref().err()
    );
}
