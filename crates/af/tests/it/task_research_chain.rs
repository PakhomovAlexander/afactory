//! ADR-0134 end to end: three Tasks in one Store, chained by artifact identity.
//!
//! One repository carries the shipped `builtin/experiment`, `builtin/report` and
//! `builtin/release-notes` starters side by side, so their Tasks share one Store. The experiment
//! Task measures a baseline and a candidate and compares them. A report Task then binds
//! `comparison` to that comparison, `measurements` to both the `baseline` and the `candidate`
//! Measurements — two Snapshots, so the port names none — and `source` to the candidate tree, and
//! runs to `verified`. A report Task that binds `sources` to a Document Task's `document` is
//! refused at plan time, because the types differ, and so is every other binding ADR-0134 refuses.
//! No credential, Provider or model is involved.

use review_store::Cas;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[allow(dead_code)] // Only `copy_tree` is used: the repository is assembled from the starters.
use crate::task_cli;

use crate::schemas;

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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
    assert!(output.status.success(), "git {args:?}: {}", text(&output));
}

struct Chain {
    _directory: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
}

impl Chain {
    fn af(&self, args: &[&str]) -> Output {
        crate::common::af()
            .current_dir(&self.repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .arg("--state")
            .arg(&self.state)
            .output()
            .unwrap()
    }

    /// One `af task` command with `--json`, expecting `code`.
    fn json(&self, args: &[&str], code: i32) -> Value {
        let mut all = args.to_vec();
        all.push("--json");
        let output = self.af(&all);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}\n{}",
            text(&output)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Write a Task file outside the repository, as ADR-0115 asks.
    fn task_file(&self, name: &str, value: &Value) -> String {
        let path = self.root.join("tasks").join(format!("{name}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        schemas::valid(&schemas::validator("task-file-v1.json"), value);
        path.to_str().unwrap().to_owned()
    }

    /// `af task start --execute` over a Task file, validated as the inspection it prints.
    fn start(&self, file: &str, code: i32) -> Value {
        let done = self.json(&["task", "start", "--execute", "--file", file], code);
        schemas::valid(&schemas::validator("task-inspection-v11.json"), &done);
        done
    }

    fn cas(&self) -> Cas {
        Cas::open_existing(self.state.join("cas")).unwrap()
    }
}

/// Initialize one starter into `root/<profile>` and return that directory.
fn starter(root: &Path, profile: &str) -> PathBuf {
    let init = crate::common::af()
        .current_dir(root)
        .args(["catalog", "init", "--profile", profile])
        .args(["--destination", profile, "--json"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{}", text(&init));
    root.join(profile)
}

fn catalog(directory: &Path) -> toml::Value {
    let path = directory.join(".af/task-catalog.toml");
    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The experiment starter with the report and Document starters' packages, policies and Task
/// files beside it, committed as one repository. Every package keeps the content-addressed
/// directory and pin its starter wrote, so the catalogs merge without re-pinning anything.
fn chain() -> Chain {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let repo = starter(&root, "experiment");
    let report = starter(&root, "report");
    let document = starter(&root, "document");
    let mut merged = catalog(&repo);
    for other in [&report, &document] {
        task_cli::copy_tree(&other.join(".af/packages"), &repo.join(".af/packages"));
        let other = catalog(other);
        let table = merged.as_table_mut().unwrap();
        for key in ["report_policy", "document_policy", "kinds"] {
            if let Some(value) = other.get(key) {
                table.insert(key.into(), value.clone());
            }
        }
        let packages = merged["packages"].as_table_mut().unwrap();
        for (name, pin) in other["packages"].as_table().unwrap() {
            assert!(
                packages.insert(name.clone(), pin.clone()).is_none(),
                "{name}"
            );
        }
    }
    std::fs::write(
        repo.join(".af/task-catalog.toml"),
        toml::to_string(&merged).unwrap(),
    )
    .unwrap();
    for (from, to) in [
        (
            report.join(".af/report-policy.toml"),
            ".af/report-policy.toml",
        ),
        (
            document.join(".af/document-policy.toml"),
            ".af/document-policy.toml",
        ),
        (report.join("report.json"), "report.json"),
        (report.join("sources.json"), "sources.json"),
        (document.join("sources.json"), "document-sources.json"),
    ] {
        std::fs::copy(from, repo.join(to)).unwrap();
    }
    // Both starters name their sources file `sources.json`; the Document one moves aside.
    let mut notes: Value =
        serde_json::from_slice(&std::fs::read(document.join("document.json")).unwrap()).unwrap();
    notes["document_sources"] = json!("document-sources.json");
    std::fs::write(
        repo.join("document.json"),
        serde_json::to_vec_pretty(&notes).unwrap(),
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "research chain"]);
    std::fs::create_dir_all(root.join("tasks")).unwrap();
    Chain {
        state: root.join("state"),
        repo,
        root,
        _directory: directory,
    }
}

/// The report starter's Task file with its captured sources replaced by `inputs`.
fn report_task(chain: &Chain, task_id: &str, inputs: Value) -> Value {
    let bytes = std::fs::read(chain.repo.join("report.json")).unwrap();
    let mut file: Value = serde_json::from_slice(&bytes).unwrap();
    file["task_id"] = json!(task_id);
    file.as_object_mut().unwrap().remove("report_sources");
    file["inputs"] = inputs;
    file
}

/// One root input port of a Task's recorded revision.
fn revision_input(cas: &Cas, done: &Value, port: &str) -> Value {
    let revision = cas.get_json(done["revision_id"].as_str().unwrap()).unwrap();
    revision["payload"]["inputs"][port].clone()
}

/// The context record bound to the one Attempt reserved for `node`.
fn context(cas: &Cas, done: &Value, node: &str) -> Value {
    let records: Vec<&Value> = done["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["record"])
        .collect();
    let reserved = records
        .iter()
        .find(|record| {
            record["kind"] == "reserved"
                && cas
                    .get_json(record["invocation_id"].as_str().unwrap())
                    .unwrap()["payload"]["node"]
                    == node
        })
        .unwrap_or_else(|| panic!("no Attempt for {node}"));
    let bound = records
        .iter()
        .find(|record| {
            record["kind"] == "context_bound" && record["attempt_id"] == reserved["attempt_id"]
        })
        .unwrap();
    cas.get_json(bound["context_id"].as_str().unwrap()).unwrap()["payload"].clone()
}

/// The exact artifacts a context manifest delivered on one declared input port, in order.
fn delivered(context: &Value, port: &str) -> Vec<String> {
    context["manifest"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["required_by"] == "declared input port" && entry["name"] == port)
        .map(|entry| entry["artifact_id"].as_str().unwrap().to_owned())
        .collect()
}

fn ids(port: &Value) -> Vec<String> {
    port["artifact_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
}

/// No Task is recorded under `task_id`: a plan-time refusal leaves nothing behind.
fn unrecorded(chain: &Chain, task_id: &str) {
    let shown = chain.af(&["task", "show", task_id]);
    assert!(!shown.status.success(), "{}", text(&shown));
    assert!(text(&shown).contains("was not found"), "{}", text(&shown));
}

#[test]
fn a_report_binds_an_experiments_comparison_measurements_and_tree_and_runs_to_verified() {
    let chain = chain();

    // 1. The experiment: a baseline and a candidate, each measured on its own Snapshot.
    let experiment = chain.start("experiment.json", 0);
    assert_eq!(experiment["result"]["domain_conclusion"], "verified");
    let outputs = &experiment["result"]["outputs"];
    let (baseline, candidate) = (&outputs["baseline"], &outputs["candidate"]);
    assert_ne!(baseline["snapshot_id"], candidate["snapshot_id"]);
    let measured = [ids(baseline), ids(candidate)].concat();

    // 2. A report over the experiment's comparison, both Measurements and its candidate tree.
    let inputs = json!({
        "comparison": {"task": "experiment", "port": "comparison"},
        "measurements": [
            {"task": "experiment", "port": "baseline"},
            {"task": "experiment", "port": "candidate"}
        ],
        "source": {"task": "experiment", "port": "snapshot"}
    });
    let file = chain.task_file("research", &report_task(&chain, "research", inputs));
    let planned = chain.json(&["task", "plan", "--file", &file], 0);
    assert_eq!(planned["attempts"], 0);
    let ports: Vec<&String> = planned["plan"]["inputs"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(
        ports,
        [
            "comparison",
            "measurements",
            "requirements",
            "source",
            "sources"
        ]
    );
    let explained = chain.af(&["task", "explain", "research", "--tree"]);
    assert!(explained.status.success(), "{}", text(&explained));
    let explained = String::from_utf8_lossy(&explained.stdout);
    for row in [
        "BOUND comparison <- task experiment/comparison (satisfied/verified)".to_owned(),
        "BOUND measurements <- task experiment/baseline (satisfied/verified)".to_owned(),
        format!("      {}", measured[0]),
        "BOUND measurements <- task experiment/candidate (satisfied/verified)".to_owned(),
        format!("      {}", measured[1]),
        "BOUND source <- task experiment/snapshot (satisfied/verified)".to_owned(),
    ] {
        assert!(explained.contains(&row), "{row}\n{explained}");
    }

    let done = chain.json(&["task", "run", "--execute", "research"], 0);
    schemas::valid(&schemas::validator("task-inspection-v11.json"), &done);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    assert_eq!(done["result"]["domain_conclusion"], "verified");
    assert_eq!(done["attempts"], 3, "author, checks and verifier");

    // The ports: `comparison` keeps its one output's Snapshot; `measurements` gathers both
    // Measurements in Task-file order and names no Snapshot, because they measured two.
    let cas = chain.cas();
    let comparison = revision_input(&cas, &done, "comparison");
    assert_eq!(comparison, outputs["comparison"]);
    let measurements = revision_input(&cas, &done, "measurements");
    assert_eq!(ids(&measurements), measured);
    assert_eq!(measurements["cardinality"], "many");
    assert!(measurements.get("snapshot_id").is_none(), "{measurements}");
    // Each Measurement keeps its own Snapshot, in its envelope and in its payload.
    for (id, output) in measured.iter().zip([baseline, candidate]) {
        let envelope = cas.get_json(id).unwrap();
        assert_eq!(envelope["type"], "af/Measurement@1");
        assert_eq!(envelope["subject_snapshot_id"], output["snapshot_id"]);
        assert_eq!(envelope["payload"]["snapshot_id"], output["snapshot_id"]);
    }

    // The author and the verifier each received the exact comparison and both Measurements,
    // and the rendered request shows every Measurement with its own Snapshot.
    for node in ["root.nodes.author", "root.nodes.verify"] {
        let context = context(&cas, &done, node);
        assert_eq!(
            delivered(&context, "comparison"),
            ids(&comparison),
            "{node}"
        );
        assert_eq!(delivered(&context, "measurements"), measured, "{node}");
        let rendered = cas.get(context["rendered_id"].as_str().unwrap()).unwrap();
        let request: Value = serde_json::from_slice(&rendered).unwrap();
        let shown: Vec<&Value> = request["inputs"]["measurements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| &value["snapshot_id"])
            .collect();
        assert_eq!(shown, [&baseline["snapshot_id"], &candidate["snapshot_id"]]);
    }

    // The durable record keeps every binding; `af task show` prints one line per output.
    let bindings = &done["input_bindings"]["bindings"];
    schemas::valid(
        &schemas::validator("task-input-bindings-v1.json"),
        &done["input_bindings"],
    );
    assert_eq!(
        bindings["measurements"]["artifact_id"],
        measured[0].as_str()
    );
    assert_eq!(bindings["measurements"]["task"]["port"], "baseline");
    let also = &bindings["measurements"]["also"];
    assert_eq!(also.as_array().unwrap().len(), 1, "{also}");
    assert_eq!(also[0]["artifact_id"], measured[1].as_str());
    assert_eq!(also[0]["snapshot_id"], candidate["snapshot_id"]);
    assert_eq!(also[0]["task"]["port"], "candidate");
    assert_eq!(
        bindings["comparison"]["snapshot_id"],
        outputs["comparison"]["snapshot_id"]
    );
    assert!(bindings["comparison"].get("also").is_none());
    let shown = chain.af(&["task", "show", "research"]);
    assert!(shown.status.success(), "{}", text(&shown));
    let shown = String::from_utf8_lossy(&shown.stdout);
    for line in [
        "bound comparison <- task experiment/comparison (satisfied)",
        "bound measurements <- task experiment/baseline (satisfied)",
        "bound measurements <- task experiment/candidate (satisfied)",
        "bound source <- task experiment/snapshot (satisfied)",
    ] {
        assert!(shown.contains(line), "{line}\n{shown}");
    }

    // 3. A Document Task's `document` is not a report's `sources`: refused at plan time, naming
    // the port and both types, and nothing is recorded.
    let notes = chain.start("document.json", 0);
    assert_eq!(notes["result"]["domain_conclusion"], "verified");
    let inputs = json!({"sources": {"task": "release-notes", "port": "document"}});
    let file = chain.task_file("from-notes", &report_task(&chain, "from-notes", inputs));
    let refused = chain.json(&["task", "plan", "--file", &file], 1);
    assert_eq!(refused["schema"], "af/error@1");
    let error = refused["error"].as_str().unwrap();
    assert!(
        error.contains(
            "Task input binding sources <- task release-notes/document: the reference is \
             af/Document@1 one and this port takes af/ReportSources@1 one"
        ),
        "{error}"
    );
    unrecorded(&chain, "from-notes");
}

/// Every refusal ADR-0134 adds runs while the revision is still being built, so it names the
/// port and the reason before any Worker is dispatched or any Provider admitted, and leaves no
/// Task behind.
#[test]
fn every_widened_binding_refusal_names_its_port_before_anything_is_recorded() {
    let chain = chain();
    let experiment = chain.start("experiment.json", 0);
    let comparison = experiment["result"]["outputs"]["comparison"]["artifact_ids"][0].clone();
    let cases = [
        (
            json!({"comparison": [
                {"task": "experiment", "port": "comparison"},
                {"task": "experiment", "port": "comparison"}
            ]}),
            "comparison <- task experiment/comparison, task experiment/comparison: a list of \
             outputs binds only a many port, and this port takes af/MeasurementComparison@1 one",
        ),
        // A listed name that is not a result output is refused for that, not for the list.
        (
            json!({"comparison": [{"task": "experiment", "port": "raw_artifact_ids"}]}),
            "comparison <- task experiment/raw_artifact_ids: the recorded result has no output \
             port raw_artifact_ids; only result outputs bind",
        ),
        (
            json!({"measurements": {"task": "experiment", "port": "baseline"}}),
            "measurements <- task experiment/baseline: the reference is af/Measurement@1 one and \
             this port takes af/Measurement@1 many",
        ),
        (
            json!({"measurements": [
                {"task": "experiment", "port": "baseline"},
                {"task": "experiment", "port": "comparison"}
            ]}),
            "measurements <- task experiment/comparison: the reference is \
             af/MeasurementComparison@1 one and this port takes af/Measurement@1 many",
        ),
        (
            json!({"comparison": {"artifact": comparison}}),
            "only a recorded Task's result output binds this port, never an exact artifact",
        ),
        (
            json!({"comparison": {"task": "experiment", "port": "raw_artifact_ids"}}),
            "comparison <- task experiment/raw_artifact_ids: the recorded result has no output \
             port raw_artifact_ids; only result outputs bind, never an Attempt's raw artifacts, \
             runtime evidence or other records",
        ),
        (
            json!({"baseline": {"task": "experiment", "port": "baseline"}}),
            "baseline <- task experiment/baseline: baseline is not a bindable root input port: \
             the selected Pipeline builtin/report does not declare it",
        ),
        (
            json!({"requirements": {"task": "experiment", "port": "comparison"}}),
            "requirements <- task experiment/comparison: the root input requirements is not \
             bindable",
        ),
    ];
    for (index, (inputs, expected)) in cases.into_iter().enumerate() {
        let task_id = format!("refused-{index}");
        let file = chain.task_file(&task_id, &report_task(&chain, &task_id, inputs));
        let refused = chain.json(&["task", "plan", "--file", &file], 1);
        let error = refused["error"].as_str().unwrap();
        assert!(error.contains(expected), "{expected}\n{error}");
        unrecorded(&chain, &task_id);
    }
}
