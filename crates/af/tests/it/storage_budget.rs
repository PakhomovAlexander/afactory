//! ADR-0144 end to end: the Storage Budget through the real CLI, over a private HOME and private
//! XDG roots, so nothing here reads or writes the machine's own af directories, Claude config or
//! GitHub. `af storage` and `af storage prune` over planted entries; the free-disk floor through
//! the debug-only `AF_TEST_FREE_BYTES` hook; collection after `af task start --execute`; and the
//! Claude history a Worker Attempt and an admission probe leave, removed by the adapter.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::{schemas, task_cli, task_gc};

const AF: &str = env!("CARGO_BIN_EXE_af");
const DAY: Duration = Duration::from_secs(86_400);

/// A private machine: every root af reads lives below one temporary directory.
struct Machine {
    _root: tempfile::TempDir,
    root: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        for name in ["home", "config", "xstate", "data", "cache"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        Self {
            _root: directory,
            root,
        }
    }

    fn state_home(&self) -> PathBuf {
        self.root.join("xstate")
    }

    fn cache_home(&self) -> PathBuf {
        self.root.join("cache")
    }

    fn command(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Command {
        let mut command = Command::new(AF);
        command
            .current_dir(cwd)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.state_home())
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.cache_home())
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("AF_VERSION")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("AF_TEST_FREE_BYTES")
            .args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }

    fn af(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.command(cwd, env, args).output().unwrap()
    }
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn json(output: &Output) -> Value {
    let (stdout, stderr) = text(output);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("{stdout}\n{stderr}"))
}

/// Every file and directory below `path`, `path` included, last modified `age` ago.
fn age(path: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    let mut all = vec![path.to_path_buf()];
    let mut index = 0;
    while index < all.len() {
        if all[index].is_dir() {
            for entry in std::fs::read_dir(&all[index]).unwrap() {
                all.push(entry.unwrap().path());
            }
        }
        index += 1;
    }
    for path in all.iter().rev() {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
}

/// A warm toolchain key below the machine's cache with `bytes` of build output, last used
/// `old` ago.
fn warm_key(machine: &Machine, toolchain: char, bytes: usize, old: Duration) -> PathBuf {
    let project = machine
        .cache_home()
        .join("af/task-build-cache")
        .join("a".repeat(64));
    let key = project.join(toolchain.to_string().repeat(64));
    std::fs::create_dir_all(key.join("cargo_target")).unwrap();
    for path in [
        machine.cache_home().join("af/task-build-cache"),
        project.clone(),
        key.clone(),
        key.join("cargo_target"),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(key.join("cargo_target/blob"), vec![7_u8; bytes]).unwrap();
    std::fs::write(key.join("warm.lock"), b"").unwrap();
    age(&key, old);
    key
}

#[test]
fn af_storage_reports_what_af_holds_and_prune_previews_before_it_removes() {
    let machine = Machine::new();
    let old = warm_key(&machine, '1', 600 * 1024, 3 * DAY);
    let newer = warm_key(&machine, '2', 600 * 1024, 2 * DAY);
    let fresh = warm_key(&machine, '3', 600 * 1024, Duration::from_secs(5));

    let shown = json(&machine.af(&machine.root, &[], &["storage", "--json"]));
    schemas::valid(&schemas::validator("storage-v1.json"), &shown);
    assert_eq!(shown["max_bytes"], 20_u64 << 30);
    assert_eq!(shown["min_free_bytes"], 10_u64 << 30);
    let warm = shown["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["kind"] == "warm_key")
        .unwrap();
    assert_eq!(warm["entries"], 3);
    assert!(warm["bytes"].as_u64().unwrap() >= 3 * 600 * 1024, "{warm}");
    let listed = machine.af(&machine.root, &[], &["storage"]);
    let (stdout, _) = text(&listed);
    assert!(listed.status.success());
    assert!(stdout.contains("warm keys"), "{stdout}");
    assert!(stdout.contains("of a 20.0 GiB budget"), "{stdout}");
    assert!(stdout.contains("[storage] min_free_bytes"), "{stdout}");

    // 1 MiB of budget: the two older keys go, least recently used first; the fresh one stays.
    let budget = [("AF_STORAGE__MAX_BYTES", "1MiB")];
    let preview = json(&machine.af(&machine.root, &budget, &["storage", "prune", "--json"]));
    schemas::valid(&schemas::validator("storage-prune-v1.json"), &preview);
    assert_eq!(preview["applied"], false);
    let paths = |document: &Value| -> Vec<String> {
        document["removals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|removal| removal["path"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        paths(&preview),
        [old.display().to_string(), newer.display().to_string()]
    );
    assert!(old.exists() && newer.exists(), "a preview removes nothing");
    let applied = json(&machine.af(
        &machine.root,
        &budget,
        &["storage", "prune", "--apply", "--json"],
    ));
    schemas::valid(&schemas::validator("storage-prune-v1.json"), &applied);
    assert_eq!(applied["applied"], true);
    assert_eq!(paths(&applied), paths(&preview));
    assert_eq!(applied["stop"], "fits");
    assert!(!old.exists() && !newer.exists());
    assert!(fresh.exists(), "used within the hour");
    let (stdout, _) = text(&machine.af(&machine.root, &budget, &["storage", "prune"]));
    assert!(stdout.contains("fits"), "{stdout}");
    assert!(stdout.contains("run with --apply"), "{stdout}");
}

#[test]
fn a_repository_storage_table_changes_nothing_and_is_reported_as_ignored() {
    let machine = Machine::new();
    let (repo, _) = task_cli::fixture_named(&machine.root, "pagination");
    std::fs::write(
        repo.join(".af/af.toml"),
        "[storage]\nmax_bytes = \"1B\"\nauto_gc = false\n",
    )
    .unwrap();
    let shown = machine.af(&repo, &[], &["config", "show"]);
    let (stdout, stderr) = text(&shown);
    assert!(shown.status.success(), "{stderr}");
    assert!(
        stdout.contains("# ignored: [storage] in ") && stdout.contains("machine-only table"),
        "{stdout}"
    );
    assert!(stdout.contains("max_bytes = \"20GiB\""), "{stdout}");
    let storage = json(&machine.af(&repo, &[], &["storage", "--json"]));
    assert_eq!(storage["max_bytes"], 20_u64 << 30);
    // The machine's own layers do change it.
    let storage = json(&machine.af(
        &repo,
        &[("AF_STORAGE__MAX_BYTES", "3GiB")],
        &["storage", "--json"],
    ));
    assert_eq!(storage["max_bytes"], 3_u64 << 30);
}

/// The Tasks of `task_gc::fixture` run with every setting in `env`, in a private machine.
fn start(machine: &Machine, repo: &Path, state: &Path, file: &str, env: &[(&str, &str)]) -> Output {
    let output = machine.af(
        repo,
        env,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            file,
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    std::thread::sleep(Duration::from_millis(20));
    output
}

fn listed(machine: &Machine, repo: &Path, state: &Path) -> Value {
    json(&machine.af(
        repo,
        &[],
        &["task", "list", "--state", state.to_str().unwrap(), "--json"],
    ))
}

fn collected(list: &Value, task_id: &str) -> bool {
    list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == task_id)
        .unwrap_or_else(|| panic!("{task_id} in {list}"))
        .get("collected")
        .is_some()
}

#[test]
fn collection_after_a_run_takes_finished_tasks_beyond_the_newest_and_auto_gc_off_keeps_them() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let eager = [
        ("AF_STORAGE__KEEP_DAYS", "0"),
        ("AF_STORAGE__KEEP_TASKS", "1"),
    ];
    assert!(
        start(&machine, &repo, &state, "gc-older.json", &[])
            .status
            .success()
    );
    // The default policy keeps a Task finished moments ago.
    let list = listed(&machine, &repo, &state);
    assert!(!collected(&list, "gc-older"));
    let newer = start(&machine, &repo, &state, "gc-newer.json", &eager);
    let (stdout, stderr) = text(&newer);
    assert!(newer.status.success(), "{stdout}\n{stderr}");
    assert!(
        stderr.contains("af storage: removed task") && stderr.contains("Task gc-older"),
        "{stderr}"
    );
    let list = listed(&machine, &repo, &state);
    assert!(collected(&list, "gc-older"), "{list}");
    assert!(!collected(&list, "gc-newer"), "{list}");
    // The Store lives outside the state root, so the registry is how the sweep reached it.
    let registry = std::fs::read_to_string(machine.state_home().join("af/stores.toml")).unwrap();
    assert!(
        registry.contains(&state.display().to_string()),
        "{registry}"
    );
    assert!(registry.contains("kind = \"task\""), "{registry}");

    // auto_gc = false: the same runs keep both; `af storage prune --apply` collects on request.
    let off = Machine::new();
    let (repo, state) = task_gc::fixture(&off.root);
    let mut quiet = eager.to_vec();
    quiet.push(("AF_STORAGE__AUTO_GC", "false"));
    for file in ["gc-older.json", "gc-newer.json"] {
        assert!(start(&off, &repo, &state, file, &quiet).status.success());
    }
    let list = listed(&off, &repo, &state);
    assert!(!collected(&list, "gc-older") && !collected(&list, "gc-newer"));
    let pruned = json(&off.af(&repo, &eager, &["storage", "prune", "--apply", "--json"]));
    assert!(
        pruned["removals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|removal| removal["task_id"] == "gc-older" && removal["rule"] == "collection"),
        "{pruned}"
    );
    let list = listed(&off, &repo, &state);
    assert!(collected(&list, "gc-older") && !collected(&list, "gc-newer"));

    // A registered Store that is gone is dropped by the next sweep.
    std::fs::remove_dir_all(&state).unwrap();
    let pruned = json(&off.af(&repo, &[], &["storage", "prune", "--apply", "--json"]));
    assert_eq!(pruned["registry_dropped"][0], state.display().to_string());
    let registry = std::fs::read_to_string(off.state_home().join("af/stores.toml")).unwrap();
    assert!(
        !registry.contains(&state.display().to_string()),
        "{registry}"
    );
}

#[test]
fn below_the_floor_a_worker_attempt_is_refused_before_it_starts_and_charged_nothing() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let refused = start(
        &machine,
        &repo,
        &state,
        "gc-older.json",
        &[("AF_TEST_FREE_BYTES", "1024")],
    );
    let (stdout, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stdout}\n{stderr}");
    for needle in [
        "insufficient_disk: 1.0 KiB (1024 bytes) free",
        "free-disk floor of 10.0 GiB",
        "`af storage`",
        "AF_STORAGE__MIN_FREE_BYTES",
    ] {
        assert!(stderr.contains(needle), "{needle}: {stderr}");
    }
    let shown = json(&machine.af(
        &repo,
        &[],
        &[
            "task",
            "show",
            "gc-older",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(shown["chargeable_tokens"], "0", "{shown}");
    let records = shown["execution_records"].as_array().unwrap();
    assert!(
        records
            .iter()
            .any(|record| record["record"]["kind"] == "released"
                && record["record"]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.starts_with("insufficient_disk: "))),
        "{records:?}"
    );
    assert!(
        !records
            .iter()
            .any(|record| record["record"]["kind"] == "started"),
        "no Attempt started: {records:?}"
    );
    // With room again the same Task runs on.
    let resumed = machine.af(
        &repo,
        &[],
        &[
            "task",
            "run",
            "gc-older",
            "--execute",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    let (stdout, stderr) = text(&resumed);
    assert!(resumed.status.success(), "{stdout}\n{stderr}");
}

#[test]
fn below_the_floor_a_check_reports_insufficient_disk_as_its_result() {
    let machine = Machine::new();
    let (repo, state) = task_cli::fixture_named(&machine.root, "pagination");
    // The implementer fills the disk once it has written its candidate: the check after it
    // meets the floor.
    let free = machine.root.join("free-bytes");
    std::fs::write(&free, "1099511627776").unwrap();
    let implementer = repo.join(".af/task-packages/fixture/implementer");
    std::fs::write(
        implementer.join("worker.py"),
        format!(
            "import json,sys\njson.load(sys.stdin)\n\
             open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\\n    return items[offset:offset+limit]\\n')\n\
             open({:?},'w').write('0')\n\
             print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'report':[{{'summary':'Implemented pagination'}}]}}}}))\n",
            free.display().to_string()
        ),
    )
    .unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    catalog["packages"]["fixture/implementer"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("fixture/implementer", &implementer).unwrap(),
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "fill the disk"]] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    let file = machine.root.join("floor-check.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&serde_json::json!({
            "schema": "af.task-file/1", "task_id": "floor-check", "kind": "implement",
            "goal": "Implement this Jira ticket: offset/limit pagination",
            "pipeline": {"name": "fixture/implementation", "fallback": "refuse"},
            "strategy": "small", "facts": {},
            "limits": {"tokens": 1000, "max_attempts": 3, "wall_ms": 600_000,
                "verification": {"tokens": 200, "attempts": 2, "wall_ms": 300_000}}
        }))
        .unwrap(),
    )
    .unwrap();
    let hook = format!("@{}", free.display());
    let output = start(
        &machine,
        &repo,
        &state,
        file.to_str().unwrap(),
        &[("AF_TEST_FREE_BYTES", hook.as_str())],
    );
    let (stdout, stderr) = text(&output);
    // The check's own result says why it did not run; whatever the Task does next starts no
    // Worker either.
    let mut found = false;
    for entry in walk(&state.join("cas")) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        if let Ok(result) = serde_json::from_slice::<Value>(&bytes)
            && result["name"] == "pagination"
            && result["status"] == "not_run"
            && result["reason"]
                .as_str()
                .is_some_and(|reason| reason.starts_with("insufficient_disk: 0 B (0 bytes) free"))
        {
            found = true;
        }
    }
    assert!(found, "no refused check result\n{stdout}\n{stderr}");
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(next) = stack.pop() {
        if next.is_dir() {
            for entry in std::fs::read_dir(&next).unwrap() {
                stack.push(entry.unwrap().path());
            }
        } else {
            files.push(next);
        }
    }
    files
}

/// A fake `claude` that records history the way the CLI does — one JSONL file below
/// `$CLAUDE_CONFIG_DIR/projects/<slug of its working directory>` — and answers the admission
/// probe and the two reviewers of the `review` fixture.
const FAKE_CLAUDE: &str = r#"#!/usr/bin/python3
import os,json,sys,re
home=os.environ['CLAUDE_CONFIG_DIR']
if sys.argv[1:3]==['auth','status']:
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':'developer@example.test'}))
 sys.exit(0)
request=sys.stdin.read()
slug=re.sub('[^A-Za-z0-9]','-',os.getcwd())
os.makedirs(os.path.join(home,'projects',slug),exist_ok=True)
open(os.path.join(home,'projects',slug,'session.jsonl'),'w').write('{}\n')
with open(os.path.join(home,'slugs'),'a') as f: f.write(slug+'\n')
if request=='Reply with exactly: OK\n':
 result='OK'
else:
 result=json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'reports':[],'benchmark_demands':[],'dispositions':[]}]}})
envelope={'is_error':False,'result':result,'usage':{'input_tokens':10,'output_tokens':2,'cache_creation_input_tokens':0}}
if request!='Reply with exactly: OK\n':
 envelope['structured_output']=json.loads(result)
print(json.dumps(envelope))
"#;

/// The `review` fixture with both reviewers on a Claude Provider whose config directory is
/// `home/claude`; returns the repository, the Store and the config directory.
fn claude_review(machine: &Machine) -> (PathBuf, PathBuf, PathBuf) {
    use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
    let (repo, state) = task_cli::fixture_named(&machine.root, "review");
    let config = machine.root.join("home/claude");
    let bin = machine.root.join("bin");
    std::fs::create_dir_all(config.join("projects/-Users-me-project")).unwrap();
    std::fs::write(
        config.join("projects/-Users-me-project/mine.jsonl"),
        b"{\"keep\":true}\n",
    )
    .unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("claude"), FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry = machine.root.join("config/af/providers.toml");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(
        &registry,
        toml::to_string(&serde_json::json!({"version": 1, "providers": [
            {"id": "claude-personal", "kind": "claude", "auth_dir": config}]}))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o600)).unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    let mut providers = toml::map::Map::new();
    for name in ["bugs", "correctness"] {
        let package = format!("fixture/{name}");
        let path = repo.join(format!(".af/task-packages/{package}"));
        let mut worker: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(path.join("worker.toml")).unwrap()).unwrap();
        worker.runner = TaskWorkerRunner::Model {
            provider_kind: "claude".into(),
            model: "claude-fixture-1".into(),
            effort: "high".into(),
        };
        worker.signature.attempt.as_mut().unwrap().tokens = 1000;
        std::fs::write(path.join("worker.toml"), toml::to_string(&worker).unwrap()).unwrap();
        catalog["packages"][&package]["digest"] =
            toml::Value::String(review_config::lock::package_digest(&package, &path).unwrap());
        providers.insert(package, toml::Value::String("claude-personal".into()));
    }
    let pipeline_dir = repo.join(".af/task-packages/fixture/review");
    let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
        toml::from_str(&std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap())
            .unwrap();
    pipeline.max_attempts = 4;
    std::fs::write(
        pipeline_dir.join("pipeline.toml"),
        toml::to_string(&pipeline).unwrap(),
    )
    .unwrap();
    catalog["packages"]["fixture/review"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("fixture/review", &pipeline_dir).unwrap(),
    );
    catalog
        .as_table_mut()
        .unwrap()
        .insert("providers".into(), toml::Value::Table(providers));
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    let file_path = repo.join("review.json");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&file_path).unwrap()).unwrap();
    file["limits"]["tokens"] = 10_000.into();
    file["limits"]["max_attempts"] = 4.into();
    file["limits"]["wall_ms"] = 600_000.into();
    file["limits"]["verification"]["tokens"] = 6_096.into();
    file["limits"]["verification"]["attempts"] = 4.into();
    file["limits"]["verification"]["wall_ms"] = 60_000.into();
    std::fs::write(file_path, serde_json::to_vec(&file).unwrap()).unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "claude reviewers"]] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    (repo, state, config)
}

fn run_claude_review(machine: &Machine, repo: &Path, state: &Path, env: &[(&str, &str)]) {
    let path = std::env::join_paths(
        std::iter::once(machine.root.join("bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let path = path.to_str().unwrap().to_owned();
    let registry = machine.root.join("config/af/providers.toml");
    let mut env = env.to_vec();
    env.extend([
        ("PATH", path.as_str()),
        ("USER", "fixture"),
        ("AF_PROVIDERS_FILE", registry.to_str().unwrap()),
    ]);
    let output = machine.af(
        repo,
        &env,
        &[
            "review",
            "--file",
            "review.json",
            "--json",
            "--state",
            state.to_str().unwrap(),
        ],
    );
    let (stdout, stderr) = text(&output);
    assert!(
        matches!(output.status.code(), Some(0 | 3)),
        "{stdout}\n{stderr}"
    );
}

fn projects(config: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(config.join("projects"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn the_claude_history_of_every_attempt_and_probe_is_removed_and_nothing_else() {
    let machine = Machine::new();
    let (repo, state, config) = claude_review(&machine);
    run_claude_review(&machine, &repo, &state, &[]);
    let slugs = std::fs::read_to_string(config.join("slugs")).unwrap();
    // The admission probe and both reviewers wrote history, each in its own af-made directory.
    assert!(slugs.lines().count() >= 3, "{slugs}");
    assert!(
        slugs.lines().all(|slug| slug.contains("-af-sandbox-")),
        "{slugs}"
    );
    assert_eq!(projects(&config), ["-Users-me-project"]);
    assert_eq!(
        std::fs::read(config.join("projects/-Users-me-project/mine.jsonl")).unwrap(),
        b"{\"keep\":true}\n"
    );

    // Kept on request.
    let kept = Machine::new();
    let (repo, state, config) = claude_review(&kept);
    run_claude_review(
        &kept,
        &repo,
        &state,
        &[("AF_STORAGE__KEEP_WORKER_TRANSCRIPTS", "true")],
    );
    let slugs = std::fs::read_to_string(config.join("slugs")).unwrap();
    let left = projects(&config);
    for slug in slugs.lines() {
        assert!(left.iter().any(|name| name == slug), "{slug}: {left:?}");
    }
}
