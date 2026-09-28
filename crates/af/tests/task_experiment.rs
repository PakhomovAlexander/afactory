//! Measure and compare (ADR-0124). The experiment fixture measures a Python command that writes
//! `size.txt`'s number of bytes into its private `$TMPDIR` and reports `bytes_written`, once on
//! the source as the baseline and once on the implementer's sealed candidate, compares the two
//! under the `smaller` objective, and dispatches its evaluator only for a passed comparison. The
//! implementer writes the files the goal names after `files=`, so each Task here is one
//! candidate; `mode.txt` makes the measured command misbehave in the ways the kernel must
//! record. No credential, Provider or model is involved.

use std::path::{Path, PathBuf};
use std::process::Command;

use review_core::task::measurement::{
    ComparisonObjective, MeasurementComparisonV1, MeasurementV1, compare_measurements,
};
use review_store::Cas;
use serde_json::{Value, json};

#[path = "support/task_cli.rs"]
mod task_cli;

#[path = "support/schemas.rs"]
mod schemas;

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    /// Prepended to the kernel's `PATH` when set: the warm test's stubbed toolchain.
    bin: Option<PathBuf>,
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The experiment fixture, with `edits` committed on top: path to its new bytes.
fn fixture(edits: &[(&str, String)]) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let (repo, state) = task_cli::fixture_named(&root, "experiment");
    if !edits.is_empty() {
        for (path, bytes) in edits {
            std::fs::write(repo.join(path), bytes).unwrap();
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "experiment variant"]);
    }
    std::fs::create_dir_all(root.join("tasks")).unwrap();
    Fixture {
        root,
        repo,
        state,
        bin: None,
        _root: directory,
    }
}

/// The fixture's code policy with `from` replaced by `to`.
fn policy(from: &str, to: &str) -> (&'static str, String) {
    let path = task_cli_workspace().join("fixtures/task-runtime/experiment/.af/code-policy.toml");
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "{from}");
    (".af/code-policy.toml", text.replace(from, to))
}

fn task_cli_workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn af(fixture: &Fixture, args: &[&str]) -> (i32, String, String) {
    let home = fixture.root.join("home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_af"));
    if let Some(bin) = &fixture.bin {
        command
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env_remove("RUSTUP_HOME")
            .env_remove("RUSTUP_TOOLCHAIN");
    }
    let output = command
        .current_dir(&fixture.repo)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", fixture.root.join("cache"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("AF_CACHE_POLICY_FILE")
        .env_remove("CARGO_TARGET_DIR")
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A Task file whose implementer writes `files`. The wall is ADR-0114's loaded-machine budget.
fn task_file(fixture: &Fixture, task_id: &str, files: Value) -> PathBuf {
    let file = fixture.root.join("tasks").join(format!("{task_id}.json"));
    std::fs::write(
        &file,
        serde_json::to_vec(&json!({
            "schema": "af.task-file/1",
            "task_id": task_id,
            "kind": "implement",
            "goal": format!("Make the measured command write fewer bytes. files={files}"),
            "pipeline": {"name": "fixture/experiment", "fallback": "refuse"},
            "strategy": "small",
            "verification": "evaluation",
            "facts": {},
            "limits": {
                "tokens": 1000,
                "max_attempts": 8,
                "wall_ms": 600_000,
                "verification": {"tokens": 200, "attempts": 3, "wall_ms": 300_000}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    file
}

/// Start and execute one Task, returning its inspection document.
fn start(fixture: &Fixture, task_id: &str, files: Value, expected: i32) -> Value {
    let file = task_file(fixture, task_id, files);
    let (code, stdout, stderr) = af(
        fixture,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            file.to_str().unwrap(),
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, expected, "{stderr}\n{stdout}");
    let outcome: Value = serde_json::from_str(stdout.trim()).unwrap();
    schemas::valid(&schemas::validator("task-inspection-v11.json"), &outcome);
    outcome
}

fn cas(fixture: &Fixture) -> Cas {
    Cas::open_existing(fixture.state.join("cas")).unwrap()
}

/// The public output `port`'s one artifact envelope.
fn output(cas: &Cas, outcome: &Value, port: &str) -> Value {
    let id = outcome["result"]["outputs"][port]["artifact_ids"][0]
        .as_str()
        .unwrap_or_else(|| panic!("no {port} output: {}", outcome["result"]));
    cas.get_json(id).unwrap()
}

fn measurement(cas: &Cas, outcome: &Value, port: &str) -> MeasurementV1 {
    let envelope = output(cas, outcome, port);
    assert_eq!(envelope["type"], "af/Measurement@1");
    schemas::valid(
        &schemas::validator("measurement-v1.json"),
        &envelope["payload"],
    );
    let value: MeasurementV1 = serde_json::from_value(envelope["payload"].clone()).unwrap();
    value.validate().unwrap();
    value
}

fn comparison(cas: &Cas, outcome: &Value) -> MeasurementComparisonV1 {
    let envelope = output(cas, outcome, "comparison");
    assert_eq!(envelope["type"], "af/MeasurementComparison@1");
    schemas::valid(
        &schemas::validator("measurement-comparison-v1.json"),
        &envelope["payload"],
    );
    serde_json::from_value(envelope["payload"].clone()).unwrap()
}

/// The recorded outcome kind of one node in the Task's last run report.
fn node_outcome(outcome: &Value, node: &str) -> String {
    let report = outcome["run_reports"].as_array().unwrap().last().unwrap();
    report["report"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["node"] == node)
        .map(|entry| entry["outcome"]["kind"].as_str().unwrap().to_string())
        .unwrap_or_else(|| "absent".into())
}

/// Whether any Attempt was ever reserved for `node`.
fn attempted(cas: &Cas, outcome: &Value, node: &str) -> bool {
    outcome["execution_records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["record"]["kind"] == "reserved")
        .any(|entry| {
            let invocation = cas
                .get_json(entry["record"]["invocation_id"].as_str().unwrap())
                .unwrap();
            invocation["payload"]["node"] == node
        })
}

const EVALUATE: &str = "root.nodes.trial.nodes.evaluate";
const COMPARE: &str = "root.nodes.trial.nodes.compare";

fn conclusion(comparison: &MeasurementComparisonV1) -> &'static str {
    comparison.metrics["bytes_written"].conclusion.as_str()
}

#[test]
fn an_improving_candidate_passes_its_comparison_and_is_verified() {
    let fixture = fixture(&[]);
    let outcome = start(&fixture, "improve", json!({"size.txt": "80\n"}), 0);
    assert_eq!(outcome["result"]["domain_conclusion"], "verified");
    let cas = cas(&fixture);
    let baseline = measurement(&cas, &outcome, "baseline");
    let candidate = measurement(&cas, &outcome, "candidate");
    for (measured, bytes) in [(&baseline, "100"), (&candidate, "80")] {
        assert_eq!(measured.measure, "write");
        assert_eq!(measured.runs.len(), 3);
        assert!(!measured.warm);
        assert_eq!(measured.summary["bytes_written"].median.to_string(), bytes);
        assert_eq!(measured.summary["bytes_written"].n, 3);
        assert_eq!(measured.summary["elapsed_ms"].n, 3);
        // Every value is the kernel's: the repetition's elapsed time and exit status, and the
        // command's own report only through its retained stdout.
        for run in &measured.runs {
            assert_eq!(run.exit_code, Some(0));
            let stdout = cas.get(run.stdout_id.as_ref().unwrap()).unwrap();
            assert!(String::from_utf8(stdout).unwrap().contains(&format!(
                "\"bytes_written\": {{\"value\": \"{bytes}\", \"unit\": \"bytes\"}}"
            )));
        }
    }
    // The baseline measured the captured source, the candidate the Snapshot that was delivered.
    assert_eq!(
        Some(&baseline.snapshot_id),
        outcome["result"]["outputs"]["baseline"]["snapshot_id"]
            .as_str()
            .map(String::from)
            .as_ref()
    );
    assert_eq!(
        candidate.snapshot_id,
        outcome["result"]["outputs"]["snapshot"]["snapshot_id"]
            .as_str()
            .unwrap()
    );
    assert_ne!(baseline.snapshot_id, candidate.snapshot_id);
    let recorded = comparison(&cas, &outcome);
    assert_eq!(serde_json::to_value(recorded.outcome).unwrap(), "passed");
    assert_eq!(conclusion(&recorded), "improved");
    let row = &recorded.metrics["bytes_written"];
    assert_eq!(row.improvement.as_ref().unwrap().to_string(), "20");
    assert_eq!(row.ratio.as_ref().unwrap().to_string(), "1/5");
    assert_eq!(node_outcome(&outcome, EVALUATE), "completed");
    // The comparison replays byte-identically from the same two Measurements, and it never
    // had an Attempt to spend.
    assert!(!attempted(&cas, &outcome, COMPARE));
    assert!(attempted(&cas, &outcome, "root.nodes.baseline"));
    assert!(attempted(&cas, &outcome, EVALUATE));
    let policy: review_pipeline::task::code::CodeTaskPolicy = toml::from_str(
        &std::fs::read_to_string(fixture.repo.join(".af/code-policy.toml")).unwrap(),
    )
    .unwrap();
    let ids = |port: &str| {
        outcome["result"]["outputs"][port]["artifact_ids"][0]
            .as_str()
            .unwrap()
            .to_string()
    };
    let replayed = compare_measurements(
        &recorded.plan_id,
        &recorded.policy_id,
        (&ids("baseline"), &baseline),
        (&ids("candidate"), &candidate),
        ComparisonObjective {
            name: "smaller",
            objective: &policy.objectives["smaller"],
        },
    )
    .unwrap();
    let recorded_payload = output(&cas, &outcome, "comparison")["payload"].clone();
    let replayed = serde_json::to_value(&replayed).unwrap();
    assert_eq!(replayed, recorded_payload);
    assert_eq!(
        review_store::canonical::content_id(&replayed).unwrap(),
        review_store::canonical::content_id(&recorded_payload).unwrap()
    );

    // `af task output` renders the comparison as one Markdown table; JSON is the envelope.
    let markdown = fixture.root.join("comparison.md");
    let (code, _, stderr) = af(
        &fixture,
        &[
            "task",
            "output",
            "improve",
            "--port",
            "comparison",
            "--format",
            "markdown",
            "--output",
            markdown.to_str().unwrap(),
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let text = std::fs::read_to_string(&markdown).unwrap();
    // One table: one header, one separator, one row per metric, nothing else in table form.
    let rows: Vec<_> = text.lines().filter(|line| line.starts_with('|')).collect();
    assert_eq!(rows.len(), 4, "{text}");
    assert_eq!(
        text.lines().filter(|line| line.starts_with("|---")).count(),
        1,
        "{text}"
    );
    assert!(
        text.contains(
            "| bytes_written (objective) | bytes | 100 | 80 | 20 | 0.2 | 3 / 3 | improved |"
        ),
        "{text}"
    );
    let exported = fixture.root.join("comparison.json");
    let (code, _, stderr) = af(
        &fixture,
        &[
            "task",
            "output",
            "improve",
            "--port",
            "comparison",
            "--output",
            exported.to_str().unwrap(),
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let exported: Value = serde_json::from_slice(&std::fs::read(exported).unwrap()).unwrap();
    assert_eq!(exported, output(&cas, &outcome, "comparison"));

    // `af task show` names each measurement's median elapsed time and each conclusion.
    let (code, stdout, stderr) = af(
        &fixture,
        &[
            "task",
            "show",
            "improve",
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let median = |m: &MeasurementV1| m.summary["elapsed_ms"].median.to_string();
    assert!(
        stdout.contains(&format!(
            "measurement root.nodes.baseline write: median elapsed {} ms over 3 repetitions",
            median(&baseline)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "measurement root.nodes.trial.nodes.measure write: median elapsed {} ms over 3 repetitions",
            median(&candidate)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "comparison root.nodes.trial.nodes.compare smaller: improved on bytes_written (100 → 80), passed"
        ),
        "{stdout}"
    );
}

#[test]
fn a_regressing_or_unchanged_candidate_fails_its_comparison_and_is_never_evaluated() {
    let fixture = fixture(&[]);
    for (task, size, expected) in [
        ("regress", "120\n", "regressed"),
        ("unchanged", "100\n", "unchanged"),
        ("marginal", "95\n", "below_threshold"),
    ] {
        let outcome = start(&fixture, task, json!({"size.txt": size}), 4);
        let cas = cas(&fixture);
        let recorded = comparison(&cas, &outcome);
        assert_eq!(conclusion(&recorded), expected, "{task}");
        assert_eq!(
            serde_json::to_value(recorded.outcome).unwrap(),
            "failed",
            "{task}"
        );
        // A failed comparison cannot be talked into a passed evaluation: there is none.
        assert_eq!(node_outcome(&outcome, EVALUATE), "suppressed", "{task}");
        assert!(!attempted(&cas, &outcome, EVALUATE), "{task}");
        assert_ne!(outcome["result"]["domain_conclusion"], "verified");
    }
}

#[test]
fn too_few_repetitions_make_the_comparison_inconclusive() {
    let fixture = fixture(&[policy("\nrepetitions = 3\n", "\nrepetitions = 1\n")]);
    let outcome = start(&fixture, "once", json!({"size.txt": "50\n"}), 4);
    let cas = cas(&fixture);
    assert_eq!(measurement(&cas, &outcome, "candidate").runs.len(), 1);
    let recorded = comparison(&cas, &outcome);
    assert_eq!(conclusion(&recorded), "inconclusive");
    assert_eq!(recorded.metrics["bytes_written"].baseline_n, 1);
    assert_eq!(
        serde_json::to_value(recorded.outcome).unwrap(),
        "inconclusive"
    );
    assert!(!attempted(&cas, &outcome, EVALUATE));
}

#[test]
fn a_failed_measurement_is_never_partial_and_is_never_evaluated() {
    let fixture = fixture(&[]);
    for (task, mode, reason, detail) in [
        ("exits", "exit\n", "exit", "the command exited 3"),
        (
            "killed",
            "kill\n",
            "exit",
            "the command was ended by a signal",
        ),
        (
            "counts",
            "count\n",
            "unit_mismatch",
            "metric bytes_written reported unit count, the measure declares bytes",
        ),
        (
            "mutates",
            "mutate\n",
            "source_mutated",
            "Check mutated its input Snapshot",
        ),
    ] {
        let outcome = start(&fixture, task, json!({"mode.txt": mode}), 4);
        let cas = cas(&fixture);
        let candidate = measurement(&cas, &outcome, "candidate");
        assert_eq!(serde_json::to_value(candidate.outcome).unwrap(), "failed");
        let failure = candidate.failure.as_ref().unwrap();
        assert_eq!(failure.reason.as_str(), reason, "{task}");
        assert_eq!(failure.detail, detail, "{task}");
        // The first repetition failed: no later one ran, its receipt is kept, nothing is
        // summarized from a partial sample.
        assert_eq!(failure.repetition, 1);
        assert_eq!(candidate.runs.len(), 1);
        assert!(candidate.runs[0].stdout_id.is_some());
        // An exit code is the command's own; a signal leaves none, never the runner's sentinel.
        match task {
            "exits" => assert_eq!(candidate.runs[0].exit_code, Some(3)),
            "killed" => assert_eq!(candidate.runs[0].exit_code, None),
            _ => {}
        }
        assert!(candidate.summary.is_empty());
        let recorded = comparison(&cas, &outcome);
        assert_eq!(
            serde_json::to_value(recorded.outcome).unwrap(),
            "inconclusive"
        );
        assert!(
            recorded
                .metrics
                .values()
                .all(|row| { row.candidate_median.is_none() && row.candidate_n == 0 })
        );
        assert_eq!(node_outcome(&outcome, EVALUATE), "suppressed", "{task}");
        assert!(!attempted(&cas, &outcome, EVALUATE), "{task}");
    }
    // The source a mutating command ran against is unchanged: the file it added is in no
    // Snapshot, candidate or output.
    let (code, stdout, _) = af(
        &fixture,
        &[
            "task",
            "show",
            "mutates",
            "--json",
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0);
    let shown: Value = serde_json::from_str(stdout.trim()).unwrap();
    let cas = cas(&fixture);
    let snapshot = cas
        .get_json(
            shown["result"]["outputs"]["snapshot"]["snapshot_id"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let manifest = cas
        .get_json(snapshot["manifest_id"].as_str().unwrap())
        .unwrap();
    assert!(!manifest.to_string().contains("added.txt"), "{manifest}");
}

#[test]
fn an_even_sample_median_is_the_exact_mean_of_its_middle_values() {
    let directory = tempfile::tempdir().unwrap();
    let counter = directory.path().join("counter");
    let fixture = fixture(&[
        policy("\nrepetitions = 3\n", "\nrepetitions = 2\n"),
        ("mode.txt", "counter\n".into()),
        ("counter.txt", format!("{}\n", counter.display())),
    ]);
    // Every run writes one more byte than the last: 100 and 101 for the baseline, 102 and
    // 103 for the unchanged candidate.
    let outcome = start(&fixture, "even", json!({"size.txt": "100\n"}), 4);
    let cas = cas(&fixture);
    let baseline = measurement(&cas, &outcome, "baseline");
    assert_eq!(
        baseline.summary["bytes_written"].median.to_string(),
        "100.5"
    );
    assert_eq!(baseline.summary["bytes_written"].min.to_string(), "100");
    assert_eq!(baseline.summary["bytes_written"].max.to_string(), "101");
    let candidate = measurement(&cas, &outcome, "candidate");
    assert_eq!(
        candidate.summary["bytes_written"].median.to_string(),
        "102.5"
    );
    let recorded = comparison(&cas, &outcome);
    assert_eq!(
        recorded.metrics["bytes_written"]
            .improvement
            .as_ref()
            .unwrap()
            .to_string(),
        "-2"
    );
    // Two repetitions are fewer than the objective's three.
    assert_eq!(conclusion(&recorded), "inconclusive");
}

#[test]
fn a_zero_baseline_is_inconclusive_against_a_non_zero_candidate() {
    let fixture = fixture(&[("size.txt", "0\n".into())]);
    let outcome = start(&fixture, "zero", json!({"size.txt": "5\n"}), 4);
    let cas = cas(&fixture);
    let recorded = comparison(&cas, &outcome);
    let row = &recorded.metrics["bytes_written"];
    assert_eq!(row.baseline_median.as_ref().unwrap().to_string(), "0");
    assert_eq!(row.improvement.as_ref().unwrap().to_string(), "-5");
    assert!(row.ratio.is_none());
    assert_eq!(conclusion(&recorded), "inconclusive");
    assert!(!attempted(&cas, &outcome, EVALUATE));
}

#[test]
fn the_plan_compiles_with_zero_attempts_and_a_repetition_budget_over_the_check_wall_is_refused() {
    let plan = |fixture: &Fixture| {
        let file = task_file(fixture, "planned", json!({"size.txt": "80\n"}));
        af(
            fixture,
            &[
                "task",
                "plan",
                "--file",
                file.to_str().unwrap(),
                "--state",
                fixture.state.to_str().unwrap(),
                "--json",
            ],
        )
    };
    let fits = fixture(&[]);
    let (code, stdout, stderr) = plan(&fits);
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    let planned: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(planned["plan_id"].as_str().is_some(), "{planned}");
    assert_eq!(planned["attempts"], 0, "{planned}");

    // Three repetitions of 20 s do not fit a 30 s measure Attempt.
    let over = fixture(&[policy("check_wall_ms = 60000\n", "check_wall_ms = 30000\n")]);
    let (code, stdout, stderr) = plan(&over);
    assert_ne!(code, 0, "{stdout}");
    let refusal = format!("{stdout}{stderr}");
    assert!(
        refusal.contains(
            "root.nodes.baseline needs 60000 ms for its repetitions (repetitions × wall_ms), \
             more than the captured check_wall_ms of 30000 ms"
        ),
        "{refusal}"
    );
    assert!(
        !over.state.join("events.sqlite").exists() || {
            let (_, listed, _) = af(
                &over,
                &[
                    "task",
                    "list",
                    "--state",
                    over.state.to_str().unwrap(),
                    "--json",
                ],
            );
            !listed.contains("\"planned\"")
        }
    );
}

/// Replace `from` with `to` in one fixture package file and re-pin that package.
fn package_edit(package: &str, file: &str, from: &str, to: &str) -> Vec<(&'static str, String)> {
    let workspace = task_cli_workspace().join("fixtures/task-runtime/experiment");
    let path = workspace.join(".af/task-packages").join(package).join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "{from}");
    let edited = text.replace(from, to);
    // Re-pin from a scratch copy of the package with the edited bytes.
    let scratch = tempfile::tempdir().unwrap();
    task_cli::copy_tree(
        &workspace.join(".af/task-packages").join(package),
        scratch.path(),
    );
    std::fs::write(scratch.path().join(file), &edited).unwrap();
    let digest = review_config::lock::package_digest(package, scratch.path()).unwrap();
    let catalog_path = workspace.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(catalog_path).unwrap()).unwrap();
    catalog["packages"][package]["digest"] = toml::Value::String(digest);
    let target: &'static str =
        Box::leak(format!(".af/task-packages/{package}/{file}").into_boxed_str());
    vec![
        (target, edited),
        (".af/task-catalog.toml", toml::to_string(&catalog).unwrap()),
    ]
}

#[test]
fn the_compiler_refuses_undeclared_measures_mixed_comparisons_and_foreign_objectives() {
    let refused = |edits: Vec<(&str, String)>, message: &str| {
        let fixture = fixture(&edits);
        let file = task_file(&fixture, "refused", json!({"size.txt": "80\n"}));
        let (code, stdout, stderr) = af(
            &fixture,
            &[
                "task",
                "plan",
                "--file",
                file.to_str().unwrap(),
                "--state",
                fixture.state.to_str().unwrap(),
                "--json",
            ],
        );
        let text = format!("{stdout}{stderr}");
        assert_ne!(code, 0, "{text}");
        assert!(text.contains(message), "expected {message:?} in {text}");
    };
    // A measure the captured policy does not declare.
    refused(
        package_edit(
            "fixture/experiment",
            "pipeline.toml",
            "op = \"measure\"\nmeasures = [\"write\"]",
            "op = \"measure\"\nmeasures = [\"missing\", \"write\"]",
        ),
        "root.nodes.baseline names measure missing, which the captured code policy does not declare",
    );
    // A second declared measure, and a comparison of it against `write`.
    let other = "\n[measures.other]\nrepetitions = 1\nwarm = false\nwall_ms = 1000\n\n\
                 [measures.other.command]\nprogram = \"/usr/bin/python3\"\n\n\
                 [[measures.other.command.args]]\nvalue = \"measure.py\"\nprovenance = \"literal\"\n";
    let (path, text) = policy(
        "\n[objectives.smaller]",
        &format!("{other}\n[objectives.smaller]"),
    );
    // Room for both measures in one baseline Attempt.
    let text = text.replace("check_wall_ms = 60000", "check_wall_ms = 120000");
    let mut edits = package_edit(
        "fixture/experiment",
        "pipeline.toml",
        "measures = [\"write\"]\n\n[nodes.inputs.source]\nkind = \"input\"",
        "measures = [\"other\", \"write\"]\n\n[nodes.inputs.source]\nkind = \"input\"",
    );
    let pipeline = edits[0].1.replace(
        "[nodes.inputs.baseline]\nkind = \"node\"\nnode = \"baseline\"\nport = \"write\"",
        "[nodes.inputs.baseline]\nkind = \"node\"\nnode = \"baseline\"\nport = \"other\"",
    );
    edits[0].1 = pipeline.clone();
    let scratch = tempfile::tempdir().unwrap();
    let workspace = task_cli_workspace().join("fixtures/task-runtime/experiment");
    task_cli::copy_tree(
        &workspace.join(".af/task-packages/fixture/experiment"),
        scratch.path(),
    );
    std::fs::write(scratch.path().join("pipeline.toml"), &pipeline).unwrap();
    let mut catalog: toml::Value = toml::from_str(&edits[1].1).unwrap();
    catalog["packages"]["fixture/experiment"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("fixture/experiment", scratch.path()).unwrap(),
    );
    edits[1].1 = toml::to_string(&catalog).unwrap();
    edits.push((path, text.clone()));
    refused(
        edits,
        "root.nodes.trial.nodes.compare compares Measurements of different measures: other and write",
    );
    // An objective the policy lacks, and one that compares another measure.
    refused(
        package_edit(
            "fixture/experiment-trial",
            "pipeline.toml",
            "objective = \"smaller\"",
            "objective = \"missing\"",
        ),
        "root.nodes.trial.nodes.compare names objective missing, which the captured code policy does not declare",
    );
    let foreign = format!(
        "{text}\n[objectives.fewer]\nmeasure = \"other\"\nmetric = \"elapsed_ms\"\n\
         direction = \"lower\"\nmin_improvement_ratio = 0\n"
    );
    let mut edits = package_edit(
        "fixture/experiment-trial",
        "pipeline.toml",
        "objective = \"smaller\"",
        "objective = \"fewer\"",
    );
    edits.push((path, foreign));
    refused(
        edits,
        "root.nodes.trial.nodes.compare: objective fewer does not compare measure write",
    );
}

/// A stubbed rustup proxy, as the warm-check fixture uses: it answers only with the kernel's
/// rustup home and auto-install off, so a key proves no toolchain was downloaded.
fn stub_toolchain(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    for (program, text) in [
        ("rustc", "rustc 1.88.0 (fixture)\nhost: fixture-host-triple"),
        ("cargo", "cargo 1.88.0 (fixture)"),
    ] {
        let path = bin.join(program);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ -z \"$RUSTUP_HOME\" ] || [ \"$RUSTUP_AUTO_INSTALL\" != 0 ]; then exit 1; fi\n\
                 printf '%s\\n' '{text}'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Toolchain proxies that fail every probe: the warm layer resolves no key and runs cold.
fn broken_toolchain(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    for program in ["rustc", "cargo"] {
        let path = bin.join(program);
        std::fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn a_warm_measure_builds_into_the_warm_check_cache_and_records_its_key_and_bytes() {
    let (path, text) = policy("\nwarm = false\n", "\nwarm = true\n");
    let text = format!("{text}\n[warm]\nbuild_cache = [\"cargo_target\"]\n");
    let mut fixture = fixture(&[(path, text), ("mode.txt", "warm\n".into())]);
    std::fs::create_dir_all(fixture.root.join("home/.rustup")).unwrap();
    let bin = fixture.root.join("bin");
    stub_toolchain(&bin);
    fixture.bin = Some(bin);
    let outcome = start(&fixture, "warm", json!({"size.txt": "80\n"}), 0);
    let cas = cas(&fixture);
    let baseline = measurement(&cas, &outcome, "baseline");
    assert!(baseline.warm);
    assert!(baseline.toolchain_id.is_some());
    // The first repetition built into an empty directory; every later one found its product.
    let had: Vec<_> = baseline
        .runs
        .iter()
        .map(|run| run.cache.as_ref().map(|cache| (cache.warm, cache.bytes)))
        .collect();
    assert_eq!(
        had,
        [Some((true, 0)), Some((true, 4096)), Some((true, 4096))]
    );
    let candidate = measurement(&cas, &outcome, "candidate");
    assert_eq!(candidate.toolchain_id, baseline.toolchain_id);
    assert!(candidate.runs.iter().all(|run| {
        run.cache
            .as_ref()
            .is_some_and(|cache| cache.warm && cache.bytes == 4096 && cache.reason.is_none())
    }));
    // The build product lives in the machine-local cache, never in a Snapshot.
    let cache = fixture.root.join("cache/af/task-build-cache");
    let mut found = false;
    for project in std::fs::read_dir(&cache).unwrap().flatten() {
        for key in std::fs::read_dir(project.path()).unwrap().flatten() {
            found |= key.path().join("cargo_target/build.bin").is_file();
        }
    }
    assert!(found, "no warm build product below {}", cache.display());
    let snapshot = cas.get_json(&candidate.snapshot_id).unwrap();
    let manifest = cas
        .get_json(snapshot["manifest_id"].as_str().unwrap())
        .unwrap();
    assert!(!manifest.to_string().contains("build.bin"));

    // A repetition whose warm directory the kernel discards before binding — a link planted
    // inside makes it suspect — runs against the emptied directory and records that: cold, with
    // the discard as its reason; the repetitions after it find the directory warm again.
    let target = {
        let mut found = None;
        for project in std::fs::read_dir(&cache).unwrap().flatten() {
            for key in std::fs::read_dir(project.path()).unwrap().flatten() {
                let directory = key.path().join("cargo_target");
                if directory.is_dir() {
                    found = Some(directory);
                }
            }
        }
        found.expect("a warm cargo_target below the cache")
    };
    std::os::unix::fs::symlink("/etc/hosts", target.join("link")).unwrap();
    let outcome = start(&fixture, "warm-discarded", json!({"size.txt": "80\n"}), 0);
    let discarded = measurement(&cas, &outcome, "baseline");
    let first = discarded.runs[0].cache.as_ref().unwrap();
    assert!(!first.warm);
    assert_eq!(first.bytes, 0);
    assert!(
        first
            .reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("discarded: ")),
        "{:?}",
        first.reason
    );
    assert!(discarded.runs[1..].iter().all(|run| {
        run.cache
            .as_ref()
            .is_some_and(|cache| cache.warm && cache.reason.is_none())
    }));
    let (code, stdout, _) = af(
        &fixture,
        &[
            "task",
            "show",
            "warm-discarded",
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains(", warm 2 of 3"), "{stdout}");

    // The same measure where the kernel cannot resolve a toolchain — its proxies fail every
    // probe — runs every repetition cold against a private target, and the Measurement says so:
    // the policy asked for warm, each run records what it had and why, and `af task show`
    // prints it.
    let (path, text) = policy("\nwarm = false\n", "\nwarm = true\n");
    let text = format!("{text}\n[warm]\nbuild_cache = [\"cargo_target\"]\n");
    let mut cold = self::fixture(&[(path, text)]);
    let broken = cold.root.join("bin");
    broken_toolchain(&broken);
    cold.bin = Some(broken);
    let outcome = start(&cold, "warm-unresolved", json!({"size.txt": "80\n"}), 0);
    let cold_cas = self::cas(&cold);
    let unresolved = measurement(&cold_cas, &outcome, "baseline");
    assert!(unresolved.warm, "what the policy asked for");
    assert!(unresolved.toolchain_id.is_none());
    let mut reasons = std::collections::BTreeSet::new();
    for run in &unresolved.runs {
        let had = run
            .cache
            .as_ref()
            .expect("a warm measure records its cache");
        assert!(!had.warm);
        assert_eq!(had.bytes, 0);
        reasons.insert(had.reason.clone().expect("a cold run says why"));
    }
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    let reason = reasons.into_iter().next().unwrap();
    let (code, stdout, _) = af(
        &cold,
        &[
            "task",
            "show",
            "warm-unresolved",
            "--state",
            cold.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains(&format!(", cold ({reason})")), "{stdout}");
}

#[test]
fn a_repetition_past_its_wall_is_a_timeout_that_keeps_what_it_printed() {
    let (path, text) = policy("wall_ms = 20000", "wall_ms = 1000");
    let fixture = fixture(&[(path, text), ("mode.txt", "sleep\n".into())]);
    let outcome = start(&fixture, "sleeps", json!({"size.txt": "80\n"}), 4);
    let cas = cas(&fixture);
    let baseline = measurement(&cas, &outcome, "baseline");
    assert_eq!(serde_json::to_value(baseline.outcome).unwrap(), "failed");
    let failure = baseline.failure.as_ref().unwrap();
    assert_eq!(failure.reason.as_str(), "timeout");
    assert_eq!(
        failure.detail,
        "the repetition exceeded its wall_ms of 1000 ms"
    );
    assert_eq!(failure.repetition, 1);
    assert_eq!(baseline.runs.len(), 1);
    assert!(baseline.runs[0].exit_code.is_none());
    assert!(
        baseline.runs[0].stdout_id.is_some(),
        "what the command printed before the kernel ended it is kept"
    );
    assert!(baseline.summary.is_empty());
}

/// Install the staged `kernel/experiment` packages and the `release_build` policy tables of
/// `fixtures/kernel-experiment` into a copy of this repository's `.af/`, exactly as its README
/// says, into a fresh Git repository — or, where a human has already taken a step in the
/// committed `.af/`, verify that what they installed is what is staged.
fn kernel_repository(root: &Path) -> PathBuf {
    let workspace = task_cli_workspace();
    let repo = root.join("kernel");
    task_cli::copy_tree(&workspace.join(".af"), &repo.join(".af"));
    // The release pin would dispatch to (and install) another `af`; this test runs this one.
    std::fs::remove_file(repo.join(".af/af.lock")).unwrap();
    task_cli::copy_tree(
        &workspace.join("fixtures/kernel-experiment"),
        &repo.join("fixtures/kernel-experiment"),
    );
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    std::fs::copy(
        workspace.join("scripts/measure-release.sh"),
        repo.join("scripts/measure-release.sh"),
    )
    .unwrap();
    let policy_path = repo.join(".af/code-policy.toml");
    let mut policy = std::fs::read_to_string(&policy_path).unwrap();
    if !policy.contains("[measures.release_build]") {
        policy.push('\n');
        policy.push_str(
            &std::fs::read_to_string(
                workspace.join("fixtures/kernel-experiment/code-policy-measures.toml"),
            )
            .unwrap(),
        );
        std::fs::write(&policy_path, policy).unwrap();
    }
    let staged: toml::Value = toml::from_str(
        &std::fs::read_to_string(workspace.join("fixtures/kernel-experiment/catalog.toml"))
            .unwrap(),
    )
    .unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    for name in [
        "kernel/experiment",
        "kernel/experiment-trial",
        "kernel/experiment-evaluator",
    ] {
        let mut pin = staged["packages"][name].clone();
        let path = format!(".af/task-packages/{name}");
        if !repo.join(&path).is_dir() {
            task_cli::copy_tree(&repo.join(pin["path"].as_str().unwrap()), &repo.join(&path));
        }
        pin["path"] = toml::Value::String(path);
        match catalog["packages"].get(name) {
            Some(installed) => assert_eq!(
                installed["digest"], pin["digest"],
                "{name}: the committed pin is the staged package"
            ),
            None => {
                catalog["packages"]
                    .as_table_mut()
                    .unwrap()
                    .insert(name.into(), pin);
            }
        }
    }
    // The staged pin is the committed catalog's pin of the one package both share.
    assert_eq!(
        staged["packages"]["kernel/implementer"]["digest"],
        catalog["packages"]["kernel/implementer"]["digest"]
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "kernel experiment"],
    ] {
        git(&repo, &args);
    }
    repo
}

/// A local command Worker replacing `package` with its exact committed contract and schemas:
/// only the runner and the model-token reservation differ, so no Provider is needed to plan.
fn local_replacement(repo: &Path, directory: &Path, package: &str, local: &str) -> toml::Value {
    let target = directory.join(local);
    task_cli::copy_tree(&repo.join(".af/task-packages").join(package), &target);
    let manifest = target.join("worker.toml");
    let mut worker: toml::Value =
        toml::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    worker["name"] = toml::Value::String(local.into());
    worker["runner"] = toml::from_str(
        "kind = \"command\"\n[command]\nprogram = \"/usr/bin/python3\"\n\
         [[command.args]]\nvalue = \"@package/worker.py\"\nprovenance = \"literal\"\n",
    )
    .unwrap();
    // A command Worker reserves no model tokens.
    worker["signature"]["attempt"]["tokens"] = toml::Value::Integer(0);
    std::fs::write(&manifest, toml::to_string(&worker).unwrap()).unwrap();
    std::fs::write(target.join("worker.py"), "raise SystemExit(1)\n").unwrap();
    let mut pin = toml::map::Map::new();
    pin.insert("version".into(), worker["version"].clone());
    pin.insert(
        "digest".into(),
        toml::Value::String(review_config::lock::package_digest(local, &target).unwrap()),
    );
    pin.insert("path".into(), toml::Value::String(local.into()));
    toml::Value::Table(pin)
}

#[test]
fn this_repositorys_experiment_pipeline_passes_its_catalog_test_and_plans_the_release_build() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let repo = kernel_repository(&root);
    let fixture = Fixture {
        root: root.clone(),
        repo: repo.clone(),
        state: root.join("state"),
        bin: None,
        _root: directory,
    };
    let (code, stdout, stderr) = af(
        &fixture,
        &[
            "catalog",
            "test",
            "--source",
            ".",
            "--manifest",
            "fixtures/kernel-experiment/catalog.toml",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    let tested: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(tested["contract_fixtures"], "passed");
    assert_eq!(
        tested["pipelines"],
        json!(["kernel/experiment", "kernel/experiment-trial"])
    );

    // Planning compiles the committed release_build measure and objective with zero Attempts;
    // local command stand-ins with the exact committed contracts replace the two model Workers.
    let local = root.join("local");
    std::fs::create_dir_all(&local).unwrap();
    let mut packages = toml::map::Map::new();
    for (package, name) in [
        ("kernel/implementer", "local/implementer"),
        ("kernel/experiment-evaluator", "local/evaluator"),
    ] {
        packages.insert(name.into(), local_replacement(&repo, &local, package, name));
    }
    let mut bindings = toml::map::Map::new();
    bindings.insert(
        "schema".into(),
        toml::Value::String("af.task-bindings/1".into()),
    );
    bindings.insert("packages".into(), toml::Value::Table(packages));
    bindings.insert(
        "slots".into(),
        toml::from_str(
            "\"root.slots.implementer\" = \"local/implementer\"\n\
             \"root.slots.evaluator\" = \"local/evaluator\"\n",
        )
        .unwrap(),
    );
    let bindings_path = local.join("bindings.toml");
    std::fs::write(&bindings_path, toml::to_string(&bindings).unwrap()).unwrap();
    std::fs::create_dir_all(root.join("tasks")).unwrap();
    let file = root.join("tasks/release.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&json!({
            "schema": "af.task-file/1",
            "task_id": "release-build",
            "kind": "implement",
            "goal": "Make the release build faster by changing Cargo.toml profiles only.",
            "pipeline": {"name": "kernel/experiment", "fallback": "refuse"},
            "strategy": "small",
            "verification": "evaluation",
            "facts": {},
            "limits": {
                "tokens": 3_000_000,
                "max_attempts": 8,
                "wall_ms": 36_000_000,
                "verification": {"tokens": 400_000, "attempts": 3, "wall_ms": 7_200_000}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, stdout, stderr) = af(
        &fixture,
        &[
            "task",
            "plan",
            "--file",
            file.to_str().unwrap(),
            "--bindings",
            bindings_path.to_str().unwrap(),
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    let planned: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(planned["plan_id"].as_str().is_some(), "{planned}");
    assert_eq!(planned["attempts"], 0);
    let (code, tree, stderr) = af(
        &fixture,
        &[
            "task",
            "explain",
            "release-build",
            "--tree",
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(tree.contains("measure: release_build"), "{tree}");
    assert!(tree.contains("compare: release_build_time"), "{tree}");
}

#[test]
fn the_release_measure_script_reports_its_target_directory_and_binary_bytes() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let bin = directory.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // A stand-in `cargo` that "builds" a 1000-byte binary and a 24-byte dependency file.
    std::fs::write(
        bin.join("cargo"),
        "#!/bin/sh\n[ \"$*\" = 'build --release -p af --locked' ] || exit 9\n\
         mkdir -p \"$CARGO_TARGET_DIR/release/deps\"\n\
         head -c 1000 /dev/zero > \"$CARGO_TARGET_DIR/release/af\"\n\
         head -c 24 /dev/zero > \"$CARGO_TARGET_DIR/release/deps/af.d\"\n\
         echo '   Compiling af' >&2\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let target = directory.path().join("target");
    let output = Command::new("bash")
        .arg(task_cli_workspace().join("scripts/measure-release.sh"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let declared = std::collections::BTreeMap::from([
        (
            "target_bytes".to_string(),
            review_core::task::measurement::MetricUnitV1::Bytes,
        ),
        (
            "binary_bytes".to_string(),
            review_core::task::measurement::MetricUnitV1::Bytes,
        ),
    ]);
    let metrics = review_core::task::measurement::parse_report(&output.stdout, &declared).unwrap();
    assert_eq!(metrics["binary_bytes"].value.to_string(), "1000");
    assert_eq!(metrics["target_bytes"].value.to_string(), "1024");
    // Without a bound target directory the script refuses to build anywhere.
    let refused = Command::new("bash")
        .arg(task_cli_workspace().join("scripts/measure-release.sh"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    assert!(!refused.status.success());
}
