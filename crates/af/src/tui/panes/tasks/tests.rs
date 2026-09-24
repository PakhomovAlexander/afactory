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
