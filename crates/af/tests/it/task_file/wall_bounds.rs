use super::*;
use std::collections::BTreeMap;

fn invoke(repo: &Path, state: &Path, args: &[&str]) -> std::process::Output {
    crate::common::af()
        .current_dir(repo)
        .args(args)
        .arg("--state")
        .arg(state)
        .arg("--json")
        .output()
        .unwrap()
}
fn write_file(repo: &Path, file: &Value) {
    std::fs::write(
        repo.join("ticket.json"),
        serde_json::to_vec_pretty(file).unwrap(),
    )
    .unwrap();
}
fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type().unwrap().is_dir() {
                collect(root, &entry.path(), files);
            } else if name.ends_with(".sqlite-shm") || name.ends_with(".sqlite-wal") {
                // What a reader of the log creates, such as the Storage Budget's sweep after the
                // refused command (ADR-0144): no state, so not compared.
                continue;
            } else {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().into(),
                    std::fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

#[test]
fn zero_file_wall_refuses_before_state_and_does_not_consume_the_task_id() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(dir.path());
    let original: Value =
        serde_json::from_slice(&std::fs::read(repo.join("ticket.json")).unwrap()).unwrap();
    let mut zero = original.clone();
    zero["limits"]["wall_ms"] = 0.into();
    write_file(&repo, &zero);
    for command in ["start", "plan"] {
        let out = invoke(&repo, &state, &["task", command, "--file", "ticket.json"]);
        // The status and both streams, so a process ended by a signal (which prints nothing)
        // reads as that rather than as a refusal that lost its message.
        let printed = format!(
            "af task {command}: {:?}\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.code(), Some(1), "{printed}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("must be positive"),
            "{printed}"
        );
        let document: Value = serde_json::from_slice(&out.stdout).expect(&printed);
        assert_eq!(document["schema"], "af/error@2", "{printed}");
        assert!(
            document["error"]
                .as_str()
                .is_some_and(|error| error.contains("must be positive")),
            "{printed}"
        );
        assert!(
            !state.exists(),
            "invalid file created state or occupied its Task ID"
        );
    }
    // The original valid file still opens that same ID once, without running Workers.
    write_file(&repo, &original);
    let plan = af(&repo, &state, &["plan", "--file", "ticket.json"]);
    assert_eq!(plan["task_id"], original["task_id"]);
    assert_eq!(plan["attempts"], 0);
    let before = inventory(&state);
    zero["task_id"] = "another-id".into();
    write_file(&repo, &zero);
    let out = invoke(
        &repo,
        &state,
        &["task", "start", "--execute", "--file", "ticket.json"],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("must be positive"),
        "{:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        inventory(&state),
        before,
        "invalid duration changed existing state/CAS"
    );
}

#[test]
fn task_timeout_zero_is_a_usage_error_but_review_file_is_validated_by_its_adapter() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(dir.path());
    let out = invoke(
        &repo,
        &state,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            "ticket.json",
            "--timeout-secs",
            "0",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(!state.exists());
    let overflow = u64::MAX.to_string();
    let out = invoke(
        &repo,
        &state,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            "ticket.json",
            "--timeout-secs",
            &overflow,
        ],
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("Task timeout overflow"));
    assert!(!state.exists());

    // Shared Review flags retain their historical parser. Only the Task-file adapter
    // rejects its effective zero duration, before touching a Campaign or Task Store.
    let other = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(other.path(), "review");
    let out = invoke(
        &repo,
        &state,
        &[
            "review",
            "run",
            "--file",
            "review.json",
            "--timeout-secs",
            "0",
        ],
    );
    assert!(!out.status.success());
    assert_ne!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("must be positive"));
    assert!(!state.exists());
}

#[test]
fn review_file_plan_names_the_whole_task_timeout_scope() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(dir.path(), "review");
    let output = crate::common::af()
        .current_dir(&repo)
        .args([
            "review",
            "plan",
            "--file",
            "review.json",
            "--timeout-secs",
            "37",
        ])
        .arg("--state")
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("timeout --timeout-secs 37: whole Task cap"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn smallest_positive_file_and_cli_durations_reach_source_validation() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(dir.path());
    let mut file: Value =
        serde_json::from_slice(&std::fs::read(repo.join("ticket.json")).unwrap()).unwrap();
    file["limits"]["wall_ms"] = 1.into();
    write_file(&repo, &file);
    // Do not open an intentionally expired Task: an absent source proves both positive
    // boundaries pass duration validation and reach the next existing ingress check.
    let absent = repo.join("absent");
    let out = invoke(
        &repo,
        &state,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            "ticket.json",
            "--timeout-secs",
            "1",
            "--repo",
            absent.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("No such file") || error.contains("not exist"),
        "{error}"
    );
    assert!(!error.contains("must be positive"));
    assert!(!state.exists());
}
