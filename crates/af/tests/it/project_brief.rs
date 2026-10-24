//! ADR-0160 end to end: a Project Brief on a declared `project` port, through the real `af`.
//!
//! Every test copies `fixtures/task-runtime/project-brief`, whose `fixture/oriented` Pipeline
//! wires the port to its implementer and not to its evaluator, pins its package digests, commits
//! it, and gives `af` a state home of its own, so no other test's Project Store or Survey is
//! seen. Task files live outside the checkout, as a Workspace keeps them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use review_store::Cas;
use serde_json::{Value, json};

use crate::{schemas, task_cli};

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

impl Fixture {
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// The fixture after `edit` changed its checkout, pinned and committed.
    fn with(edit: impl FnOnce(&Path)) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = task_cli::fixture_named(root.path(), "project-brief");
        edit(&repo);
        pin_packages(&repo);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "pin packages"]);
        std::fs::create_dir_all(root.path().join("tasks")).unwrap();
        Self { root, repo, state }
    }

    fn state_home(&self) -> PathBuf {
        self.root.path().join("xdg-state")
    }

    /// `af` in `directory`, with this fixture's own state home.
    fn af_in(&self, directory: &Path, args: &[&str]) -> Output {
        crate::common::af()
            .current_dir(directory)
            .env("XDG_STATE_HOME", self.state_home())
            .args(args)
            .output()
            .unwrap()
    }

    fn af(&self, args: &[&str]) -> Output {
        self.af_in(&self.repo, args)
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.af(args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}\n{}",
            shown(&output)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// A Task command with `--state`, its `--json` document.
    fn task(&self, args: &[&str], code: i32) -> Value {
        let mut args = args.to_vec();
        args.extend(["--json", "--state", self.state.to_str().unwrap()]);
        let output = self.af(&args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}\n{}",
            shown(&output)
        );
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }

    /// A Task command with `--state`, its text.
    fn task_text(&self, args: &[&str]) -> String {
        let mut args = args.to_vec();
        args.extend(["--state", self.state.to_str().unwrap()]);
        self.ok(&args)
    }

    /// Write the Task file `task_id` outside the checkout: `oriented.json` on `pipeline`, with
    /// `inputs` when given.
    fn task_file(&self, task_id: &str, pipeline: &str, inputs: Option<Value>) -> String {
        let mut file: Value =
            serde_json::from_slice(&std::fs::read(self.repo.join("oriented.json")).unwrap())
                .unwrap();
        file["task_id"] = json!(task_id);
        file["pipeline"]["name"] = json!(pipeline);
        if let Some(inputs) = inputs {
            file["inputs"] = inputs;
        }
        let path = self
            .root
            .path()
            .join("tasks")
            .join(format!("{task_id}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn refresh(&self) {
        self.ok(&["project", "refresh"]);
    }

    fn commit(&self, name: &str) -> String {
        std::fs::write(self.repo.join(name), format!("{name}\n")).unwrap();
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-qm", name]);
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn cas(&self) -> Cas {
        Cas::open_existing(self.state.join("cas")).unwrap()
    }

    /// The `af/ProjectBrief@1` the revision of `done` carries on `project`, if any.
    fn port(&self, done: &Value) -> Option<String> {
        let revision = self
            .cas()
            .get_json(done["revision_id"].as_str().unwrap())
            .unwrap();
        revision["payload"]["inputs"]["project"]["artifact_ids"][0]
            .as_str()
            .map(str::to_owned)
    }

    fn preview(&self, args: &[&str]) -> Value {
        let mut all = vec!["project", "brief"];
        all.extend_from_slice(args);
        all.push("--json");
        serde_json::from_str(&self.ok(&all)).unwrap()
    }

    fn write_policy(&self, policy: &str) {
        let text = format!(
            "version = 1\n\n[project]\nname = \"fixture\"\nmin_af = \"0.2\"\n\n[defaults]\npipeline = \"p1-review\"\n\n{policy}"
        );
        std::fs::write(self.repo.join(".af/af.toml"), text).unwrap();
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-qm", "brief policy"]);
    }
}

/// Pin every package digest of the catalog at `repo` from its tree.
fn pin_packages(repo: &Path) {
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        let package = repo.join(pin["path"].as_str().unwrap());
        let digest = review_config::lock::package_digest(name, &package).unwrap();
        pin["digest"] = toml::Value::String(digest);
    }
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
}

fn shown(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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

/// The manifest entries a context lists for `port`.
fn entries(context: &Value, port: &str) -> Vec<Value> {
    context["manifest"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["name"] == port)
        .cloned()
        .collect()
}

/// The plan a text preview names.
fn plan_id(preview: &str) -> String {
    preview
        .lines()
        .find_map(|line| line.strip_prefix("PLAN  "))
        .unwrap()
        .trim()
        .to_owned()
}

/// A preview without its clock-dependent `TIME` row.
fn timeless(preview: &str) -> String {
    preview
        .lines()
        .filter(|line| !line.starts_with("TIME  "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The one Brief port line of a plan or explain preview.
fn port_line(preview: &str) -> String {
    let lines: Vec<&str> = preview
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("project: "))
        .collect();
    assert_eq!(lines.len(), 1, "{preview}");
    lines[0].to_owned()
}

/// The Task's Brief receipts, read from its revision's provenance.
fn receipts(cas: &Cas, revision_id: &str) -> Vec<Value> {
    let revision = cas.get_json(revision_id).unwrap();
    revision["payload"]["provenance"]["input_artifact_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|id| cas.get_json(id.as_str().unwrap()).ok())
        .filter(|envelope| envelope["type"] == "af/ProjectBriefReceipt@1")
        .map(|envelope| envelope["payload"].clone())
        .collect()
}

/// (a), (b), (f), (h): the wired Worker alone sees the Brief, `af project brief` printed its
/// exact bytes, and a newer Survey between plan and run changes nothing.
#[test]
fn the_wired_worker_alone_receives_the_pinned_brief_and_a_refresh_changes_nothing() {
    let f = Fixture::new();
    f.refresh();
    let head = git(&f.repo, &["rev-parse", "HEAD"]);
    let text = f.ok(&["project", "brief", "--variant", "orient"]);
    let preview = f.preview(&["--variant", "orient"]);
    schemas::valid(
        &schemas::validator("project-brief-preview-v1.json"),
        &preview,
    );
    let brief_id = preview["brief_id"].as_str().unwrap().to_owned();
    // The document's Brief is byte for byte the text form.
    let canonical = review_store::canonicalize(&preview["brief"]).unwrap();
    assert_eq!(format!("{}\n", String::from_utf8(canonical).unwrap()), text);
    assert_eq!(preview["manifest"]["required_by"], "port:project");

    let file = f.task_file("oriented", "fixture/oriented", None);
    let planned = f.task_text(&["task", "plan", "--file", &file]);
    let line = port_line(&planned);
    let bytes = preview["brief"]["rendered_bytes"].as_str().unwrap();
    let tokens = preview["manifest"]["estimated_tokens"].as_str().unwrap();
    assert_eq!(
        line,
        format!(
            "project: af/ProjectBrief@1 {brief_id} (orient, {bytes} B, ~{tokens} tokens, survey {}, behind 0)",
            &head[..12]
        )
    );
    let plan_id = plan_id(&planned);

    // A new commit and a newer Survey before the plan runs.
    f.commit("later.txt");
    f.refresh();
    let newer = f.preview(&["--variant", "orient"]);
    assert_ne!(newer["brief_id"], preview["brief_id"]);

    let done = f.task(&["task", "run", "oriented", "--confirm-plan", &plan_id], 0);
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    let cas = f.cas();
    assert_eq!(f.port(&done).as_deref(), Some(brief_id.as_str()));
    let stored = cas.get_json(&brief_id).unwrap();
    assert_eq!(stored["type"], "af/ProjectBrief@1");
    let payload = review_store::canonicalize(&stored["payload"]).unwrap();
    assert_eq!(format!("{}\n", String::from_utf8(payload).unwrap()), text);

    let implement = context(&cas, &done, "root.nodes.implement");
    let project = entries(&implement, "project");
    assert_eq!(project.len(), 1, "{implement}");
    assert_eq!(project[0]["required_by"], "port:project");
    assert_eq!(project[0]["artifact_id"], brief_id.as_str());
    assert_eq!(project[0]["artifact_type"], "af/ProjectBrief@1");
    assert_eq!(
        project[0]["rendered_bytes"].as_u64().unwrap().to_string(),
        preview["manifest"]["rendered_bytes"].as_str().unwrap()
    );
    let evaluate = context(&cas, &done, "root.nodes.evaluate");
    assert!(entries(&evaluate, "project").is_empty(), "{evaluate}");

    // Explain prints the captured line, unchanged by the newer Survey.
    let explained = f.task_text(&["task", "explain", "oriented"]);
    assert_eq!(port_line(&explained), line);
}

/// (d), (n): without a Survey the port is empty and the Task runs; variant `none` constructs
/// nothing with or without one.
#[test]
fn an_empty_port_says_why_and_the_task_still_runs() {
    let f = Fixture::new();
    let file = f.task_file("empty", "fixture/oriented", None);
    let planned = f.task_text(&["task", "plan", "--file", &file]);
    assert_eq!(
        port_line(&planned),
        "project: none (no Survey recorded; run af project refresh)"
    );
    let failed = f.af(&["project", "brief"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        shown(&failed).contains("none (no Survey recorded; run af project refresh)"),
        "{}",
        shown(&failed)
    );
    let preview = f.preview(&[]);
    assert_eq!(
        preview,
        json!({"outcome":"none","reason":"no_survey","schema":"af/project-brief-preview@1","variant":"orient"})
    );
    schemas::valid(
        &schemas::validator("project-brief-preview-v1.json"),
        &preview,
    );

    let done = f.task(
        &["task", "run", "empty", "--confirm-plan", &plan_id(&planned)],
        0,
    );
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    assert_eq!(f.port(&done), None);
    let implement = context(&f.cas(), &done, "root.nodes.implement");
    assert!(entries(&implement, "project").is_empty());

    // Variant `none`, with and without a Survey.
    let none = Fixture::with(|repo| {
        let path = repo.join(".af/task-packages/fixture/oriented/pipeline.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("brief = \"orient\"", "brief = \"none\""),
        )
        .unwrap();
    });
    for surveyed in [false, true] {
        if surveyed {
            none.refresh();
        }
        let id = format!("none-{surveyed}");
        let file = none.task_file(&id, "fixture/oriented", None);
        let planned = none.task_text(&["task", "plan", "--file", &file]);
        assert_eq!(port_line(&planned), "project: none (variant none)");
        let done = none.task(
            &["task", "run", &id, "--confirm-plan", &plan_id(&planned)],
            0,
        );
        assert_eq!(none.port(&done), None);
        let implement = context(&none.cas(), &done, "root.nodes.implement");
        assert!(entries(&implement, "project").is_empty());
    }
    let preview = none.preview(&["--variant", "none"]);
    assert_eq!(preview["outcome"], "none");
    assert_eq!(preview["reason"], "variant_none");

    // A binding still wins over `none`: the port carries the recorded Brief.
    let file = none.task_file("recorded", "fixture/exposing", None);
    let recorded = none.task(&["task", "start", "--execute", "--file", &file], 0);
    let brief = recorded["result"]["outputs"]["brief"]["artifact_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let bound = json!({"project": {"task": "recorded", "port": "brief"}});
    let file = none.task_file("bound-over-none", "fixture/oriented", Some(bound));
    let planned = none.task_text(&["task", "plan", "--file", &file]);
    assert!(
        port_line(&planned).starts_with(&format!(
            "project: af/ProjectBrief@1 {brief} (bound, orient, "
        )),
        "{planned}"
    );
    let done = none.task(
        &[
            "task",
            "run",
            "bound-over-none",
            "--confirm-plan",
            &plan_id(&planned),
        ],
        0,
    );
    assert_eq!(none.port(&done).as_deref(), Some(brief.as_str()));
    let implement = context(&none.cas(), &done, "root.nodes.implement");
    assert_eq!(
        entries(&implement, "project")[0]["artifact_id"],
        brief.as_str()
    );
}

/// (e): `behind_commits` counts the commits from the Survey head to the source commit, and is
/// `unknown` when the head is no ancestor of it.
#[test]
fn staleness_counts_commits_since_the_survey_head_or_is_unknown() {
    let f = Fixture::new();
    f.refresh();
    for name in ["one", "two", "three"] {
        f.commit(name);
    }
    let file = f.task_file("behind", "fixture/oriented", None);
    let planned = f.task_text(&["task", "plan", "--file", &file]);
    assert!(port_line(&planned).ends_with("behind 3)"), "{planned}");

    // A Survey of a divergent branch is no ancestor of the Task's source.
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["checkout", "-q", "-b", "side", "HEAD~3"]);
    f.commit("side.txt");
    f.refresh();
    git(&f.repo, &["checkout", "-q", "main"]);
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), base);
    let file = f.task_file("diverged", "fixture/oriented", None);
    let planned = f.task(&["task", "plan", "--file", &file], 0);
    let explained = f.task_text(&["task", "explain", "diverged"]);
    assert!(
        port_line(&explained).ends_with("behind unknown)"),
        "{explained}"
    );
    let receipt = &receipts(&f.cas(), planned["revision_id"].as_str().unwrap())[0];
    assert_eq!(
        receipt["staleness"],
        json!({"kind":"unknown","reason":"not_ancestor"})
    );
    assert_eq!(receipt["source_commit"], base.as_str());
}

/// (c), (p), (r): the drop rule end to end, and every policy refusal at load.
#[test]
fn a_variant_over_max_bytes_drops_its_last_section_and_bad_policy_is_refused_at_load() {
    let f = Fixture::new();
    f.refresh();
    let full = f.preview(&[]);
    let size: u64 = full["brief"]["rendered_bytes"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(size >= 1024, "the fixture's orient Brief is {size} bytes");

    // Exactly at the boundary nothing is dropped; one byte under drops `hot_paths`, orient's
    // last-declared section, and records why.
    f.write_policy(&format!("[brief.orient]\nmax_bytes = {size}\n"));
    let at = f.preview(&[]);
    assert_eq!(at["brief"]["dropped"], json!([]));
    f.write_policy(&format!("[brief.orient]\nmax_bytes = {}\n", size - 1));
    let under = f.preview(&[]);
    assert_eq!(
        under["brief"]["dropped"],
        json!([{"reason":"max_bytes","section":"hot_paths"}])
    );
    let fits: u64 = under["brief"]["rendered_bytes"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(fits < size);
    assert_eq!(
        review_store::canonicalize(&under["brief"]).unwrap().len() as u64,
        fits
    );
    // The plan binds the same dropped Brief.
    let file = f.task_file("dropped", "fixture/oriented", None);
    let planned = f.task(&["task", "plan", "--file", &file], 0);
    assert_eq!(f.port(&planned).as_deref(), under["brief_id"].as_str());

    for (index, (policy, needle)) in [
        (
            "[brief.orient]\nmax_bytes = 1023\n",
            "[brief.orient] max_bytes: 1023",
        ),
        (
            "[brief.slim]\nsections = [\"tree\"]\nmax_bytes = 262145\n",
            "[brief.slim] max_bytes: 262145",
        ),
        (
            "[brief.slim]\nsections = [\"tree\", \"owners\"]\n",
            "[brief.slim] sections: `owners`",
        ),
        (
            "[brief.slim]\nsections = [\"tree\", \"tree\"]\n",
            "[brief.slim] sections: `tree` is listed twice",
        ),
        (
            "[brief.slim]\nsections = [\"tree\"]\ncolour = 1\n",
            "unknown field `colour`",
        ),
        (
            "[brief.slim]\nsections = [\"tree\"]\ndecisions = 1001\n",
            "[brief.slim] decisions",
        ),
        ("[brief.none]\nsections = [\"tree\"]\n", "[brief.none] name"),
        (
            "[brief.orient]\nsections = [\"tree\", \"contributors\"]\n",
            "[brief.orient] sections",
        ),
        (
            "[brief.orient]\nsections = [\"tree\", \"activity\"]\n",
            "[brief.orient] sections",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        f.write_policy(policy);
        let file = f.task_file(&format!("refused-{index}"), "fixture/oriented", None);
        for args in [
            vec!["project", "brief"],
            vec![
                "task",
                "plan",
                "--file",
                &file,
                "--state",
                f.state.to_str().unwrap(),
            ],
        ] {
            let refused = f.af(&args);
            assert_eq!(
                refused.status.code(),
                Some(1),
                "{policy}\n{}",
                shown(&refused)
            );
            let said = shown(&refused);
            assert!(
                said.contains("the [brief] policy does not load"),
                "{policy}\n{said}"
            );
            assert!(said.contains(needle), "`{needle}`\n{said}");
        }
    }

    // A port asking for a variant nothing defines fails the plan, naming the node and variant.
    let unknown = Fixture::with(|repo| {
        let path = repo.join(".af/task-packages/fixture/oriented/pipeline.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("brief = \"orient\"", "brief = \"slim\""),
        )
        .unwrap();
    });
    unknown.refresh();
    let file = unknown.task_file("unknown", "fixture/oriented", None);
    let refused = unknown.af(&[
        "task",
        "plan",
        "--file",
        &file,
        "--state",
        unknown.state.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1), "{}", shown(&refused));
    let said = shown(&refused);
    assert!(
        said.contains("wired to node implement") && said.contains("requests variant slim"),
        "{said}"
    );
}

/// (k), (g), (l), (s): a Task file binds the port to a recorded Brief output, which suppresses
/// construction; the exact-artifact form is refused; one Brief bound at two source commits gets
/// two receipts; a Worker-authored Brief is refused at output admission.
#[test]
fn a_bound_brief_carries_the_recorded_one_and_is_receipted_per_task() {
    let f = Fixture::new();
    f.refresh();
    let file = f.task_file("first", "fixture/exposing", None);
    let first = f.task(&["task", "start", "--execute", "--file", &file], 0);
    assert_eq!(first["result"]["acceptance"], "satisfied", "{first}");
    let recorded = first["result"]["outputs"]["brief"].clone();
    assert_eq!(recorded["artifact_type"], "af/ProjectBrief@1");
    let brief = recorded["artifact_ids"][0].as_str().unwrap().to_owned();
    // Forwarded unchanged: the public output is the constructed input, same ID.
    assert_eq!(f.port(&first).as_deref(), Some(brief.as_str()));

    // A Project Store without a Survey: nothing could be constructed, so the bound Brief is
    // the only one there is.
    let store = f.state_home().join("af/project");
    std::fs::remove_dir_all(&store).unwrap();
    let bound = json!({"project": {"task": "first", "port": "brief"}});
    let mut seen = Vec::new();
    for (id, commit) in [("second", "c1"), ("third", "c2")] {
        f.commit(commit);
        let file = f.task_file(id, "fixture/oriented", Some(bound.clone()));
        let planned = f.task_text(&["task", "plan", "--file", &file]);
        assert!(planned.contains("project <- task first/brief"), "{planned}");
        let line = port_line(&planned);
        assert!(
            line.starts_with(&format!(
                "project: af/ProjectBrief@1 {brief} (bound, orient, "
            )),
            "{line}"
        );
        let done = f.task(
            &["task", "run", id, "--confirm-plan", &plan_id(&planned)],
            0,
        );
        assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
        assert_eq!(f.port(&done).as_deref(), Some(brief.as_str()));
        let cas = f.cas();
        let implement = context(&cas, &done, "root.nodes.implement");
        assert_eq!(
            entries(&implement, "project")[0]["artifact_id"],
            brief.as_str()
        );
        let receipt = receipts(&cas, done["revision_id"].as_str().unwrap()).remove(0);
        assert_eq!(receipt["brief_id"], brief.as_str());
        assert_eq!(receipt["outcome"], "bound");
        seen.push(receipt);
    }
    assert_eq!(
        seen[0]["staleness"],
        json!({"kind":"behind","behind_commits":"1"})
    );
    assert_eq!(
        seen[1]["staleness"],
        json!({"kind":"behind","behind_commits":"2"})
    );
    assert_ne!(seen[0]["source_commit"], seen[1]["source_commit"]);
    assert!(f.cas().get_json(&brief).is_ok());

    // The exact-artifact form is refused with ADR-0134's message.
    let exact = json!({"project": {"artifact": brief}});
    let file = f.task_file("exact", "fixture/oriented", Some(exact));
    let refused = f.af(&[
        "task",
        "plan",
        "--file",
        &file,
        "--state",
        f.state.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        shown(&refused).contains(
            "only a recorded Task's result output binds this port, never an exact artifact"
        ),
        "{}",
        shown(&refused)
    );

    // A Worker that returns the Brief it was handed as its own output is refused at admission.
    f.refresh();
    let file = f.task_file("forged", "fixture/forged", None);
    let forged = f.task(&["task", "start", "--execute", "--file", &file], 6);
    assert_eq!(forged["result"]["acceptance"], "inconclusive", "{forged}");
    let refused = &forged["run_reports"][0]["diagnostics"]["root.nodes.implement"]["message"];
    assert!(
        refused
            .as_str()
            .unwrap()
            .starts_with("not_kernel_constructed: "),
        "{forged}"
    );
    let cas = f.cas();
    let settled = forged["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["record"]["kind"] == "settled")
        .unwrap();
    let feedback = settled["record"]["result"]["feedback_id"].as_str().unwrap();
    let feedback = cas.get_json(feedback).unwrap();
    assert_eq!(
        feedback["payload"]["code"], "output_admission_rejected",
        "{feedback}"
    );
    assert!(
        forged["result"]["outputs"].get("brief").is_none(),
        "{forged}"
    );
}

impl Fixture {
    /// A second repository beside the fixture's: the same packages under its own root commit,
    /// so its own Project, sharing this fixture's state home and Task Store.
    fn other_repository(&self) -> PathBuf {
        let (repo, _) = task_cli::fixture_named(&self.root.path().join("other"), "project-brief");
        std::fs::remove_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("other.txt"), "another repository\n").unwrap();
        pin_packages(&repo);
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Other"]);
        git(&repo, &["config", "user.email", "other@example.invalid"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "other"]);
        repo
    }
}

/// The automatic Brief is the Project's of the resolved source Snapshot (ADR-0160 clause 4): a
/// Task planned in repository A whose `source` is bound to a tree of repository B never carries
/// A's Survey. The port is empty while B has no complete Survey, although A has one, and once B
/// is surveyed it carries B's Brief, whose staleness is unknown: the re-rooted tree is no commit.
#[test]
fn a_bound_source_of_another_repository_pins_that_repositorys_survey_or_nothing() {
    let a = Fixture::new();
    a.refresh();
    let ours = a.preview(&[]);
    let b = a.other_repository();
    let state = a.state.to_str().unwrap().to_owned();
    let in_b = a.task_file("in-b", "fixture/oriented", None);
    let ran = a.af_in(
        &b,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            &in_b,
            "--json",
            "--state",
            &state,
        ],
    );
    assert_eq!(ran.status.code(), Some(0), "{}", shown(&ran));
    let ran: Value = serde_json::from_slice(&ran.stdout).unwrap();
    assert_eq!(ran["result"]["acceptance"], "satisfied", "{ran}");
    assert_eq!(a.port(&ran), None, "B has no Survey yet");

    let bound = json!({"source": {"task": "in-b", "port": "snapshot"}});
    let file = a.task_file("from-b", "fixture/oriented", Some(bound.clone()));
    let planned = a.task(&["task", "plan", "--file", &file], 0);
    assert_eq!(a.port(&planned), None, "A's Survey is never selected");
    let explained = a.task_text(&["task", "explain", "from-b"]);
    assert!(
        explained.contains("source <- task in-b/snapshot"),
        "{explained}"
    );
    assert_eq!(
        port_line(&explained),
        "project: none (no Survey recorded; run af project refresh)"
    );
    let receipt = receipts(&a.cas(), planned["revision_id"].as_str().unwrap()).remove(0);
    assert_eq!(receipt["outcome"], "none");
    assert_eq!(receipt["reason"], "no_survey");

    let surveyed = a.af_in(&b, &["project", "refresh"]);
    assert_eq!(surveyed.status.code(), Some(0), "{}", shown(&surveyed));
    let theirs = a.af_in(&b, &["project", "brief", "--json"]);
    assert_eq!(theirs.status.code(), Some(0), "{}", shown(&theirs));
    let theirs: Value = serde_json::from_slice(&theirs.stdout).unwrap();
    assert_ne!(theirs["brief"]["project_id"], ours["brief"]["project_id"]);

    let file = a.task_file("from-b-surveyed", "fixture/oriented", Some(bound));
    let planned = a.task(&["task", "plan", "--file", &file], 0);
    let carried = a.port(&planned).unwrap();
    assert_eq!(theirs["brief_id"], carried.as_str());
    assert_ne!(ours["brief_id"], carried.as_str());
    let brief = a.cas().get_json(&carried).unwrap();
    assert_eq!(
        brief["payload"]["project_id"],
        theirs["brief"]["project_id"]
    );
    let receipt = receipts(&a.cas(), planned["revision_id"].as_str().unwrap()).remove(0);
    assert_eq!(receipt["outcome"], "constructed");
    assert_eq!(
        receipt["staleness"],
        json!({"kind":"unknown","reason":"no_committed_revision"})
    );
    let explained = a.task_text(&["task", "explain", "from-b-surveyed"]);
    assert!(
        port_line(&explained).ends_with("behind unknown)"),
        "{explained}"
    );
}

/// ADR-0159 clause 6 holds in a Brief: an author's email never enters it, while repository text
/// enters verbatim, `@` included. Under both identity policies the decision title keeps the
/// address it was committed with, the author whose name is an email address is the sanitized
/// identity `h:` and the hash of that name, and neither author's email appears.
#[test]
fn a_brief_keeps_an_email_in_a_decision_title_and_sanitizes_an_email_shaped_author() {
    use sha2::Digest as _;
    const TITLE: &str = "ADR-0001: Send key requests to keys@example.com";
    let f = Fixture::with(|repo| {
        let adr = repo.join("docs/adr");
        std::fs::create_dir_all(&adr).unwrap();
        std::fs::write(
            adr.join("0001-key-requests.md"),
            format!("# {TITLE}\n\nStatus: accepted on 2026-10-01.\n"),
        )
        .unwrap();
        let path = repo.join(".af/task-packages/fixture/oriented/pipeline.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("brief = \"orient\"", "brief = \"people\""),
        )
        .unwrap();
    });
    let author = "maintainer@example.net";
    f.commit_by(author, author, "one.txt");
    f.commit_by("Ada", "ada@example.org", "two.txt");
    let hashed = format!(
        "h:{}",
        &review_core::hex::encode(&sha2::Sha256::digest(author.as_bytes()))[..16]
    );
    for identity in ["name", "email_hash"] {
        f.write_policy(&format!(
            "[survey]\nidentity = \"{identity}\"\n\n[brief.people]\nsections = \
             [\"contributors\", \"decisions\"]\nmax_bytes = 262144\n"
        ));
        f.refresh();
        let preview = f.preview(&["--variant", "people"]);
        let brief = &preview["brief"];
        let titles: Vec<&str> = brief["sections"]["decisions"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|record| record["title"].as_str())
            .collect();
        assert_eq!(titles, [TITLE], "{identity}: {brief}");
        let contributors = brief["sections"]["contributors"]["by_identity"]
            .as_array()
            .unwrap();
        let sanitized = contributors
            .iter()
            .find(|contributor| contributor["key"] == hashed.as_str())
            .unwrap_or_else(|| panic!("{identity}: no {hashed} in {contributors:?}"));
        if identity == "name" {
            assert_eq!(sanitized["name"], hashed.as_str(), "{sanitized}");
        } else {
            assert!(sanitized.get("name").is_none(), "{sanitized}");
        }
        // The Brief the plan binds is the preview's, byte for byte.
        let file = f.task_file(&format!("people-{identity}"), "fixture/oriented", None);
        let planned = f.task(&["task", "plan", "--file", &file], 0);
        let bound = f.port(&planned).unwrap();
        assert_eq!(preview["brief_id"], bound.as_str());
        let stored = f.cas().get_json(&bound).unwrap()["payload"].to_string();
        let text = brief.to_string();
        for document in [&text, &stored] {
            assert!(document.contains("keys@example.com"), "{identity}");
            for email in [author, "ada@example.org", "example.invalid"] {
                assert!(
                    !document.contains(email),
                    "{identity}: {email} in {document}"
                );
            }
        }
    }
}

/// (j): `af project brief` and a plan with the port leave nothing in the checkout.
#[test]
fn the_brief_writes_nothing_inside_the_checkout() {
    let f = Fixture::new();
    f.refresh();
    f.ok(&["project", "brief"]);
    f.ok(&["project", "brief", "--json"]);
    let file = f.task_file("clean", "fixture/oriented", None);
    f.task_text(&["task", "plan", "--file", &file]);
    assert_eq!(git(&f.repo, &["status", "--porcelain", "--ignored"]), "");
}

/// (q): collecting earlier Tasks releases their pinned Surveys while a later Task replays and
/// explains unchanged.
#[test]
fn collecting_earlier_tasks_releases_their_pins_and_a_later_task_stays_replayable() {
    let f = Fixture::new();
    let mut pins = Vec::new();
    for (id, commit) in [("gc-a", "a"), ("gc-b", "b"), ("gc-c", "c")] {
        f.commit(commit);
        f.refresh();
        let file = f.task_file(id, "fixture/oriented", None);
        let done = f.task(&["task", "start", "--execute", "--file", &file], 0);
        assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
        let cas = f.cas();
        let brief = cas.get_json(&f.port(&done).unwrap()).unwrap();
        let pin = brief["input_artifacts"][0].as_str().unwrap().to_owned();
        assert_eq!(cas.get_json(&pin).unwrap()["type"], "af/ProjectSurveyPin@1");
        pins.push(pin);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        pins.iter().collect::<std::collections::BTreeSet<_>>().len(),
        3
    );
    let explained = f.task_text(&["task", "explain", "gc-c"]);
    let shown_before = f.task(&["task", "show", "gc-c"], 0);
    let before = f.task(&["task", "list", "--sizes"], 0)["store"]["bytes"]
        .as_u64()
        .unwrap();
    let applied = f.task(
        &["task", "gc", "--older-than", "0", "--keep", "1", "--apply"],
        0,
    );
    assert_eq!(
        applied["applied"]["tombstoned"],
        json!(["gc-a", "gc-b"]),
        "{applied}"
    );
    let after = f.task(&["task", "list", "--sizes"], 0)["store"]["bytes"]
        .as_u64()
        .unwrap();
    assert!(after < before, "{before} -> {after}");
    let cas = f.cas();
    assert!(!cas.is_filed(&pins[0]) && !cas.is_filed(&pins[1]));
    assert!(cas.is_filed(&pins[2]));
    assert_eq!(
        timeless(&f.task_text(&["task", "explain", "gc-c"])),
        timeless(&explained)
    );
    assert_eq!(f.task(&["task", "show", "gc-c"], 0), shown_before);
}

/// The fixture with a Planner whose proposal is `fixture/oriented` renamed
/// `generated/oriented`: a generated root that introduces the `project` port after the initial
/// capture. Every installed Pipeline requires a fact the Task file denies, so selection asks for
/// generation.
fn planning_fixture() -> (Fixture, minisign::KeyPair) {
    use review_core::task::pipeline::PipelineDefinitionV1;
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let public = key.pk.to_box().unwrap().into_string();
    let f = Fixture::with(|repo| {
        let read = |name: &str| {
            let path = repo.join(format!(".af/task-packages/fixture/{name}/pipeline.toml"));
            let text = std::fs::read_to_string(&path).unwrap();
            (path, toml::from_str::<PipelineDefinitionV1>(&text).unwrap())
        };
        let mut generated = read("oriented").1;
        generated.name = "generated/oriented".into();
        for name in ["oriented", "exposing", "forged"] {
            let (path, mut existing) = read(name);
            existing
                .accepts
                .required_facts
                .insert("small".into(), serde_json::from_value(json!(true)).unwrap());
            std::fs::write(&path, toml::to_string(&existing).unwrap()).unwrap();
        }
        let proposal = json!({"schema":"af.pipeline-proposal/1","root":generated.name,
            "definitions":{"generated/oriented": toml::to_string(&generated).unwrap()}});
        let planner = repo.join(".af/task-packages/fixture/planner");
        std::fs::create_dir_all(planner.join("outputs")).unwrap();
        std::fs::write(planner.join("proposal.json"), proposal.to_string()).unwrap();
        std::fs::write(
            planner.join("worker.py"),
            "import json, sys\nfrom pathlib import Path\njson.load(sys.stdin)\nproposal = json.loads(Path(__file__).with_name('proposal.json').read_text())\nprint(json.dumps({'schema':'af.worker-reply/1','outputs':{'proposal':[proposal]}}))\n",
        )
        .unwrap();
        let port = |kind: &str| json!({"artifact_type":kind,"cardinality":"one","optional":false,"affinity":{"kind":"unbound"},"covers":[]});
        let manifest: review_config::task::catalog::TaskWorkerManifest = serde_json::from_value(json!({
            "schema":"af.worker/1","name":"fixture/planner","version":"1.0.0",
            "signature":{"contract":{"inputs":{"request":port("af/PlanningRequest@1")},"outputs":{"proposal":port("af/PipelineProposal@1")}},"effects":[],"roles":["plan"],"evidence":{},"retains":{"proposal":["request"]},"worker_input_type":"af/PlannerInput@1","worker_output_type":"af/PipelineProposal@1","attempt":{"tokens":0,"wall_ms":5000}},
            "runner":{"kind":"command","command":{"program":"/usr/bin/python3","args":[{"value":"-B","provenance":"literal"},{"value":"@package/worker.py","provenance":"literal"}]}}
        }))
        .unwrap();
        std::fs::write(
            planner.join("worker.toml"),
            toml::to_string(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            planner.join("input.schema.json"),
            json!({"type":"object","additionalProperties":false,"required":["request"],"properties":{"request":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"object"}}}}).to_string(),
        )
        .unwrap();
        let mut schema: Value = serde_json::from_str(include_str!(
            "../../../../schemas/pipeline-proposal-v1.json"
        ))
        .unwrap();
        schema.as_object_mut().unwrap().remove("$id");
        schema.as_object_mut().unwrap().remove("$schema");
        std::fs::write(
            planner.join("outputs/proposal.schema.json"),
            schema.to_string(),
        )
        .unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        let table = catalog.as_table_mut().unwrap();
        table["packages"].as_table_mut().unwrap().insert(
            "fixture/planner".into(),
            toml::Value::try_from(json!({"version":"1.0.0","path":".af/task-packages/fixture/planner","digest":format!("sha256:{}", "0".repeat(64))})).unwrap(),
        );
        table.insert(
            "developers".into(),
            toml::Value::try_from(json!({"schema":"af.task-developers/1","keys":{"owner":public}}))
                .unwrap(),
        );
        table.insert(
            "planner".into(),
            toml::Value::try_from(json!({"worker":"fixture/planner","max_attempts":1})).unwrap(),
        );
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    });
    (f, key)
}

impl Fixture {
    /// The Task file of a generated plan: fallback `generate`, the fact `oriented` refuses, and
    /// room for the Planner's Attempt.
    fn generated_task_file(&self, task_id: &str) -> String {
        let path = self.task_file(task_id, "fixture/oriented", None);
        let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file["pipeline"]["fallback"] = json!("generate");
        file["facts"] = json!({"small": false});
        file["limits"]["max_attempts"] = json!(4);
        file["limits"]["wall_ms"] = json!(600_000);
        std::fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        path
    }

    /// Sign and record the developer's approval of the Task's current generated plan.
    fn approve(&self, task_id: &str, key: &minisign::KeyPair) {
        let payload = self.root.path().join("approved.payload");
        let signature = self.root.path().join("approved.minisig");
        self.task(
            &[
                "task",
                "decision-payload",
                task_id,
                "--developer",
                "owner",
                "--decision",
                "approved",
                "--reason",
                "Reviewed the generated closure",
                "--output",
                payload.to_str().unwrap(),
            ],
            0,
        );
        let signed = minisign::sign(
            Some(&key.pk),
            &key.sk,
            std::fs::read(&payload).unwrap().as_slice(),
            Some("exact generated plan"),
            None,
        )
        .unwrap();
        std::fs::write(&signature, signed.into_string()).unwrap();
        self.task(
            &[
                "task",
                "approve",
                task_id,
                "--payload",
                payload.to_str().unwrap(),
                "--signature",
                signature.to_str().unwrap(),
            ],
            0,
        );
    }
}

/// (m), (o): a generated root that introduces the port after the initial capture renders its
/// Brief from the pinned Survey; a newer Survey, a removed Project Store and a deleted checkout
/// between capture and handoff change nothing, and the captured staleness is still reported.
#[test]
fn a_generated_root_renders_the_pinned_survey_after_the_checkout_is_gone() {
    let (f, key) = planning_fixture();
    f.refresh();
    let pinned = f.preview(&[]);
    let file = f.generated_task_file("generated");
    let planned = f.task(&["task", "plan", "--file", &file], 0);
    assert_eq!(
        planned["plan"]["preparation"],
        json!({"kind":"planning"}),
        "{planned}"
    );
    f.commit("after-capture.txt");

    // A newer Survey, then no Project Store and no checkout at all.
    f.refresh();
    assert_ne!(f.preview(&[])["brief_id"], pinned["brief_id"]);
    std::fs::remove_dir_all(f.state_home().join("af/project")).unwrap();
    let elsewhere = f.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::remove_dir_all(&f.repo).unwrap();
    let state = f.state.to_str().unwrap();
    let handoff = f.af_in(
        &elsewhere,
        &[
            "task",
            "run",
            "--execute",
            "generated",
            "--json",
            "--state",
            state,
        ],
    );
    assert_eq!(handoff.status.code(), Some(0), "{}", shown(&handoff));
    let waiting: Value = serde_json::from_slice(&handoff.stdout).unwrap();
    assert_eq!(
        waiting["phase"],
        json!({"kind":"waiting","reason":"needs_plan_review"}),
        "{waiting}"
    );
    let cas = f.cas();
    let revision = cas
        .get_json(waiting["revision_id"].as_str().unwrap())
        .unwrap();
    let bound = &revision["payload"]["inputs"]["project"]["artifact_ids"][0];
    assert_eq!(bound, &pinned["brief_id"]);
    let receipt = receipts(&cas, waiting["revision_id"].as_str().unwrap()).remove(0);
    assert_eq!(receipt["outcome"], "constructed");
    // Captured when the Task was planned: the Survey head was the source commit.
    assert_eq!(
        receipt["staleness"],
        json!({"kind":"behind","behind_commits":"0"})
    );
    let explained = f.af_in(
        &elsewhere,
        &["task", "explain", "generated", "--state", state],
    );
    assert_eq!(explained.status.code(), Some(0), "{}", shown(&explained));
    let line = port_line(&String::from_utf8(explained.stdout).unwrap());
    assert!(
        line.starts_with(&format!(
            "project: af/ProjectBrief@1 {} (orient, ",
            pinned["brief_id"].as_str().unwrap()
        )) && line.ends_with("behind 0)"),
        "{line}"
    );

    std::fs::create_dir_all(&f.repo).unwrap();
    f.approve("generated", &key);
    let done = f.task(&["task", "run", "--execute", "generated"], 0);
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    let implement = context(&cas, &done, "root.nodes.implement");
    assert_eq!(
        entries(&implement, "project")[0]["artifact_id"],
        pinned["brief_id"]
    );
}

/// (p): a Worker node whose instructions and root inputs with the Brief exceed the Worker
/// request cap is refused at plan time, naming the node and the bytes, before any dispatch.
#[test]
fn a_node_over_the_request_cap_with_its_brief_is_refused_at_plan_time() {
    // Just under the cap on their own; the Brief tips them over.
    let instructions = "x".repeat(review_runner::task::MAX_WORKER_BYTES - 64);
    let f = Fixture::with(|repo| {
        let path = repo.join(".af/task-packages/fixture/implementer/instructions.md");
        std::fs::write(path, &instructions).unwrap();
    });
    let file = f.task_file("unbriefed", "fixture/oriented", None);
    f.task_text(&["task", "plan", "--file", &file]);
    f.refresh();
    let file = f.task_file("briefed", "fixture/oriented", None);
    let refused = f.af(&[
        "task",
        "plan",
        "--file",
        &file,
        "--json",
        "--state",
        f.state.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1), "{}", shown(&refused));
    let said = shown(&refused);
    assert!(
        said.contains("Worker node root.nodes.implement would receive")
            && said.contains("over the Worker request cap of 1048576 bytes"),
        "{said}"
    );
    let shown = f.af(&[
        "task",
        "show",
        "briefed",
        "--state",
        f.state.to_str().unwrap(),
    ]);
    assert!(
        !shown.status.success() || !String::from_utf8_lossy(&shown.stdout).contains("attempt"),
        "no Attempt was dispatched"
    );
}

/// The plan-time cap is the renderer's own accounting (ADR-0160 clause 7): the serialized Worker
/// request, its JSON escaping, value wrappers and output schemas included. Here the instructions
/// and the root inputs with the Brief sum to well under 1 MiB, but the instructions are quotes,
/// each escaped to two bytes, and the report schema carries a long description, so the request
/// the runner would render is over the cap and the plan fails before any dispatch.
#[test]
fn a_known_request_over_the_cap_only_once_serialized_fails_planning() {
    let quotes = 200_000;
    let description = 700_000;
    let f = Fixture::with(|repo| {
        let package = repo.join(".af/task-packages/fixture/implementer");
        std::fs::write(package.join("instructions.md"), "\"".repeat(quotes)).unwrap();
        let path = package.join("outputs/report.schema.json");
        let mut schema: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        schema["description"] = json!("d".repeat(description));
        std::fs::write(&path, schema.to_string()).unwrap();
    });
    // Without a Brief the node is not checked at plan time.
    let file = f.task_file("unbriefed", "fixture/oriented", None);
    f.task_text(&["task", "plan", "--file", &file]);
    f.refresh();
    let brief = f.preview(&[]);
    let brief_bytes: usize = brief["manifest"]["rendered_bytes"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    // The payload sum: the raw instructions and the Brief's value, with a generous allowance
    // for the requirements and source values, fits the cap with room to spare.
    let cap = review_runner::task::MAX_WORKER_BYTES;
    assert!(quotes + brief_bytes + 64 * 1024 < cap);
    // Serialized, the quotes and the schema alone are over it.
    assert!(2 * quotes + description > cap);
    let file = f.task_file("briefed", "fixture/oriented", None);
    let refused = f.af(&[
        "task",
        "plan",
        "--file",
        &file,
        "--json",
        "--state",
        f.state.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1), "{}", shown(&refused));
    let said = shown(&refused);
    assert!(
        said.contains("Worker node root.nodes.implement would receive a request of at least")
            && said.contains("output schemas as the renderer serializes them")
            && said.contains("over the Worker request cap of 1048576 bytes"),
        "{said}"
    );
    let bytes: usize = said
        .split("at least ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|count| count.parse().ok())
        .unwrap();
    assert!(bytes > 2 * quotes + description, "{said}");
}

impl Fixture {
    /// The fixture's one Project Store directory, once a command registered it.
    fn project_store(&self) -> PathBuf {
        let stores: Vec<PathBuf> = std::fs::read_dir(self.state_home().join("af/project"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.join("events.sqlite").is_file())
            .collect();
        assert_eq!(stores.len(), 1, "{stores:?}");
        stores[0].clone()
    }

    /// Every event type of the Project Store's log, in order.
    fn project_events(&self) -> Vec<String> {
        let log = rusqlite::Connection::open(self.project_store().join("events.sqlite")).unwrap();
        log.prepare("SELECT type FROM events ORDER BY sequence")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Commit `name` as the author `author` with the address `email`.
    fn commit_by(&self, author: &str, email: &str, name: &str) {
        std::fs::write(self.repo.join(name), format!("{name}\n")).unwrap();
        git(&self.repo, &["add", "-A"]);
        let user = format!("user.name={author}");
        let address = format!("user.email={email}");
        git(
            &self.repo,
            &["-c", &user, "-c", &address, "commit", "-qm", name],
        );
    }
}

/// Every file beneath `directory` with its length: what a command that records nothing leaves
/// as it found it. SQLite's shared-memory index is a reader's, not a record.
fn listing(directory: &Path) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else if !entry.file_name().to_string_lossy().ends_with("-shm") {
                found.push((entry.path(), entry.metadata().unwrap().len()));
            }
        }
    }
    found.sort();
    found
}

/// `af project brief` reads and records nothing: run before any Survey it registers no Project
/// and creates no Project Store, and after one it appends no event, writes no file there, and
/// leaves the Store registry `af/stores.toml`, the Project Store's entry and its last use, as it
/// found them.
#[test]
fn af_project_brief_registers_and_records_nothing() {
    let f = Fixture::new();
    assert_eq!(f.af(&["project", "brief"]).status.code(), Some(1));
    assert_eq!(f.preview(&[])["reason"], "no_survey");
    assert!(
        !f.state_home().join("af/project").exists(),
        "af project brief created a Project Store"
    );
    assert!(!f.state_home().join("af/stores.toml").exists());

    f.refresh();
    let events = f.project_events();
    let before = listing(&f.project_store());
    let registry = f.state_home().join("af/stores.toml");
    let registered = std::fs::read(&registry).unwrap();
    let last_use = |text: &[u8]| -> Vec<(String, i64)> {
        let parsed: toml::Value = toml::from_str(std::str::from_utf8(text).unwrap()).unwrap();
        parsed["store"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["kind"].as_str() == Some("project"))
            .map(|entry| {
                let path = entry["path"].as_str().unwrap().to_owned();
                (path, entry["last_use_unix_ms"].as_integer().unwrap())
            })
            .collect()
    };
    let used = last_use(&registered);
    assert_eq!(used.len(), 1, "{used:?}");
    // A later use would be a later millisecond.
    std::thread::sleep(std::time::Duration::from_millis(20));
    f.ok(&["project", "brief"]);
    f.preview(&["--variant", "orient"]);
    f.preview(&["--variant", "none"]);
    assert_eq!(f.project_events(), events);
    assert_eq!(listing(&f.project_store()), before);
    let after = std::fs::read(&registry).unwrap();
    assert_eq!(last_use(&after), used);
    assert_eq!(after, registered, "af project brief rewrote af/stores.toml");
}

/// No Brief document (the Brief, its pin, the capture, the receipt, the preview) holds an author
/// email or a host path, under either identity policy, a variant that renders every section
/// included: authors whose name is an email and whose name carries one among them.
#[test]
fn no_brief_document_holds_an_email_or_a_host_path_under_either_identity_policy() {
    let f = Fixture::with(|repo| {
        let path = repo.join(".af/task-packages/fixture/oriented/pipeline.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("brief = \"orient\"", "brief = \"team\""),
        )
        .unwrap();
    });
    f.commit_by("someone@example.org", "someone@example.org", "one.txt");
    f.commit_by("Ada ada@example.net", "ada@example.net", "two.txt");
    let root = f.root.path().to_str().unwrap().to_owned();
    let resolved = std::fs::canonicalize(f.root.path()).unwrap();
    let mut forbidden = vec![
        "example.org".to_owned(),
        "example.net".to_owned(),
        "example.invalid".to_owned(),
        root,
        resolved.to_str().unwrap().to_owned(),
    ];
    forbidden.dedup();
    for (index, identity) in ["name", "email_hash"].into_iter().enumerate() {
        f.write_policy(&format!(
            "[survey]\nidentity = \"{identity}\"\n\n[brief.team]\nsections = [\"identity\", \
             \"tree\", \"history\", \"contributors\", \"decisions\", \"vocabulary\", \"checks\", \
             \"hot_paths\", \"activity\"]\nmax_bytes = 262144\n"
        ));
        f.refresh();
        let preview = f.preview(&["--variant", "team"]);
        let contributors = &preview["brief"]["sections"]["contributors"]["by_identity"];
        assert!(
            contributors.as_array().is_some_and(|all| all.len() >= 3),
            "{preview}"
        );
        let id = format!("team-{index}");
        let file = f.task_file(&id, "fixture/oriented", None);
        let planned = f.task(&["task", "plan", "--file", &file], 0);
        let cas = f.cas();
        let brief_id = f.port(&planned).unwrap();
        assert_eq!(preview["brief_id"], brief_id.as_str());
        let brief = cas.get_json(&brief_id).unwrap();
        let pin = cas
            .get_json(brief["input_artifacts"][0].as_str().unwrap())
            .unwrap();
        assert_eq!(pin["type"], "af/ProjectSurveyPin@1");
        let revision = cas
            .get_json(planned["revision_id"].as_str().unwrap())
            .unwrap();
        let mut documents = vec![preview.clone(), brief, pin];
        for id in revision["payload"]["provenance"]["input_artifact_ids"]
            .as_array()
            .unwrap()
        {
            let Ok(envelope) = cas.get_json(id.as_str().unwrap()) else {
                continue;
            };
            if ["af/ProjectBriefCapture@1", "af/ProjectBriefReceipt@1"]
                .contains(&envelope["type"].as_str().unwrap_or_default())
            {
                documents.push(envelope);
            }
        }
        assert_eq!(documents.len(), 5, "{documents:?}");
        for document in &documents {
            let text = document.to_string();
            for needle in &forbidden {
                assert!(!text.contains(needle.as_str()), "{needle} in {text}");
            }
        }
    }
}

/// A refusal test, so it names the superseded label it refuses, as the Survey's own tests do:
/// a Survey record relabelled to the superseded version (ADR-0145) never feeds a Brief. `af
/// project brief` refuses it naming that version, a plan pins nothing and previews the empty
/// port, and the next refresh records a Survey of this release that renders again. The label
/// comes from the Survey's refusal constant; the Brief's live code never names it.
#[test]
fn a_survey_record_of_the_superseded_project_survey_1_label_is_refused_and_feeds_no_brief() {
    use review_core::project::{PROJECT_SURVEY_V1_SUPERSEDED, PROJECT_SURVEY_V2};
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.refresh();
    let store = f.project_store();
    let log = rusqlite::Connection::open(store.join("events.sqlite")).unwrap();
    let (sequence, payload): (i64, String) = log
        .query_row(
            "SELECT sequence, payload FROM events WHERE type = 'SurveyRecorded@1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut payload: Value = serde_json::from_str(&payload).unwrap();
    let object = |id: &str| {
        let hex = id.trim_start_matches("sha256:");
        store.join("cas/objects").join(&hex[..2]).join(&hex[2..])
    };
    let current = object(payload["survey_id"].as_str().unwrap());
    let text = std::fs::read_to_string(&current).unwrap();
    let current_label = format!("\"schema\":\"{PROJECT_SURVEY_V2}\"");
    assert!(text.contains(&current_label), "{text}");
    let old = text.replace(PROJECT_SURVEY_V2, PROJECT_SURVEY_V1_SUPERSEDED);
    let old_id = review_store::canonical::blob_content_id(old.as_bytes());
    std::fs::create_dir_all(object(&old_id).parent().unwrap()).unwrap();
    std::fs::write(object(&old_id), &old).unwrap();
    std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::remove_file(&current).unwrap();
    payload["survey_id"] = json!(old_id);
    log.execute(
        "UPDATE events SET payload = ?1, artifact_refs = ?2 WHERE sequence = ?3",
        rusqlite::params![payload.to_string(), json!([old_id]).to_string(), sequence],
    )
    .unwrap();
    drop(log);

    let refused = f.af(&["project", "brief"]);
    assert_eq!(refused.status.code(), Some(1), "{}", shown(&refused));
    assert!(
        shown(&refused).contains(&format!(
            "it is {PROJECT_SURVEY_V1_SUPERSEDED}, a superseded version"
        )),
        "{}",
        shown(&refused)
    );
    let file = f.task_file("superseded", "fixture/oriented", None);
    let planned = f.af(&[
        "task",
        "plan",
        "--file",
        &file,
        "--state",
        f.state.to_str().unwrap(),
    ]);
    assert_eq!(planned.status.code(), Some(0), "{}", shown(&planned));
    let said = String::from_utf8(planned.stdout.clone()).unwrap();
    assert_eq!(
        port_line(&said),
        "project: none (no Survey recorded; run af project refresh)"
    );
    assert!(
        String::from_utf8_lossy(&planned.stderr).contains(PROJECT_SURVEY_V1_SUPERSEDED),
        "{}",
        shown(&planned)
    );

    f.refresh();
    let preview = f.preview(&[]);
    assert_eq!(preview["outcome"], "brief", "{preview}");
    assert_ne!(preview["brief"]["survey_id"], old_id.as_str());
}

/// Every directory and file beneath `directory`, each file with its bytes: what a command that
/// changes nothing leaves byte for byte as it found it.
fn contents(directory: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
                found.push((entry.path(), None));
            } else {
                found.push((entry.path(), Some(std::fs::read(entry.path()).unwrap())));
            }
        }
    }
    found.sort();
    found
}

/// Paths made read-only, files `0444` and directories `0555`, until the value drops, which gives
/// each its own mode back.
struct Unwritable(Vec<(PathBuf, u32)>);

impl Unwritable {
    fn hold(paths: &[PathBuf]) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let mut held = Vec::new();
        for path in paths {
            let metadata = std::fs::metadata(path).unwrap();
            let mode = if metadata.is_dir() { 0o555 } else { 0o444 };
            held.push((path.clone(), metadata.permissions().mode()));
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        Self(held)
    }
}

impl Drop for Unwritable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        for (path, mode) in self.0.iter().rev() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode));
        }
    }
}

/// `af project brief` opens the Project Store strictly read-only (ADR-0160 clause 9): with the
/// Store directory and its `events.sqlite` unwritable every preview form still renders, and no
/// byte beneath the state root changes and nothing appears there: the Store's lock, log and
/// CAS, its journal files, and the Store registry `af/stores.toml` with its last use included.
#[test]
fn af_project_brief_reads_an_unwritable_project_store_and_changes_no_byte() {
    let f = Fixture::new();
    f.refresh();
    let store = f.project_store();
    let before = contents(&f.state_home());
    assert!(
        before
            .iter()
            .any(|(path, _)| path.ends_with("af/stores.toml")),
        "{before:?}"
    );
    {
        let _held = Unwritable::hold(&[store.join("events.sqlite"), store.clone()]);
        let text = f.ok(&["project", "brief"]);
        let preview = f.preview(&[]);
        assert_eq!(preview["outcome"], "brief", "{preview}");
        let canonical = review_store::canonicalize(&preview["brief"]).unwrap();
        assert_eq!(format!("{}\n", String::from_utf8(canonical).unwrap()), text);
        let survey = preview["brief"]["survey_id"].as_str().unwrap().to_owned();
        let named = f.preview(&["--survey", &survey]);
        assert_eq!(named, preview);
        assert_eq!(f.preview(&["--variant", "none"])["reason"], "variant_none");
    }
    assert_eq!(contents(&f.state_home()), before);
}

impl Fixture {
    /// The IDs of every typed record of `kind` in the Task Store's CAS.
    fn records(&self, kind: &str) -> Vec<String> {
        let cas = self.cas();
        cas.filed_objects()
            .unwrap()
            .into_iter()
            .filter(|object| {
                cas.get_optional_artifact(&object.digest)
                    .ok()
                    .flatten()
                    .is_some_and(|envelope| envelope.artifact_type == kind)
            })
            .map(|object| object.digest)
            .collect()
    }

    /// The CAS object of the Project Store's one recorded Survey.
    fn survey_object(&self) -> PathBuf {
        let store = self.project_store();
        let log = rusqlite::Connection::open(store.join("events.sqlite")).unwrap();
        let payload: String = log
            .query_row(
                "SELECT payload FROM events WHERE type = 'SurveyRecorded@1' \
                 ORDER BY sequence DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        let hex = payload["survey_id"]
            .as_str()
            .unwrap()
            .trim_start_matches("sha256:")
            .to_owned();
        store.join("cas/objects").join(&hex[..2]).join(&hex[2..])
    }
}

/// The `af/ProjectBriefCapture@1` payload a revision's provenance holds, if any.
fn capture_of(cas: &Cas, revision_id: &str) -> Option<Value> {
    let revision = cas.get_json(revision_id).unwrap();
    revision["payload"]["provenance"]["input_artifact_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|id| cas.get_json(id.as_str().unwrap()).ok())
        .find(|envelope| envelope["type"] == "af/ProjectBriefCapture@1")
        .map(|envelope| envelope["payload"].clone())
}

/// An explicit binding suppresses construction without reading a Survey (ADR-0160 clause 6).
/// The Project Store keeps its complete Survey, and a named pipeline's one Brief port is bound
/// to a recorded Brief: planning reads no Survey (its record is set aside while the Task is
/// planned, and nothing misses it), writes no `af/ProjectSurveyPin@1`, and records a capture
/// that names no pin. A Survey refresh after planning leaves the revision the run executes, its
/// provenance, capture and receipt as planned.
#[test]
fn a_bound_brief_port_reads_no_survey_and_a_later_refresh_changes_nothing() {
    let f = Fixture::new();
    f.refresh();
    let file = f.task_file("recorded", "fixture/exposing", None);
    let recorded = f.task(&["task", "start", "--execute", "--file", &file], 0);
    assert_eq!(recorded["result"]["acceptance"], "satisfied", "{recorded}");
    let brief = recorded["result"]["outputs"]["brief"]["artifact_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let pins = f.records("af/ProjectSurveyPin@1");
    assert_eq!(pins.len(), 1, "{pins:?}");

    let survey = f.survey_object();
    let aside = survey.with_extension("aside");
    std::fs::rename(&survey, &aside).unwrap();
    let bound = json!({"project": {"task": "recorded", "port": "brief"}});
    let file = f.task_file("bound", "fixture/oriented", Some(bound));
    let planned = f.af(&[
        "task",
        "plan",
        "--file",
        &file,
        "--state",
        f.state.to_str().unwrap(),
    ]);
    std::fs::rename(&aside, &survey).unwrap();
    assert_eq!(planned.status.code(), Some(0), "{}", shown(&planned));
    assert!(
        !String::from_utf8_lossy(&planned.stderr).contains("Project Brief"),
        "planning a bound port read the Project Store: {}",
        shown(&planned)
    );
    let planned = String::from_utf8(planned.stdout).unwrap();
    let line = port_line(&planned);
    assert!(
        line.starts_with(&format!(
            "project: af/ProjectBrief@1 {brief} (bound, orient, "
        )),
        "{line}"
    );
    assert_eq!(f.records("af/ProjectSurveyPin@1"), pins);
    let shown_plan = f.task(&["task", "show", "bound"], 0);
    let revision_id = shown_plan["revision_id"].as_str().unwrap().to_owned();
    let cas = f.cas();
    let capture = capture_of(&cas, &revision_id).unwrap();
    assert!(capture.get("pin_id").is_none(), "{capture}");
    let head = git(&f.repo, &["rev-parse", "HEAD"]);
    assert_eq!(capture["source_commit"], head.as_str());
    assert_eq!(
        capture["ancestry"],
        json!({ head.clone(): {"kind": "behind", "behind_commits": "0"} })
    );
    let provenance = cas.get_json(&revision_id).unwrap()["payload"]["provenance"].clone();
    let receipt = receipts(&cas, &revision_id);

    // A newer Survey after planning.
    f.commit("later.txt");
    f.refresh();
    assert_ne!(f.preview(&[])["brief_id"], brief.as_str());
    let done = f.task(
        &["task", "run", "bound", "--confirm-plan", &plan_id(&planned)],
        0,
    );
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    assert_eq!(done["revision_id"], revision_id.as_str());
    assert_eq!(f.port(&done).as_deref(), Some(brief.as_str()));
    let cas = f.cas();
    assert_eq!(
        cas.get_json(&revision_id).unwrap()["payload"]["provenance"],
        provenance
    );
    assert_eq!(capture_of(&cas, &revision_id).unwrap(), capture);
    assert_eq!(receipts(&cas, &revision_id), receipt);
    assert_eq!(receipt[0]["outcome"], "bound");
    assert_eq!(f.records("af/ProjectSurveyPin@1"), pins);
    assert_eq!(port_line(&f.task_text(&["task", "explain", "bound"])), line);
}

/// A `no_survey` receipt names the resolved source commit (ADR-0160 clause 5): a Task over a
/// committed checkout with a Brief port, no complete Survey and no variant of the project's own
/// keeps its capture, which holds that commit alone, and its receipt carries it.
#[test]
fn a_no_survey_receipt_carries_the_resolved_source_commit() {
    let f = Fixture::new();
    let head = git(&f.repo, &["rev-parse", "HEAD"]);
    let file = f.task_file("unsurveyed", "fixture/oriented", None);
    let planned = f.task(&["task", "plan", "--file", &file], 0);
    let revision_id = planned["revision_id"].as_str().unwrap();
    let cas = f.cas();
    assert_eq!(
        capture_of(&cas, revision_id),
        Some(json!({"schema": "af/ProjectBriefCapture@1", "source_commit": head}))
    );
    assert_eq!(
        receipts(&cas, revision_id),
        vec![json!({
            "schema": "af/ProjectBriefReceipt@1",
            "port": "project",
            "outcome": "none",
            "variant": "orient",
            "reason": "no_survey",
            "source_commit": head,
        })]
    );
}
