//! ADR-0151 end to end: one Project per repository, its Workspaces, and `af project`.
//!
//! Two worktrees of one repository register one Project with two Workspaces, and `af project
//! show` answers the same from either; a shallow clone is refused with the way out; a repository
//! without commits registers nothing. Adopting a default Task Store changes none of its bytes.
//! `--workspace` plans exactly what the explicit `--state` and `--bindings` plan.
//! `af project workspace new|remove` create and retire Workspaces without a commit and without
//! removing a Store, and delivery registers the worktree it made. `workspace new` fills its
//! worktree without a checkout, so a configured smudge filter never runs; it refuses a path inside
//! the checkout, another Workspace or af's state with exit 2, and every refusal leaves worktrees,
//! refs and directories as they were. A git that outlives its deadline fails the command and
//! leaves no child. The first `af task plan --workspace <checkout>` finds the checkout it
//! registers. No command writes inside a checkout, and the help answers in under 50 ms.
//!
//! Every test runs with its own `XDG_STATE_HOME`, so the Projects it registers are its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::schemas;

/// One machine: a private state home below a temporary root.
struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("state")).unwrap();
        Self { root }
    }

    fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path()).unwrap()
    }

    fn state_home(&self) -> PathBuf {
        self.root().join("state")
    }

    fn af(&self, directory: &Path, args: &[&str]) -> Output {
        crate::common::af()
            .current_dir(directory)
            .env("XDG_STATE_HOME", self.state_home())
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1")
            .args(args)
            .output()
            .unwrap()
    }

    /// Run `af`, expect `code`, and return stdout.
    fn expect(&self, directory: &Path, args: &[&str], code: i32) -> String {
        let output = self.af(directory, args);
        let (stdout, stderr) = text(&output);
        assert_eq!(
            output.status.code(),
            Some(code),
            "af {args:?}\n{stdout}\n{stderr}"
        );
        stdout
    }

    fn json(&self, directory: &Path, args: &[&str]) -> Value {
        let stdout = self.expect(directory, args, 0);
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("not JSON: {stdout}"))
    }

    fn projects(&self) -> PathBuf {
        self.state_home().join("af").join("project")
    }
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repository with two commits and a `.af/af.toml` that declares its name.
fn repository(root: &Path) -> PathBuf {
    let repo = root.join("fixture");
    std::fs::create_dir_all(repo.join(".af")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[project]\nname = \"fixture\"\nmin_af = \"0.1\"\n[defaults]\npipeline = \
         \"review\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("README.md"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);
    std::fs::write(repo.join("README.md"), "two\n").unwrap();
    git(&repo, &["commit", "-qam", "two"]);
    repo
}

/// What `af` derives, derived here from git alone.
fn expected_project_id(repo: &Path) -> String {
    let mut roots: Vec<String> = git(repo, &["rev-list", "--max-parents=0", "HEAD"])
        .lines()
        .map(str::to_string)
        .collect();
    roots.sort();
    let mut bytes = b"af/project-id@1\0".to_vec();
    bytes.extend_from_slice(roots.join(",").as_bytes());
    format!(
        "sha256:{}",
        review_core::hex::encode(&Sha256::digest(&bytes))
    )
}

/// The default Task Store `af task` uses in `checkout` without `--state`.
fn default_store(machine: &Machine, checkout: &Path) -> PathBuf {
    let checkout = std::fs::canonicalize(checkout).unwrap();
    let digest = Sha256::digest(checkout.as_os_str().as_encoded_bytes());
    machine
        .state_home()
        .join("af/task/local")
        .join(&review_core::hex::encode(&digest)[..16])
}

/// Every file below `root` except git's own directory, with its bytes.
fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn workspace<'a>(document: &'a Value, label: &str) -> &'a Value {
    document["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workspace| workspace["label"] == label)
        .unwrap_or_else(|| panic!("no Workspace {label} in {document}"))
}

fn labels(document: &Value) -> Vec<String> {
    document["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["label"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn two_worktrees_register_one_project_with_two_workspaces() {
    let machine = Machine::new();
    let repo = repository(&machine.root());
    let second = machine.root().join("fixture-wt-second");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "second",
            second.to_str().unwrap(),
        ],
    );
    let before = (files(&repo), files(&second));

    let first = machine.json(&repo, &["project", "show", "--json"]);
    assert_eq!(labels(&first), ["fixture"]);
    machine.expect(&second, &["project", "show"], 0);
    let from_repo = machine.json(&repo, &["project", "show", "--json"]);
    let from_second = machine.json(&second, &["project", "show", "--json"]);
    assert_eq!(from_repo, from_second);
    let validator = schemas::validator("project-v1.json");
    for document in [&first, &from_repo] {
        schemas::valid(&validator, document);
    }

    let project_id = expected_project_id(&repo);
    assert_eq!(from_repo["project_id"], project_id.as_str());
    assert_eq!(
        from_repo["repository_id"],
        git(&repo, &["rev-list", "--max-parents=0", "HEAD"]).as_str()
    );
    assert_eq!(from_repo["name"], "fixture");
    assert_eq!(from_repo["min_af"], "0.1");
    let store = machine
        .projects()
        .join(project_id.trim_start_matches("sha256:"));
    assert_eq!(from_repo["store"], store.to_str().unwrap());
    assert!(store.join("events.sqlite").is_file() && store.join("cas").is_dir());
    assert_eq!(std::fs::read_dir(machine.projects()).unwrap().count(), 1);
    // The Store registry records the Project Store so the Storage Budget reaches it.
    let registry = std::fs::read_to_string(machine.state_home().join("af/stores.toml")).unwrap();
    assert!(registry.contains(store.to_str().unwrap()), "{registry}");
    assert!(registry.contains("kind = \"project\""), "{registry}");

    assert_eq!(labels(&from_repo), ["fixture", "fixture-wt-second"]);
    for (label, checkout) in [("fixture", &repo), ("fixture-wt-second", &second)] {
        let entry = workspace(&from_repo, label);
        assert_eq!(entry["origin"], "adopted");
        assert_eq!(entry["path"], checkout.to_str().unwrap());
        assert_eq!(
            entry["state"],
            default_store(&machine, checkout).to_str().unwrap()
        );
    }

    // The same Project in text, from either worktree.
    for checkout in [&repo, &second] {
        let shown = machine.expect(checkout, &["project", "show"], 0);
        assert!(
            shown.starts_with(&format!("Project {project_id}\n")),
            "{shown}"
        );
        assert!(shown.contains("2 Workspaces"), "{shown}");
        assert!(shown.contains("this checkout"), "{shown}");
    }
    let listed = machine.json(&second, &["project", "workspaces", "--json"]);
    schemas::valid(&schemas::validator("project-workspaces-v1.json"), &listed);
    assert_eq!(listed["workspaces"], from_repo["workspaces"]);

    // The records hold no path: only IDs, labels and origins.
    let workspaces = std::fs::read_to_string(store.join("workspaces.toml")).unwrap();
    assert!(workspaces.contains(repo.to_str().unwrap()), "{workspaces}");
    let log = rusqlite::Connection::open(store.join("events.sqlite")).unwrap();
    let payloads: Vec<String> = log
        .prepare("SELECT payload FROM events ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(payloads.len(), 3, "{payloads:?}");
    for payload in &payloads {
        assert!(
            !payload.contains(machine.root().to_str().unwrap()),
            "{payload}"
        );
    }
    // Nothing was written inside either checkout.
    assert_eq!((files(&repo), files(&second)), before);
}

#[test]
fn a_shallow_clone_is_refused_and_a_repository_without_commits_registers_nothing() {
    let machine = Machine::new();
    let repo = repository(&machine.root());
    let shallow = machine.root().join("shallow");
    git(
        &machine.root(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", repo.display()),
            shallow.to_str().unwrap(),
        ],
    );
    let refused = machine.af(&shallow, &["project", "show"]);
    let (_, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("git fetch --unshallow"), "{stderr}");
    let refused = machine.af(&shallow, &["project", "show", "--json"]);
    let document: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(document["schema"], "af/error@1");
    assert!(
        document["error"]
            .as_str()
            .unwrap()
            .contains("git fetch --unshallow")
    );

    // A command that registers lazily warns once and keeps its own exit code: a plan of an
    // absent Task file fails the same way inside and outside a repository.
    let elsewhere = machine.root().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let plan = ["task", "plan", "--file", "absent.json"];
    let outside = machine.af(&elsewhere, &plan);
    let lazily = machine.af(&shallow, &plan);
    let (_, stderr) = text(&lazily);
    assert_eq!(lazily.status.code(), outside.status.code(), "{stderr}");
    assert_eq!(
        stderr.matches("git fetch --unshallow").count(),
        1,
        "{stderr}"
    );
    assert!(!machine.projects().exists());

    let empty = machine.root().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    git(&empty, &["init", "-q", "-b", "main"]);
    let refused = machine.af(&empty, &["project", "show"]);
    let (_, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("no commit"), "{stderr}");
    let lazily = machine.af(&empty, &plan);
    let (_, stderr) = text(&lazily);
    assert_eq!(lazily.status.code(), outside.status.code(), "{stderr}");
    assert!(!stderr.contains("Project"), "{stderr}");
    assert!(!machine.projects().exists());
    assert!(files(&empty).is_empty());
}

#[test]
fn adopting_an_existing_default_store_changes_no_byte_of_it() {
    let machine = Machine::new();
    let (repo, _) = crate::task_gc::fixture(&machine.root());
    machine.expect(
        &repo,
        &["task", "plan", "--file", "gc-older.json", "--json"],
        0,
    );
    let store = default_store(&machine, &repo);
    assert!(store.join("events.sqlite").is_file());
    // As if the Store predated Projects: no Project Store, no registration.
    std::fs::remove_dir_all(machine.projects()).unwrap();
    let before = files(&store);

    let shown = machine.json(&repo, &["project", "show", "--json"]);
    let adopted = workspace(&shown, "repo");
    assert_eq!(adopted["origin"], "adopted");
    assert_eq!(adopted["state"], store.to_str().unwrap());
    assert_eq!(files(&store), before);
}

#[test]
fn a_task_planned_with_a_workspace_equals_one_planned_with_the_explicit_flags() {
    let machine = Machine::new();
    let (repo, _) = crate::task_gc::fixture(&machine.root());
    let created = machine.json(
        &repo,
        &["project", "workspace", "new", "research", "--json"],
    );
    schemas::valid(&schemas::validator("project-workspaces-v1.json"), &created);
    let research = workspace(&created, "research").clone();
    let worktree = PathBuf::from(research["path"].as_str().unwrap());
    let state = PathBuf::from(research["state"].as_str().unwrap());
    let bindings = PathBuf::from(research["bindings"].as_str().unwrap());
    let tasks = PathBuf::from(research["tasks"].as_str().unwrap());
    let file = tasks.join("gc-older.json");
    std::fs::copy(repo.join("gc-older.json"), &file).unwrap();
    let file = file.to_str().unwrap();

    let by_label = machine.json(
        &worktree,
        &[
            "task",
            "plan",
            "--workspace",
            "research",
            "--file",
            file,
            "--json",
        ],
    );
    assert!(
        state.join("events.sqlite").is_file(),
        "the Workspace's Store"
    );
    let shown = machine.json(
        &worktree,
        &[
            "task",
            "show",
            "gc-older",
            "--workspace",
            "research",
            "--json",
        ],
    );
    assert_eq!(shown["revision_id"], by_label["revision_id"]);
    // The same plan again, by the explicit paths, in a Store at the same path.
    std::fs::rename(&state, state.with_file_name("state-by-label")).unwrap();
    let explicit = machine.json(
        &worktree,
        &[
            "task",
            "plan",
            "--state",
            state.to_str().unwrap(),
            "--bindings",
            bindings.to_str().unwrap(),
            "--file",
            file,
            "--json",
        ],
    );
    let recorded = |state: &Path, document: &Value| -> (Value, Value) {
        let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
        let revision = cas
            .get_json(document["revision_id"].as_str().unwrap())
            .unwrap()["payload"]
            .clone();
        let adapter = cas
            .get_json(revision["provenance"]["adapter_id"].as_str().unwrap())
            .unwrap();
        (revision, adapter)
    };
    let (by_label_revision, by_label_adapter) =
        recorded(&state.with_file_name("state-by-label"), &by_label);
    let (explicit_revision, explicit_adapter) = recorded(&state, &explicit);
    // Two plans differ only by the wall-clock deadline each computed when it ran, and by the
    // identities derived from it; every captured input, the bindings (in the authority) and the
    // compiled plan are the same.
    let deadline = |revision: &Value| revision["limits"]["deadline_unix_ms"].as_u64().unwrap();
    assert!(deadline(&by_label_revision).abs_diff(deadline(&explicit_revision)) < 60_000);
    let timeless = |value: &Value, pointers: &[&str]| {
        let mut value = value.clone();
        for pointer in pointers {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            value
                .pointer_mut(parent)
                .and_then(Value::as_object_mut)
                .unwrap()
                .remove(key)
                .unwrap_or_else(|| panic!("{pointer}"));
        }
        value
    };
    let revision_volatile = ["/limits/deadline_unix_ms", "/provenance/adapter_id"];
    assert_eq!(
        timeless(&by_label_revision, &revision_volatile),
        timeless(&explicit_revision, &revision_volatile)
    );
    assert_eq!(
        by_label_adapter["source_adapter_id"],
        explicit_adapter["source_adapter_id"]
    );
    let plan_volatile = [
        "/plan/task_revision_id",
        "/plan/compiled_graph_id",
        "/plan/limits/deadline_unix_ms",
    ];
    assert_eq!(
        timeless(&by_label, &plan_volatile)["plan"],
        timeless(&explicit, &plan_volatile)["plan"]
    );
    assert_eq!(by_label["graph"], explicit["graph"]);
    assert_eq!(
        by_label["selection"]["assessment"],
        explicit["selection"]["assessment"]
    );
    // The bindings were applied: without them the authority is another one.
    std::fs::rename(&state, state.with_file_name("state-explicit")).unwrap();
    let unbound = machine.json(
        &worktree,
        &[
            "task",
            "plan",
            "--state",
            state.to_str().unwrap(),
            "--file",
            file,
            "--json",
        ],
    );
    let (unbound_revision, _) = recorded(&state, &unbound);
    assert_ne!(
        unbound_revision["authority"]["policy_id"],
        by_label_revision["authority"]["policy_id"]
    );

    let both = machine.af(
        &worktree,
        &[
            "task",
            "plan",
            "--workspace",
            "research",
            "--state",
            "/tmp/x",
            "--file",
            file,
        ],
    );
    assert_eq!(both.status.code(), Some(2), "{:?}", text(&both));
    let both = machine.af(
        &worktree,
        &[
            "task",
            "list",
            "--workspace",
            "research",
            "--state",
            "/tmp/x",
        ],
    );
    assert_eq!(both.status.code(), Some(2), "{:?}", text(&both));
    let unknown = machine.af(&worktree, &["task", "list", "--workspace", "nobody"]);
    let (_, stderr) = text(&unknown);
    assert_eq!(unknown.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("af project workspaces"), "{stderr}");
}

#[test]
fn workspace_new_creates_a_worktree_a_branch_and_a_directory_and_commits_nothing() {
    let machine = Machine::new();
    let repo = repository(&machine.root());
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let commits = git(&repo, &["rev-list", "--all", "--count"]);
    let before = files(&repo);

    let created = machine.expect(&repo, &["project", "workspace", "new", "research"], 0);
    assert!(
        created.starts_with("created Workspace research\n"),
        "{created}"
    );
    let worktree = machine.root().join("fixture-wt-research");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "af/research"
    );
    assert_eq!(git(&repo, &["rev-list", "--all", "--count"]), commits);
    assert_eq!(files(&repo), before);
    let store = machine
        .projects()
        .join(expected_project_id(&repo).trim_start_matches("sha256:"));
    let directory = store.join("workspaces/research");
    assert!(directory.join("state").is_dir());
    assert_eq!(
        std::fs::read_dir(directory.join("state")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(directory.join("tasks")).unwrap().count(),
        0
    );
    let template = std::fs::read_to_string(directory.join("bindings.toml")).unwrap();
    assert!(
        template.contains("schema = \"af.task-bindings/1\""),
        "{template}"
    );
    assert!(template.contains("[providers]"), "{template}");
    let parsed: toml::Value = toml::from_str(&template).unwrap();
    assert_eq!(parsed["providers"].as_table().unwrap().len(), 0);

    let listed = machine.json(&worktree, &["project", "workspaces", "--json"]);
    let research = workspace(&listed, "research");
    assert_eq!(research["origin"], "created");
    assert_eq!(research["path"], worktree.to_str().unwrap());
    assert_eq!(research["state"], directory.join("state").to_str().unwrap());
    assert_eq!(
        research["bindings"],
        directory.join("bindings.toml").to_str().unwrap()
    );
    assert_eq!(research["tasks"], directory.join("tasks").to_str().unwrap());

    // An existing label, path or branch is refused, and nothing is made.
    let taken = machine.af(&repo, &["project", "workspace", "new", "research"]);
    assert_eq!(taken.status.code(), Some(1));
    std::fs::create_dir_all(machine.root().join("occupied")).unwrap();
    let occupied = machine.af(
        &repo,
        &[
            "project",
            "workspace",
            "new",
            "spike",
            "--path",
            "../occupied",
        ],
    );
    assert_eq!(occupied.status.code(), Some(1), "{:?}", text(&occupied));
    let branch = machine.af(
        &repo,
        &["project", "workspace", "new", "spike", "--branch", "main"],
    );
    let (_, stderr) = text(&branch);
    assert_eq!(branch.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(!store.join("workspaces/spike").exists());
    assert!(!machine.root().join("fixture-wt-spike").exists());
    let bad = machine.af(&repo, &["project", "workspace", "new", "../escape"]);
    assert_eq!(bad.status.code(), Some(2), "a label is a safe name");

    // Explicit flags: another branch, from the first commit, at another path.
    let first = git(&repo, &["rev-list", "--max-parents=0", "HEAD"]);
    machine.expect(
        &repo,
        &[
            "project",
            "workspace",
            "new",
            "spike",
            "--branch",
            "agent/spike",
            "--from",
            &first,
            "--path",
            "../spike",
        ],
        0,
    );
    let spike = machine.root().join("spike");
    assert_eq!(git(&spike, &["rev-parse", "HEAD"]), first);
    assert_eq!(
        git(&spike, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "agent/spike"
    );
}

#[test]
fn workspace_remove_keeps_a_store_with_an_uncollected_task_and_names_task_gc() {
    let machine = Machine::new();
    let (repo, _) = crate::task_gc::fixture(&machine.root());
    let created = machine.json(
        &repo,
        &["project", "workspace", "new", "research", "--json"],
    );
    let research = workspace(&created, "research").clone();
    let worktree = PathBuf::from(research["path"].as_str().unwrap());
    let state = PathBuf::from(research["state"].as_str().unwrap());
    let done = machine.json(
        &worktree,
        &[
            "task",
            "start",
            "--execute",
            "--workspace",
            "research",
            "--file",
            "gc-older.json",
            "--json",
        ],
    );
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    let kept = files(&state);

    let removed = machine.expect(&repo, &["project", "workspace", "remove", "research"], 0);
    assert!(removed.contains("removed Workspace research"), "{removed}");
    assert!(removed.contains("gc-older"), "{removed}");
    assert!(removed.contains("af task gc"), "{removed}");
    assert_eq!(files(&state), kept, "the Store is left as it was");
    assert!(
        worktree.is_dir(),
        "the worktree stays without --delete-worktree"
    );
    let listed = machine.json(&repo, &["project", "workspaces", "--json"]);
    assert_eq!(labels(&listed), ["repo"]);
    let unknown = machine.af(&repo, &["project", "workspace", "remove", "research"]);
    assert_eq!(unknown.status.code(), Some(1));

    // --delete-worktree removes a clean worktree, keeps the branch, and refuses a dirty one.
    machine.expect(&repo, &["project", "workspace", "new", "spike"], 0);
    let spike = machine.root().join("repo-wt-spike");
    std::fs::write(spike.join("scratch.txt"), "work in progress\n").unwrap();
    let dirty = machine.af(
        &repo,
        &[
            "project",
            "workspace",
            "remove",
            "spike",
            "--delete-worktree",
        ],
    );
    let (_, stderr) = text(&dirty);
    assert_eq!(dirty.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("nothing was changed"), "{stderr}");
    assert!(spike.join("scratch.txt").is_file());
    let listed = machine.json(&repo, &["project", "workspaces", "--json"]);
    assert!(labels(&listed).contains(&"spike".to_string()));
    std::fs::remove_file(spike.join("scratch.txt")).unwrap();
    let gone = machine.expect(
        &repo,
        &[
            "project",
            "workspace",
            "remove",
            "spike",
            "--delete-worktree",
        ],
        0,
    );
    assert!(gone.contains("removed the worktree"), "{gone}");
    assert!(!spike.exists());
    git(&repo, &["rev-parse", "--verify", "refs/heads/af/spike"]);
}

#[test]
fn workspace_add_adopts_a_checkout_of_this_project_and_refuses_another() {
    let machine = Machine::new();
    let repo = repository(&machine.root());
    let hand_made = machine.root().join("workstreams/research");
    for directory in ["state", "tasks"] {
        std::fs::create_dir_all(hand_made.join(directory)).unwrap();
    }
    std::fs::write(
        hand_made.join("bindings.toml"),
        "schema = \"af.task-bindings/1\"\n",
    )
    .unwrap();
    let worktree = machine.root().join("fixture-wt-research");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "research",
            worktree.to_str().unwrap(),
        ],
    );
    let added = machine.json(
        &repo,
        &[
            "project",
            "workspace",
            "add",
            worktree.to_str().unwrap(),
            "--label",
            "research",
            "--state",
            hand_made.join("state").to_str().unwrap(),
            "--bindings",
            hand_made.join("bindings.toml").to_str().unwrap(),
            "--tasks",
            hand_made.join("tasks").to_str().unwrap(),
            "--json",
        ],
    );
    let research = workspace(&added, "research");
    assert_eq!(research["origin"], "adopted");
    assert_eq!(research["state"], hand_made.join("state").to_str().unwrap());
    let again = machine.af(
        &repo,
        &["project", "workspace", "add", worktree.to_str().unwrap()],
    );
    let (_, stderr) = text(&again);
    assert_eq!(again.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("already the Workspace `research`"),
        "{stderr}"
    );

    let other = machine.root().join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    git(
        &other,
        &["commit", "-q", "--allow-empty", "-m", "unrelated"],
    );
    let refused = machine.af(
        &repo,
        &["project", "workspace", "add", other.to_str().unwrap()],
    );
    let (_, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&expected_project_id(&repo)), "{stderr}");
    assert!(stderr.contains(&expected_project_id(&other)), "{stderr}");
}

#[test]
fn delivery_registers_the_worktree_it_created_and_its_receipt_is_unchanged() {
    let machine = Machine::new();
    let (repo, _) = crate::task_gc::fixture(&machine.root());
    machine.expect(&repo, &["project", "workspace", "new", "research"], 0);
    let done = machine.json(
        &repo,
        &[
            "task",
            "start",
            "--execute",
            "--workspace",
            "research",
            "--file",
            "gc-older.json",
            "--json",
        ],
    );
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    let delivered = machine.root().join("delivered");
    let output = machine.af(
        &repo,
        &[
            "task",
            "deliver",
            "gc-older",
            "--workspace",
            "research",
            "--branch",
            "af/gc-older",
            "--worktree",
            delivered.to_str().unwrap(),
            "--confirm",
            "gc-older",
            "--json",
        ],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{stderr}");
    assert_eq!(stdout.lines().count(), 1, "one receipt: {stdout}");
    let receipt: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(receipt["schema"], "af/task-delivery@1");
    let keys: Vec<&String> = receipt.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "schema",
            "delivery_id",
            "task_id",
            "result_id",
            "source_snapshot_id",
            "derived_snapshot_id",
            "target",
            "outcome",
            "ignored_paths",
            "remote_actions"
        ]
    );
    assert_eq!(receipt["target"]["worktree"], delivered.to_str().unwrap());

    let listed = machine.json(&repo, &["project", "workspaces", "--json"]);
    let entry = workspace(&listed, "gc-older");
    assert_eq!(entry["origin"], "delivered");
    assert_eq!(entry["path"], delivered.to_str().unwrap());
    assert_eq!(
        entry["state"],
        default_store(&machine, &delivered).to_str().unwrap()
    );
    assert!(entry.get("bindings").is_none());
}

/// The best of several runs, so a loaded machine does not decide the answer.
fn fastest(machine: &Machine, args: &[&str]) -> std::time::Duration {
    let directory = machine.root();
    (0..10)
        .map(|_| {
            let started = std::time::Instant::now();
            let output = machine.af(&directory, args);
            let elapsed = started.elapsed();
            assert!(output.status.success(), "{:?}", text(&output));
            elapsed
        })
        .min()
        .unwrap()
}

#[test]
fn af_help_and_af_project_help_answer_in_under_50_ms() {
    let machine = Machine::new();
    for args in [["--help"].as_slice(), ["project", "--help"].as_slice()] {
        let elapsed = fastest(&machine, args);
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "af {args:?} took {elapsed:?}"
        );
    }
    assert!(!machine.projects().exists(), "help registers nothing");
}

/// What a refusal of `workspace new` must leave as it found: the worktrees, every ref, the
/// Project Store's Workspace directories, and the checkout's files.
#[derive(Debug, PartialEq, Eq)]
struct RepositoryState {
    worktrees: String,
    refs: String,
    directories: Vec<PathBuf>,
    files: Vec<(PathBuf, Vec<u8>)>,
}

fn repository_state(machine: &Machine, repo: &Path) -> RepositoryState {
    let directories = std::fs::read_dir(machine.projects())
        .map(|stores| {
            let mut found: Vec<PathBuf> = stores
                .flat_map(|store| {
                    std::fs::read_dir(store.unwrap().path().join("workspaces"))
                        .into_iter()
                        .flatten()
                        .map(|entry| entry.unwrap().path())
                })
                .collect();
            found.sort();
            found
        })
        .unwrap_or_default();
    RepositoryState {
        worktrees: git(repo, &["worktree", "list", "--porcelain"]),
        refs: git(repo, &["for-each-ref"]),
        directories,
        files: files(repo),
    }
}

/// Every file below `root` by its path relative to `root`, with its bytes.
fn relative_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    files(root)
        .into_iter()
        .map(|(path, bytes)| (path.strip_prefix(root).unwrap().to_path_buf(), bytes))
        .collect()
}

#[test]
fn workspace_new_writes_the_commits_tree_without_running_a_checkout_filter() {
    use std::os::unix::fs::PermissionsExt;
    let machine = Machine::new();
    let repo = repository(&machine.root());
    std::fs::write(repo.join(".gitattributes"), "*.txt filter=marker\n").unwrap();
    std::fs::write(repo.join("data.txt"), "filtered on checkout\n").unwrap();
    std::fs::write(repo.join("run.sh"), "#!/bin/sh\necho run\n").unwrap();
    std::fs::set_permissions(repo.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("README.md", repo.join("link")).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "attributes"]);
    // The filter is the checkout's own configuration: any checkout of `data.txt` runs it.
    let marker = machine.root().join("smudged");
    git(
        &repo,
        &[
            "config",
            "filter.marker.smudge",
            &format!("touch '{}' && cat", marker.display()),
        ],
    );
    let control = machine.root().join("control");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            control.to_str().unwrap(),
        ],
    );
    assert!(
        marker.is_file(),
        "the fixture's filter runs on a git checkout"
    );
    git(&repo, &["worktree", "remove", control.to_str().unwrap()]);
    std::fs::remove_file(&marker).unwrap();

    machine.expect(&repo, &["project", "workspace", "new", "filtered"], 0);
    let worktree = machine.root().join("fixture-wt-filtered");
    assert!(
        !marker.exists(),
        "workspace new ran the checkout's smudge filter"
    );
    assert_eq!(relative_files(&worktree), relative_files(&repo));
    let mode = std::fs::metadata(worktree.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "the executable bit of the commit");
    assert_eq!(
        std::fs::read_link(worktree.join("link")).unwrap(),
        Path::new("README.md")
    );
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD^{tree}"]),
        git(&repo, &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
    assert!(!marker.exists());
}

#[test]
fn workspace_new_refuses_a_path_inside_the_checkout_a_workspace_or_afs_state() {
    let machine = Machine::new();
    let repo = repository(&machine.root());
    machine.expect(&repo, &["project", "workspace", "new", "research"], 0);
    let research = machine.root().join("fixture-wt-research");
    let inside_state = machine.state_home().join("af/worktree");
    let before = repository_state(&machine, &repo);
    let research_before = files(&research);
    for (path, reason) in [
        (".af/research", "inside the checkout"),
        (".", "exactly the checkout"),
        (
            "../fixture-wt-research/nested",
            "inside the Workspace `research`",
        ),
        (
            inside_state.to_str().unwrap(),
            "inside af's state directory",
        ),
    ] {
        let output = machine.af(
            &repo,
            &["project", "workspace", "new", "spike", "--path", path],
        );
        let (stdout, stderr) = text(&output);
        assert_eq!(
            output.status.code(),
            Some(2),
            "--path {path}\n{stdout}\n{stderr}"
        );
        assert!(stderr.contains(reason), "--path {path}: {stderr}");
        assert!(stderr.contains("nothing was created"), "{stderr}");
        assert_eq!(repository_state(&machine, &repo), before, "--path {path}");
        assert_eq!(files(&research), research_before, "--path {path}");
    }
    assert!(!repo.join(".af/research").exists());
    assert!(!inside_state.exists());
    let listed = machine.json(&repo, &["project", "workspaces", "--json"]);
    assert_eq!(labels(&listed), ["fixture", "research"]);
}

#[test]
fn a_refused_workspace_new_leaves_nothing_behind_and_the_same_command_then_succeeds() {
    let machine = Machine::new();
    let repo = repository(&machine.root());
    machine.expect(&repo, &["project", "show"], 0);
    // An orphan branch: a commit of the same tree without a parent, so its root is another.
    let orphan = git(&repo, &["commit-tree", "HEAD^{tree}", "-m", "orphan"]);
    git(&repo, &["update-ref", "refs/heads/orphan", &orphan]);
    let store = machine
        .projects()
        .join(expected_project_id(&repo).trim_start_matches("sha256:"));
    let before = repository_state(&machine, &repo);

    let refused = machine.af(
        &repo,
        &["project", "workspace", "new", "spike", "--from", "orphan"],
    );
    let (_, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&orphan), "{stderr}");
    assert!(stderr.contains("nothing was created"), "{stderr}");
    assert_eq!(repository_state(&machine, &repo), before);
    assert!(!store.join("workspaces/spike").exists());
    assert!(!machine.root().join("fixture-wt-spike").exists());

    // A failure after the first mutation undoes, newest first, what was made.
    let file = machine.root().join("a-file");
    std::fs::write(&file, "not a directory\n").unwrap();
    let failed = machine.af(
        &repo,
        &[
            "project",
            "workspace",
            "new",
            "spike",
            "--path",
            file.join("worktree").to_str().unwrap(),
        ],
    );
    let (_, stderr) = text(&failed);
    assert_eq!(failed.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("deleted the branch refs/heads/af/spike"),
        "{stderr}"
    );
    assert!(stderr.contains("nothing was left behind"), "{stderr}");
    assert_eq!(repository_state(&machine, &repo), before);
    assert!(!store.join("workspaces/spike").exists());

    let created = machine.expect(
        &repo,
        &["project", "workspace", "new", "spike", "--from", "HEAD"],
        0,
    );
    assert!(
        created.starts_with("created Workspace spike\n"),
        "{created}"
    );
    let spike = machine.root().join("fixture-wt-spike");
    assert_eq!(
        git(&spike, &["rev-parse", "HEAD"]),
        git(&repo, &["rev-parse", "HEAD"])
    );
    assert!(store.join("workspaces/spike/state").is_dir());
}

/// A fake `git` that passes every command to the real one except `worktree add`, which first
/// makes what git makes before it checks anything out (the worktree directory, its `gitdir:`
/// `.git` file and a locked entry under `<gitdir>/worktrees/`) and then runs `then`.
fn partial_worktree_git(bin: &Path, repo: &Path, then: &str) -> PathBuf {
    worktree_add_git(
        bin,
        repo,
        "mkdir -p \"$target\" \"$entry\"\n  echo initializing > \"$entry/locked\"\n  echo \
         \"$target/.git\" > \"$entry/gitdir\"\n  echo \"gitdir: $entry\" > \"$target/.git\"",
        then,
    )
}

/// A fake `git` that passes every command to the real one except `worktree add`, which runs the
/// shell `creates` (with `$target`, the worktree path, and `$entry`, its administrative entry
/// under `<gitdir>/worktrees/`) and then `then`.
fn worktree_add_git(bin: &Path, repo: &Path, creates: &str, then: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    // Baked in by its absolute path: af's isolated git inherits only `PATH`, which names the fake.
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    assert!(Path::new(&real).is_absolute(), "git is {real:?}");
    let admin = repo.join(".git/worktrees");
    std::fs::create_dir_all(bin).unwrap();
    let fake = bin.join("git");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n[ \"$1\" = warm ] && exit 0\nprevious=\nadding=\ntarget=\nfor argument in \
             \"$@\"; do\n  if [ \"$adding\" = after ]; then target=$argument; break; fi\n  [ \
             \"$previous\" = worktree ] && [ \"$argument\" = add ] && adding=yes\n  [ \"$adding\" \
             = yes ] && [ \"$argument\" = -- ] && adding=after\n  previous=$argument\ndone\nif [ \
             -n \"$target\" ]; then\n  entry='{admin}'/$(basename \"$target\")\n  \
             {creates}\n  {then}\nfi\nexec '{real}' \"$@\"\n",
            admin = admin.display(),
            real = real,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The first exec of a script just written can take as long as the deadline (macOS scans it).
    assert!(Command::new(&fake).arg("warm").status().unwrap().success());
    fake
}

#[test]
fn a_worktree_add_that_fails_or_is_stopped_midway_is_undone_and_the_same_command_then_succeeds() {
    let machine = Machine::new();
    let repo = repository(&machine.root());
    machine.expect(&repo, &["project", "show"], 0);
    let store = machine
        .projects()
        .join(expected_project_id(&repo).trim_start_matches("sha256:"));
    let worktree = machine.root().join("fixture-wt-spike");
    let before = repository_state(&machine, &repo);
    for (case, then) in [("fails", "exit 128"), ("is stopped", "sleep 120")] {
        let bin = machine
            .root()
            .join(format!("bin-{}", case.replace(' ', "-")));
        partial_worktree_git(&bin, &repo, then);
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = crate::common::af()
            .current_dir(&repo)
            .env("XDG_STATE_HOME", machine.state_home())
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1")
            .env("PATH", path)
            .env("AF_TEST_PROJECT_GIT_DEADLINE_MS", "3000")
            .args(["project", "workspace", "new", "spike"])
            .output()
            .unwrap();
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(1), "{case}\n{stdout}\n{stderr}");
        assert!(
            stderr.contains(&format!("removed the worktree {}", worktree.display())),
            "{case}: {stderr}"
        );
        assert!(
            stderr.contains("deleted the branch refs/heads/af/spike"),
            "{case}: {stderr}"
        );
        assert!(
            stderr.contains("nothing was left behind"),
            "{case}: {stderr}"
        );
        if case == "is stopped" {
            assert!(stderr.contains("deadline of 3s"), "{case}: {stderr}");
        }
        assert_eq!(repository_state(&machine, &repo), before, "{case}");
        let entries = std::fs::read_dir(repo.join(".git/worktrees"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(entries, 0, "{case}: an administrative entry survived");
        assert!(!worktree.exists(), "{case}");
        assert!(!store.join("workspaces/spike").exists(), "{case}");
    }

    let created = machine.expect(&repo, &["project", "workspace", "new", "spike"], 0);
    assert!(
        created.starts_with("created Workspace spike\n"),
        "{created}"
    );
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD"]),
        git(&repo, &["rev-parse", "HEAD"])
    );
    assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
    assert!(store.join("workspaces/spike/state").is_dir());
}

#[test]
fn a_worktree_add_stopped_before_it_made_the_path_leaves_no_administrative_entry() {
    let machine = Machine::new();
    let repo = repository(&machine.root());
    machine.expect(&repo, &["project", "show"], 0);
    let store = machine
        .projects()
        .join(expected_project_id(&repo).trim_start_matches("sha256:"));
    let worktree = machine.root().join("fixture-wt-spike");
    let before = repository_state(&machine, &repo);
    let listed = git(&repo, &["worktree", "list", "--porcelain"]);
    // What git makes first: the entry, locked while it initializes, then its `gitdir` and `HEAD`;
    // the path comes later. Git neither lists nor prunes the locked entry without a `gitdir`.
    let locked = "mkdir -p \"$entry\"\n  echo initializing > \"$entry/locked\"";
    let with_gitdir = format!(
        "{locked}\n  echo \"$target/.git\" > \"$entry/gitdir\"\n  echo \
         'ref: refs/heads/af/spike' > \"$entry/HEAD\""
    );
    for (case, creates, then) in [
        ("locked", locked, "exit 128"),
        ("with-gitdir", with_gitdir.as_str(), "exit 128"),
        ("stopped", locked, "sleep 120"),
    ] {
        let bin = machine.root().join(format!("bin-{case}"));
        worktree_add_git(&bin, &repo, creates, then);
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = crate::common::af()
            .current_dir(&repo)
            .env("XDG_STATE_HOME", machine.state_home())
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1")
            .env("PATH", path)
            .env("AF_TEST_PROJECT_GIT_DEADLINE_MS", "3000")
            .args(["project", "workspace", "new", "spike"])
            .output()
            .unwrap();
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(1), "{case}\n{stdout}\n{stderr}");
        assert!(
            stderr.contains("removed the administrative entry worktrees/fixture-wt-spike"),
            "{case}: {stderr}"
        );
        assert!(
            stderr.contains("deleted the branch refs/heads/af/spike"),
            "{case}: {stderr}"
        );
        assert!(
            stderr.contains("nothing was left behind"),
            "{case}: {stderr}"
        );
        if case == "stopped" {
            assert!(stderr.contains("deadline of 3s"), "{case}: {stderr}");
        }
        let entries = std::fs::read_dir(repo.join(".git/worktrees"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(entries, 0, "{case}: an administrative entry survived");
        assert_eq!(
            git(&repo, &["worktree", "list", "--porcelain"]),
            listed,
            "{case}"
        );
        assert_eq!(repository_state(&machine, &repo), before, "{case}");
        assert!(!worktree.exists(), "{case}");
        assert!(!store.join("workspaces/spike").exists(), "{case}");
    }

    let created = machine.expect(&repo, &["project", "workspace", "new", "spike"], 0);
    assert!(
        created.starts_with("created Workspace spike\n"),
        "{created}"
    );
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD"]),
        git(&repo, &["rev-parse", "HEAD"])
    );
    assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
    assert!(store.join("workspaces/spike/state").is_dir());
}

#[test]
fn a_git_that_outlives_its_deadline_fails_the_command_and_leaves_no_child() {
    use std::os::unix::fs::PermissionsExt;
    let machine = Machine::new();
    let repo = repository(&machine.root());
    let bin = machine.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let pids = machine.root().join("pids");
    let fake = bin.join("git");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n[ \"$1\" = warm ] && exit 0\nsleep 120 &\necho $$ $! >> '{}'\nwait\n",
            pids.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The first exec of a script just written can take as long as the deadline (macOS scans it),
    // so the fake could be stopped before it records its processes: start it once beforehand.
    assert!(Command::new(&fake).arg("warm").status().unwrap().success());
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let started = std::time::Instant::now();
    let output = crate::common::af()
        .current_dir(&repo)
        .env("XDG_STATE_HOME", machine.state_home())
        .env("AF_SELF_OFFLINE", "1")
        .env("NO_COLOR", "1")
        .env("PATH", path)
        .env("AF_TEST_PROJECT_GIT_DEADLINE_MS", "2000")
        .args(["project", "show"])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(stderr.contains("deadline of 2s"), "{stderr}");
    // The fake git holds for two minutes; only the deadline ends the command sooner. The bound
    // leaves room for a loaded machine's first launch of a freshly linked `af`.
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "the command took {elapsed:?}"
    );
    let started: Vec<String> = std::fs::read_to_string(&pids)
        .unwrap()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    assert!(!started.is_empty());
    let alive = |pid: &str| {
        Command::new("kill")
            .args(["-0", pid])
            .output()
            .unwrap()
            .status
            .success()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while started.iter().any(|pid| alive(pid)) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    for pid in &started {
        assert!(!alive(pid), "the fake git's process {pid} survived");
    }
    assert!(!machine.projects().exists(), "nothing was registered");
}

#[test]
fn the_first_task_plan_with_the_checkouts_own_label_succeeds() {
    let machine = Machine::new();
    let (repo, _) = crate::task_gc::fixture(&machine.root());
    assert!(!machine.projects().exists(), "no Project Store yet");
    let planned = machine.json(
        &repo,
        &[
            "task",
            "plan",
            "--workspace",
            "repo",
            "--file",
            "gc-older.json",
            "--json",
        ],
    );
    assert!(planned["revision_id"].is_string(), "{planned}");
    assert!(
        default_store(&machine, &repo)
            .join("events.sqlite")
            .is_file(),
        "`--workspace repo` is the checkout's default Task Store"
    );
    let listed = machine.json(&repo, &["project", "workspaces", "--json"]);
    assert_eq!(labels(&listed), ["repo"]);
}

#[test]
fn no_project_command_and_no_workspace_plan_writes_inside_the_checkout() {
    let machine = Machine::new();
    let (repo, _) = crate::task_gc::fixture(&machine.root());
    let other = machine.root().join("repo-other");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            other.to_str().unwrap(),
        ],
    );
    let listing = |root: &Path| {
        let mut names: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        (names, files(root))
    };
    let clean = |when: &str| {
        assert_eq!(
            git(&repo, &["status", "--porcelain", "--ignored"]),
            "",
            "after {when}"
        );
    };
    clean("the fixture");
    let before = listing(&repo);
    for args in [
        &["project", "show"][..],
        &["project", "show", "--json"],
        &["project", "workspaces"],
        &["project", "workspaces", "--json"],
        &["project", "workspace", "new", "research"],
        &[
            "project",
            "workspace",
            "add",
            other.to_str().unwrap(),
            "--label",
            "other",
        ],
        &["project", "workspace", "remove", "other"],
        &["project", "workspace", "new", "spike", "--json"],
        &[
            "project",
            "workspace",
            "remove",
            "spike",
            "--delete-worktree",
        ],
        &[
            "task",
            "plan",
            "--workspace",
            "repo",
            "--file",
            "gc-older.json",
        ],
        &[
            "task",
            "plan",
            "--workspace",
            "research",
            "--file",
            "gc-newer.json",
        ],
    ] {
        machine.expect(&repo, args, 0);
        clean(&args.join(" "));
        assert_eq!(listing(&repo), before, "after af {}", args.join(" "));
    }
    assert!(!machine.root().join("repo-wt-spike").exists());
    assert!(machine.root().join("repo-wt-research").is_dir());
}
