//! ADR-0151 end to end: one Project per repository, its Workspaces, and `af project`.
//!
//! Two worktrees of one repository register one Project with two Workspaces, and `af project
//! show` answers the same from either; a shallow clone is refused with the way out; a repository
//! without commits registers nothing. Adopting a default Task Store changes none of its bytes.
//! `--workspace` plans exactly what the explicit `--state` and `--bindings` plan.
//! `af project workspace new|remove` create and retire Workspaces without a commit and without
//! removing a Store, and delivery registers the worktree it made. No command writes inside a
//! checkout, and the help answers in under 50 ms.
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
