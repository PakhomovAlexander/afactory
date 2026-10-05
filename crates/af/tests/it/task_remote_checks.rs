//! Remote Checks through the real CLI (ADR-0139): the pipeline's check node chooses where a
//! check runs, and the plan says so. The `pagination` fixture declares `[checks.pagination.remote]`
//! and gains a remote twin of its pipeline, `fixture/implementation-remote`, whose check node
//! lists `pagination` in `remote_checks`. The operator's mapping only names a push target: a real
//! `git` pushes to a local bare repository, and the fake `gh` of `fixtures/remote-checks/` serves
//! the recorded GitHub documents. Credential-free and offline.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::task_cli;

const TASK: &str = "pagination-remote";
const REMOTE: &str = "fixture/implementation-remote";
const LOCAL: &str = "fixture/implementation";
const FAKE_GH: &str = include_str!("../../../../fixtures/remote-checks/fake-gh.sh");
const KNOB: &str = "AF_TASK_REMOTE_CHECK_POLICY_FILE or $XDG_CONFIG_HOME/af/remote-checks.toml";

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    bare: PathBuf,
    gh_state: PathBuf,
    bin: PathBuf,
    /// The operator's mapping, outside the repository and the Store.
    mapping: PathBuf,
    repository_id: String,
}

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// The pagination fixture with a declared remote form and a remote twin of its pipeline that
/// differs from the original only in its name and its two check lists.
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let (repo, state) = task_cli::fixture_named(&root, "pagination");
    let policy = repo.join(".af/code-policy.toml");
    let text = std::fs::read_to_string(&policy).unwrap();
    // The remote phase shares the check Attempt's wall with the local checks.
    let text = text.replace("check_wall_ms = 5000", "check_wall_ms = 120000");
    std::fs::write(
        &policy,
        format!(
            "{text}\n[checks.pagination.remote]\nexecutor = \"github-pr\"\n\
             workflow = \".github/workflows/ci.yml\"\n\
             required = [\"validation / lint\", \"validation / check (ubuntu-latest)\"]\n"
        ),
    )
    .unwrap();
    let local = repo.join(".af/task-packages/fixture/implementation/pipeline.toml");
    let original = std::fs::read_to_string(&local).unwrap();
    let twin = original
        .replace(
            &format!("name = \"{LOCAL}\""),
            &format!("name = \"{REMOTE}\""),
        )
        .replace(
            "checks = [\"pagination\"]",
            "checks = []\nremote_checks = [\"pagination\"]",
        );
    assert_eq!(twin.lines().count(), original.lines().count() + 1);
    let package = repo.join(".af/task-packages/fixture/implementation-remote");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("pipeline.toml"), twin).unwrap();
    let digest = review_config::lock::package_digest(REMOTE, &package).unwrap();
    let catalog = repo.join(".af/task-catalog.toml");
    let mut text = std::fs::read_to_string(&catalog).unwrap();
    text.push_str(&format!(
        "\n[packages.\"{REMOTE}\"]\nversion = \"1.0.0\"\ndigest = \"{digest}\"\n\
         path = \".af/task-packages/{REMOTE}\"\n"
    ));
    std::fs::write(&catalog, text).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "remote twin"]);
    let repository_id = git(&repo, &["rev-list", "--max-parents=0", "HEAD"]);
    let bare = root.join("remote.git");
    git(
        &root,
        &["init", "--bare", "--quiet", bare.to_str().unwrap()],
    );
    let gh_state = root.join("gh-state");
    let bin = root.join("bin");
    std::fs::create_dir_all(&gh_state).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        FAKE_GH
            .replace("@STATE@", gh_state.to_str().unwrap())
            .replace("@BARE@", bare.to_str().unwrap())
            .replace("@TASK@", TASK),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::create_dir_all(root.join("tasks")).unwrap();
    Fixture {
        mapping: root.join("config/remote-checks.toml"),
        _root: directory,
        root,
        repo,
        state,
        bare,
        gh_state,
        bin,
        repository_id,
    }
}

impl Fixture {
    /// A mapping naming this machine's push target for the fixture repository.
    fn map(&self, extra: &str) {
        std::fs::write(
            &self.mapping,
            format!(
                "version = 1\n\n[[github_pr]]\nrepository_id = \"{}\"\ngithub = \"octo/gate\"\n\
                 push_url = \"{}\"\n{extra}",
                self.repository_id,
                self.bare.display()
            ),
        )
        .unwrap();
    }

    /// Serve one recorded GitHub document under `name` in the fake's state. Written, never
    /// copied, so a read-only source tree cannot leave a read-only document behind.
    fn serve(&self, document: &str, name: &str) {
        let bytes = std::fs::read(
            workspace()
                .join("fixtures/remote-checks/github")
                .join(document),
        )
        .unwrap();
        std::fs::write(self.gh_state.join(name), bytes).unwrap();
    }

    fn task_file(&self, task_id: &str, pipeline: &str) -> PathBuf {
        let file = self.root.join("tasks").join(format!("{task_id}.json"));
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({
                "schema": "af.task-file/1",
                "task_id": task_id,
                "kind": "implement",
                "goal": "Implement this Jira ticket: offset/limit pagination",
                "pipeline": {"name": pipeline, "fallback": "refuse"},
                "strategy": "small",
                "facts": {},
                "limits": {
                    "tokens": 1000,
                    "max_attempts": 3,
                    "wall_ms": 600_000,
                    "verification": {"tokens": 200, "attempts": 2, "wall_ms": 300_000}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        file
    }

    fn af(&self, state: &Path, args: &[&str]) -> (i32, String, String) {
        let home = self.root.join("home");
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone()).chain(std::env::split_paths(&inherited)),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&self.repo)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("AF_TASK_REMOTE_CHECK_POLICY_FILE", &self.mapping)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("PATH", path)
            .env_remove("AF_CACHE_POLICY_FILE")
            .env_remove("CARGO_TARGET_DIR")
            .args(args)
            .arg("--state")
            .arg(state)
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// `af task plan` of `pipeline` into `state`: the preview text and the `--json` document.
    fn plan(&self, state: &Path, task_id: &str, pipeline: &str) -> (i32, String, String) {
        let file = self.task_file(task_id, pipeline);
        self.af(state, &["task", "plan", "--file", file.to_str().unwrap()])
    }

    fn plan_json(&self, state: &Path, task_id: &str, pipeline: &str) -> Value {
        let file = self.task_file(task_id, pipeline);
        let (code, stdout, stderr) = self.af(
            state,
            &["task", "plan", "--file", file.to_str().unwrap(), "--json"],
        );
        assert_eq!(code, 0, "{stderr}\n{stdout}");
        serde_json::from_str(stdout.trim()).unwrap()
    }

    /// Every byte the Store holds, as one haystack.
    fn store_bytes(&self, state: &Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut stack = vec![state.to_path_buf()];
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    stack.push(entry.unwrap().path());
                }
            } else if let Ok(read) = std::fs::read(&path) {
                bytes.extend(read);
            }
        }
        bytes
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn line<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix} line in:\n{text}"))
}

#[test]
fn a_remote_pipeline_says_it_publishes_and_then_runs_through_the_gate_pull_request() {
    let fixture = fixture();
    fixture.map("");
    // The preview names the effect and the destination; confirming this plan is the consent.
    let (code, preview, stderr) = fixture.plan(&fixture.state, TASK, REMOTE);
    assert_eq!(code, 0, "{stderr}\n{preview}");
    assert_eq!(
        line(&preview, "EFFECTS "),
        "EFFECTS execute-checks, publish-gate, read-source, write-source"
    );
    assert_eq!(line(&preview, "SEND  "), "SEND  github:octo/gate");
    let (code, explained, stderr) =
        fixture.af(&fixture.state, &["task", "explain", TASK, "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let explained: Value = serde_json::from_str(explained.trim()).unwrap();
    assert_eq!(explained["attempts"], 0, "planning starts no Attempt");
    assert_eq!(
        explained["plan"]["authority"]["allowed_effects"],
        json!([
            "execute-checks",
            "publish-gate",
            "read-source",
            "write-source"
        ])
    );
    assert_eq!(
        explained["plan"]["authority"]["data_destinations"],
        json!(["github:octo/gate"])
    );
    let (code, tree, stderr) = fixture.af(&fixture.state, &["task", "explain", TASK, "--tree"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(tree.contains("remote checks: pagination"), "{tree}");
    assert!(tree.contains("SEND  github:octo/gate"), "{tree}");

    // The run pushes the two gate branches and records observed evidence, as in RC1.
    fixture.serve("runs-pull-request.json", "runs.json");
    fixture.serve("jobs-success.json", "jobs-77-1.json");
    let plan_id = explained["plan_id"].as_str().unwrap();
    let (code, stdout, stderr) = fixture.af(
        &fixture.state,
        &["task", "run", TASK, "--confirm-plan", plan_id, "--json"],
    );
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    let done: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    let [remote] = done["remote_checks"].as_array().unwrap().as_slice() else {
        panic!("one remote check: {done}");
    };
    assert_eq!(remote["check"], "pagination", "{remote}");
    assert_eq!(remote["status"], "passed", "{remote}");
    assert_eq!(remote["record"]["state"], "observed", "{remote}");
    assert_eq!(remote["record"]["github"], "octo/gate");
    let branches = git(
        &fixture.bare,
        &["for-each-ref", "--format=%(refname)", "refs/heads/"],
    );
    assert_eq!(
        branches,
        format!("refs/heads/af-gate/{TASK}/base\nrefs/heads/af-gate/{TASK}/head")
    );
    // No record holds the push URL or the mapping's path.
    let bytes = fixture.store_bytes(&fixture.state);
    assert!(!contains(&bytes, fixture.bare.to_str().unwrap()));
    assert!(!contains(&bytes, fixture.mapping.to_str().unwrap()));
    assert!(!stdout.contains(fixture.bare.to_str().unwrap()));
    // Nothing the kernel ran forced a push.
    let calls = std::fs::read_to_string(fixture.gh_state.join("calls.log")).unwrap();
    assert!(!calls.contains("--force") && !calls.contains(" +"));
}

#[test]
fn a_remote_pipeline_without_a_target_is_refused_before_any_attempt() {
    let fixture = fixture();
    let cases = [
        ("absent", None),
        ("another repository", Some(String::new())),
        (
            "a mapping that still selects checks",
            Some("checks = [\"pagination\"]\n".to_string()),
        ),
    ];
    for (why, mapping) in cases {
        match &mapping {
            None => {
                let _ = std::fs::remove_file(&fixture.mapping);
            }
            Some(extra) if why == "another repository" => {
                fixture.map(extra);
                let text = std::fs::read_to_string(&fixture.mapping).unwrap();
                std::fs::write(
                    &fixture.mapping,
                    text.replace(&fixture.repository_id, &"7".repeat(40)),
                )
                .unwrap();
            }
            Some(extra) => fixture.map(extra),
        }
        let state = fixture
            .root
            .join(format!("state-{}", why.replace(' ', "-")));
        let (code, stdout, stderr) = fixture.plan(&state, TASK, REMOTE);
        assert_eq!(code, 1, "{why}: {stderr}\n{stdout}");
        let said = format!("{stdout}\n{stderr}");
        assert!(said.contains(KNOB), "{why}: names the knob\n{said}");
        if why == "a mapping that still selects checks" {
            assert!(
                said.contains("carries `checks`") && said.contains("pipeline's check node"),
                "{said}"
            );
        } else {
            assert!(said.contains("names no push target"), "{why}: {said}");
            assert!(
                said.contains(&fixture.repository_id),
                "{why}: names the repository\n{said}"
            );
        }
        assert!(!said.contains(fixture.mapping.to_str().unwrap()), "{said}");
        assert!(!said.contains(fixture.bare.to_str().unwrap()), "{said}");
        let refused: Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(refused["attempts"], 0, "{why}");
        assert!(!contains(
            &fixture.store_bytes(&state),
            fixture.mapping.to_str().unwrap()
        ));
    }
    assert_eq!(
        git(&fixture.bare, &["for-each-ref", "--format=%(refname)"]),
        "",
        "nothing was pushed"
    );
}

#[test]
fn a_local_pipeline_plans_exactly_as_before_whatever_the_mapping_holds() {
    let fixture = fixture();
    let mut plans = Vec::new();
    for (why, mapping) in [
        ("no mapping", None),
        ("a target for this repository", Some("")),
        (
            "a mapping that is not even valid",
            Some("checks = [\"pagination\"]\n"),
        ),
    ] {
        match mapping {
            None => {
                let _ = std::fs::remove_file(&fixture.mapping);
            }
            Some(extra) => fixture.map(extra),
        }
        let state = fixture
            .root
            .join(format!("state-{}", why.replace(' ', "-")));
        let (code, preview, stderr) = fixture.plan(&state, "pagination-local", LOCAL);
        assert_eq!(code, 0, "{why}: {stderr}\n{preview}");
        assert_eq!(
            line(&preview, "EFFECTS "),
            "EFFECTS execute-checks, read-source, write-source",
            "{why}"
        );
        assert_eq!(line(&preview, "SEND  "), "SEND  none", "{why}");
        let planned = fixture.plan_json(&state.join("json"), "pagination-local", LOCAL);
        let mut plan = planned["plan"].clone();
        // The deadline is the only clock in a plan, and the revision and graph identities
        // carry it; everything else is byte for byte the plan this package found.
        for key in ["task_revision_id", "compiled_graph_id"] {
            plan.as_object_mut().unwrap().remove(key);
        }
        plan["limits"]
            .as_object_mut()
            .unwrap()
            .remove("deadline_unix_ms");
        plans.push((why, plan));
    }
    let (_, first) = &plans[0];
    assert_eq!(
        first["authority"]["data_destinations"],
        json!([]),
        "SEND none"
    );
    for (why, plan) in &plans[1..] {
        assert_eq!(plan, first, "{why}");
    }
}

/// The staged remote twins of this repository's pipelines (RC3 deliverable 6), each with the
/// local original it twins.
const STAGED: &str = "fixtures/remote-checks/packages";
const TWINS: [(&str, &str); 4] = [
    ("kernel/gate-bench-remote", "kernel/gate-bench"),
    (
        "kernel/implementation-reviewed-remote",
        "kernel/implementation-reviewed",
    ),
    (
        "kernel/verification-reviewed-remote",
        "kernel/verification-reviewed",
    ),
    ("kernel/review-code-remote", "kernel/review-code"),
];

/// The original's bytes with `kernel` moved from `checks` to `remote_checks`, the twin's own
/// name, and a call of the twin child where the original calls the local one.
fn twin_of(original: &str, local: &str, remote: &str) -> String {
    original
        .replacen(
            &format!("name = \"{local}\"\n"),
            &format!("name = \"{remote}\"\n"),
            1,
        )
        .replace(
            "checks = [\"kernel\", \"markdownlint\"]\n",
            "checks = [\"markdownlint\"]\nremote_checks = [\"kernel\"]\n",
        )
        .replace(
            "pipeline = \"kernel/review-code\"\n",
            "pipeline = \"kernel/review-code-remote\"\n",
        )
}

/// Install the staged packages into `repo`'s `.af/` exactly as their README says: append the
/// `remote` table to the code policy, copy each package, and add each pin the catalog lacks.
fn install_remote_twins(repo: &Path) {
    let staged = workspace().join(STAGED);
    let policy_path = repo.join(".af/code-policy.toml");
    let policy = std::fs::read_to_string(&policy_path).unwrap();
    if !policy.contains("[checks.kernel.remote]") {
        let table = std::fs::read_to_string(staged.join("code-policy-remote.toml")).unwrap();
        std::fs::write(&policy_path, format!("{policy}\n{table}")).unwrap();
    }
    let fragment: toml::Value =
        toml::from_str(&std::fs::read_to_string(staged.join("catalog-fragment.toml")).unwrap())
            .unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog = std::fs::read_to_string(&catalog_path).unwrap();
    let installed: toml::Value = toml::from_str(&catalog).unwrap();
    for (name, _) in TWINS {
        let target = repo.join(".af/task-packages").join(name);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join("pipeline.toml"),
            std::fs::read(
                staged
                    .join("task-packages")
                    .join(name)
                    .join("pipeline.toml"),
            )
            .unwrap(),
        )
        .unwrap();
        let pin = &fragment["packages"][name];
        assert_eq!(
            pin["path"].as_str(),
            Some(format!(".af/task-packages/{name}").as_str())
        );
        match installed["packages"].get(name) {
            Some(existing) => assert_eq!(existing, pin, "{name}: the installed pin is staged"),
            None => catalog.push_str(&format!(
                "\n[packages.\"{name}\"]\nversion = \"{}\"\ndigest = \"{}\"\npath = \"{}\"\n",
                pin["version"].as_str().unwrap(),
                pin["digest"].as_str().unwrap(),
                pin["path"].as_str().unwrap()
            )),
        }
    }
    std::fs::write(&catalog_path, catalog).unwrap();
}

/// Every file under `directory` with its bytes.
fn tree_bytes(directory: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut stack = vec![directory.to_path_buf()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                stack.push(entry.unwrap().path());
            }
        } else {
            files.insert(
                path.strip_prefix(directory).unwrap().to_path_buf(),
                std::fs::read(&path).unwrap(),
            );
        }
    }
    files
}

/// A local command Worker standing in for `package` with its exact committed contract, so no
/// Provider is needed to plan.
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
fn the_staged_remote_twins_differ_only_where_they_run_kernel_and_plan_on_this_repositorys_policy() {
    let workspace = workspace();
    // Each twin is its committed original with its own name and `kernel` moved to
    // `remote_checks`; a parent calls the twin of its child. Nothing else differs.
    for (remote, local) in TWINS {
        let original = std::fs::read_to_string(
            workspace
                .join(".af/task-packages")
                .join(local)
                .join("pipeline.toml"),
        )
        .unwrap();
        let staged = std::fs::read_to_string(
            workspace
                .join(STAGED)
                .join("task-packages")
                .join(remote)
                .join("pipeline.toml"),
        )
        .unwrap();
        assert_eq!(staged, twin_of(&original, local, remote), "{remote}");
        assert_ne!(staged, original, "{remote}");
    }
    // The catalog fragment's pins are the staged bytes' digests, at the originals' versions.
    let fragment: toml::Value = toml::from_str(
        &std::fs::read_to_string(workspace.join(STAGED).join("catalog-fragment.toml")).unwrap(),
    )
    .unwrap();
    let committed: toml::Value =
        toml::from_str(&std::fs::read_to_string(workspace.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    assert_eq!(fragment["packages"].as_table().unwrap().len(), TWINS.len());
    for (remote, local) in TWINS {
        let pin = &fragment["packages"][remote];
        let digest = review_config::lock::package_digest(
            remote,
            &workspace.join(STAGED).join("task-packages").join(remote),
        )
        .unwrap();
        assert_eq!(pin["digest"].as_str(), Some(digest.as_str()), "{remote}");
        assert_eq!(
            pin["version"], committed["packages"][local]["version"],
            "{remote}"
        );
    }

    // Installing twice changes nothing.
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let repo = root.join("kernel");
    task_cli::copy_tree(&workspace.join(".af"), &repo.join(".af"));
    // The release pin would dispatch to (and install) another `af`; this test runs this one.
    std::fs::remove_file(repo.join(".af/af.lock")).unwrap();
    install_remote_twins(&repo);
    let once = tree_bytes(&repo.join(".af"));
    install_remote_twins(&repo);
    assert_eq!(
        tree_bytes(&repo.join(".af")),
        once,
        "a second install changes nothing"
    );
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "remote twins"]);
    let repository_id = git(&repo, &["rev-list", "--max-parents=0", "HEAD"]);

    // Each root twin plans against this repository's policy plus the `remote` table, with
    // zero Attempts, and its plan says it publishes to the mapped repository.
    let local = root.join("local");
    std::fs::create_dir_all(&local).unwrap();
    let mut packages = toml::map::Map::new();
    let mut slots = toml::map::Map::new();
    for (package, name) in [
        ("kernel/implementer", "local/implementer"),
        ("kernel/evaluator", "local/evaluator"),
        ("kernel/bugs", "local/bugs"),
        ("kernel/correctness", "local/correctness"),
    ] {
        packages.insert(name.into(), local_replacement(&repo, &local, package, name));
        let slot = name.trim_start_matches("local/");
        slots.insert(
            format!("root.slots.{slot}"),
            toml::Value::String(name.into()),
        );
    }
    let mapping = root.join("remote-checks.toml");
    std::fs::write(
        &mapping,
        format!(
            "version = 1\n\n[[github_pr]]\nrepository_id = \"{repository_id}\"\n\
             github = \"PakhomovAlexander/afactory\"\npush_url = \"{}\"\n",
            root.join("unused.git").display()
        ),
    )
    .unwrap();
    for (pipeline, kind, standard, bound) in [
        ("kernel/gate-bench-remote", "verification", false, &[][..]),
        (
            "kernel/implementation-reviewed-remote",
            "implement",
            true,
            &["implementer", "evaluator", "bugs", "correctness"][..],
        ),
        (
            "kernel/verification-reviewed-remote",
            "verification",
            true,
            &["evaluator", "bugs", "correctness"][..],
        ),
    ] {
        let mut bindings = toml::map::Map::new();
        bindings.insert(
            "schema".into(),
            toml::Value::String("af.task-bindings/1".into()),
        );
        let used = |name: &String| bound.iter().any(|slot| name.ends_with(&format!("/{slot}")));
        bindings.insert(
            "packages".into(),
            toml::Value::Table(
                packages
                    .iter()
                    .filter(|(name, _)| used(name))
                    .map(|(name, pin)| (name.clone(), pin.clone()))
                    .collect(),
            ),
        );
        bindings.insert(
            "slots".into(),
            toml::Value::Table(
                slots
                    .iter()
                    .filter(|(_, worker)| used(&worker.as_str().unwrap().to_string()))
                    .map(|(slot, worker)| (slot.clone(), worker.clone()))
                    .collect(),
            ),
        );
        let task_id = pipeline.trim_start_matches("kernel/");
        let bindings_path = local.join(format!("{task_id}.toml"));
        std::fs::write(&bindings_path, toml::to_string(&bindings).unwrap()).unwrap();
        let file = root.join(format!("{task_id}.json"));
        let mut task = json!({
            "schema": "af.task-file/1",
            "task_id": task_id,
            "kind": kind,
            "goal": "Hold this repository's gate on its own CI.",
            "pipeline": {"name": pipeline, "fallback": "refuse"},
            "strategy": "small",
            "facts": {},
            "limits": {
                "tokens": 3_000_000,
                "max_attempts": 16,
                "wall_ms": 36_000_000,
                "verification": {"tokens": 400_000, "attempts": 8, "wall_ms": 14_400_000}
            }
        });
        if standard {
            task["facts"]["standard"] = json!(true);
        }
        if kind == "implement" {
            task["verification"] = json!("review");
        }
        std::fs::write(&file, serde_json::to_vec(&task).unwrap()).unwrap();
        let home = root.join("home");
        let mut command = Command::new(env!("CARGO_BIN_EXE_af"));
        command
            .current_dir(&repo)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("AF_TASK_REMOTE_CHECK_POLICY_FILE", &mapping)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("AF_CACHE_POLICY_FILE")
            .env_remove("CARGO_TARGET_DIR")
            .args(["task", "plan", "--file", file.to_str().unwrap()]);
        if !bound.is_empty() {
            command.args(["--bindings", bindings_path.to_str().unwrap()]);
        }
        let output = command
            .args(["--state", root.join("state").to_str().unwrap(), "--json"])
            .output()
            .unwrap();
        let (stdout, stderr) = (
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(output.status.success(), "{pipeline}: {stderr}\n{stdout}");
        let planned: Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(planned["attempts"], 0, "{pipeline}");
        let authority = &planned["plan"]["authority"];
        assert!(
            authority["allowed_effects"]
                .as_array()
                .unwrap()
                .contains(&json!("publish-gate")),
            "{pipeline}: {authority}"
        );
        assert_eq!(
            authority["data_destinations"],
            json!(["github:PakhomovAlexander/afactory"]),
            "{pipeline}"
        );
    }
}
