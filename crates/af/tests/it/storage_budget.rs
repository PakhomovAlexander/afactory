//! ADR-0144 end to end: the Storage Budget through the real CLI, over a private HOME and private
//! XDG roots, so nothing here reads or writes the machine's own af directories, Claude config or
//! GitHub. `af storage` and `af storage prune` over planted entries; the free-disk floor through
//! the debug-only `AF_TEST_FREE_BYTES` hook; collection after `af task start --execute`, and the
//! `af/TaskStorageSweep@2` receipt it leaves on the Task, an unknown total included; and the
//! Claude history a Worker Attempt and an admission probe leave, removed by the adapter.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::{schemas, task_cli, task_gc};

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
        let mut command = crate::common::af();
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

    // 50 GiB free: `max_bytes = "auto"` is 20 GiB on every host.
    let modest = [("AF_TEST_FREE_BYTES", "53687091200")];
    let shown = json(&machine.af(&machine.root, &modest, &["storage", "--json"]));
    schemas::valid(&schemas::validator("storage-v5.json"), &shown);
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
    let listed = machine.af(&machine.root, &modest, &["storage"]);
    let (stdout, _) = text(&listed);
    assert!(listed.status.success());
    assert!(stdout.contains("warm keys"), "{stdout}");
    assert!(stdout.contains("of a 20.0 GiB budget"), "{stdout}");
    assert!(stdout.contains("[storage] min_free_bytes"), "{stdout}");

    // 1 MiB of budget: the two older keys go, least recently used first; the fresh one stays.
    let budget = [("AF_STORAGE__MAX_BYTES", "1MiB")];
    let preview = json(&machine.af(&machine.root, &budget, &["storage", "prune", "--json"]));
    schemas::valid(&schemas::validator("storage-prune-v4.json"), &preview);
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
    schemas::valid(&schemas::validator("storage-prune-v4.json"), &applied);
    assert_eq!(applied["applied"], true);
    assert_eq!(paths(&applied), paths(&preview));
    assert_eq!(applied["stop"], "fits");
    assert!(!old.exists() && !newer.exists());
    assert!(fresh.exists(), "used within the hour");
    let (stdout, _) = text(&machine.af(&machine.root, &budget, &["storage", "prune"]));
    assert!(stdout.contains("fits"), "{stdout}");
    assert!(stdout.contains("run with --apply"), "{stdout}");
}

/// ADR-0144: every `af task run` ends with the budget step, one refused before it started work
/// included; with `auto_gc` off it evicts only while af holds more than the budget.
#[test]
fn a_run_refused_before_any_work_still_holds_af_to_its_budget() {
    let machine = Machine::new();
    let old = warm_key(&machine, '4', 600 * 1024, 3 * DAY);
    let newer = warm_key(&machine, '5', 600 * 1024, 2 * DAY);
    // A Task that does not exist: refused before any work, as a malformed Task file would be.
    let refused = machine.af(
        &machine.root,
        &[("AF_STORAGE__MAX_BYTES", "1MiB")],
        &["task", "run", "no-such-task", "--execute"],
    );
    let (stdout, stderr) = text(&refused);
    assert!(!refused.status.success(), "{stdout}\n{stderr}");
    assert!(!old.exists(), "the least recently used key went\n{stderr}");
    assert!(newer.exists(), "af fits once the older key is gone");
    // Within the budget the same refused run removes nothing.
    let refused = machine.af(
        &machine.root,
        &[],
        &["task", "run", "no-such-task", "--execute"],
    );
    assert!(!refused.status.success());
    assert!(newer.exists());
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
    assert!(stdout.contains("max_bytes = \"auto\""), "{stdout}");
    let storage = json(&machine.af(
        &repo,
        &[("AF_TEST_FREE_BYTES", "53687091200")],
        &["storage", "--json"],
    ));
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
fn collection_after_a_run_takes_finished_tasks_only_when_auto_gc_is_on() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let eager = [
        ("AF_STORAGE__AUTO_GC", "true"),
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
    // One summary line names the class it freed, never the sweep's every line.
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("storage: freed ") && line.ends_with("(1 finished Task)")),
        "{stderr}"
    );
    assert!(!stderr.contains("af storage: removed"), "{stderr}");
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

    // The default, auto_gc = false: the same runs keep both, since nothing is over the budget;
    // `af storage prune --apply` collects on request.
    let off = Machine::new();
    let (repo, state) = task_gc::fixture(&off.root);
    let quiet = [
        ("AF_STORAGE__KEEP_DAYS", "0"),
        ("AF_STORAGE__KEEP_TASKS", "1"),
    ];
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

/// The laboratory's browser fixture registers its Stores by this rule
/// (`fixtures/performance/cli/setup.sh`, ADR-0260): a command that resolves a `--state` Store
/// records it, and `af task gc`'s preview writes nothing else; `af task list` only reads.
#[test]
fn a_task_gc_preview_registers_its_state_store_and_a_listing_does_not() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let state_arg = state.to_str().unwrap();
    let captured = machine.af(
        &repo,
        &[],
        &[
            "task",
            "start",
            "--file",
            "gc-older.json",
            "--state",
            state_arg,
            "--json",
        ],
    );
    assert!(captured.status.success(), "{}", text(&captured).1);
    let registry = machine.state_home().join("af/stores.toml");
    std::fs::remove_file(&registry).unwrap();
    let list = listed(&machine, &repo, &state);
    assert_eq!(list["tasks"][0]["task_id"], "gc-older", "{list}");
    assert!(!registry.exists(), "af task list recorded its Store");
    let preview = machine.af(
        &repo,
        &[],
        &[
            "task",
            "gc",
            "--older-than",
            "36500",
            "--keep",
            "1000",
            "--state",
            state_arg,
        ],
    );
    let (stdout, stderr) = text(&preview);
    assert!(preview.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("Nothing was written"), "{stdout}");
    let recorded = std::fs::read_to_string(&registry).unwrap();
    assert!(
        recorded.contains(&format!("path = \"{}\"", state.display())),
        "{recorded}"
    );
    assert!(recorded.contains("kind = \"task\""), "{recorded}");
}

#[test]
fn below_the_floor_a_worker_attempt_is_refused_before_it_starts_and_charged_nothing() {
    attempt_refused_below_the_floor(&[]);
}

/// The floor refuses a Worker Attempt whatever else the machine sets: `env` adds settings
/// (Telemetry at `content`, for ADR-0153's clause 7).
pub(crate) fn attempt_refused_below_the_floor(env: &[(&str, &str)]) {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let mut settings = vec![("AF_TEST_FREE_BYTES", "1024")];
    settings.extend_from_slice(env);
    let refused = start(&machine, &repo, &state, "gc-older.json", &settings);
    let (stdout, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stdout}\n{stderr}");
    for needle in [
        "insufficient_disk: ",
        ": 1.0 KiB (1024 bytes) free, floor 10.0 GiB (10737418240 bytes); the sandbox root is ",
        "set TMPDIR or [storage] sandbox_root to a volume with room",
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
        env,
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
    check_refused_below_the_floor(&[]);
}

/// The floor refuses a check whatever else the machine sets: `env` adds settings (Telemetry at
/// `content`, for ADR-0153's clause 7).
pub(crate) fn check_refused_below_the_floor(env: &[(&str, &str)]) {
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
    let mut settings = vec![("AF_TEST_FREE_BYTES", hook.as_str())];
    settings.extend_from_slice(env);
    let output = start(&machine, &repo, &state, file.to_str().unwrap(), &settings);
    let (stdout, stderr) = text(&output);
    // The check's own result says why it did not run; whatever the Task does next starts no
    // Worker either.
    let mut found = false;
    for entry in walk(&state.join("cas")) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        if let Ok(result) = serde_json::from_slice::<Value>(&bytes)
            && result["name"] == "pagination"
            && result["status"] == "not_run"
            && result["reason"].as_str().is_some_and(|reason| {
                reason.starts_with("insufficient_disk: ")
                    && reason.contains(": 0 B (0 bytes) free, floor ")
            })
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
/// `$CLAUDE_CONFIG_DIR/projects/<slug of its working directory>`, for the account probe
/// (`auth status`) as for every Attempt — and answers the admission probe and the two reviewers
/// of the `review` fixture. Every slug it wrote is listed in `slugs`, the probe's in
/// `probe-slugs`.
const FAKE_CLAUDE: &str = r#"#!/usr/bin/python3
import os,json,sys,re
home=os.environ['CLAUDE_CONFIG_DIR']
slug=re.sub('[^A-Za-z0-9]','-',os.getcwd())
os.makedirs(os.path.join(home,'projects',slug),exist_ok=True)
open(os.path.join(home,'projects',slug,'session.jsonl'),'w').write('{}\n')
with open(os.path.join(home,'slugs'),'a') as f: f.write(slug+'\n')
if sys.argv[1:3]==['auth','status']:
 with open(os.path.join(home,'probe-slugs'),'a') as f: f.write(slug+'\n')
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':'developer@example.test'}))
 sys.exit(0)
request=sys.stdin.read()
if request==PROBE_INPUT:
 result='OK'
else:
 result=json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'reports':[],'benchmark_demands':[],'dispositions':[]}]}})
# Under `stream-json` (Telemetry at `spans` and above) Claude prints its envelope as the
# `result` line; under `json` this fixture prints it without a `type`.
envelope={'type':'result'} if 'stream-json' in sys.argv else {}
envelope.update({'is_error':False,'result':result,'usage':{'input_tokens':10,'output_tokens':2,'cache_creation_input_tokens':0}})
if request!=PROBE_INPUT:
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
    // What a session run from `/` keeps: never af's to remove.
    std::fs::create_dir_all(config.join("projects/-")).unwrap();
    std::fs::write(config.join("projects/-/root.jsonl"), b"{\"keep\":true}\n").unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("claude"),
        crate::provider_cli::with_help("claude", &crate::provider_cli::with_probe(FAKE_CLAUDE)),
    )
    .unwrap();
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
    // The account probe, the admission probe and both reviewers wrote history, each in its own
    // af-made directory, never from `/`.
    assert!(slugs.lines().count() >= 4, "{slugs}");
    assert!(
        slugs.lines().all(|slug| slug.contains("-af-sandbox-")),
        "{slugs}"
    );
    let probes = std::fs::read_to_string(config.join("probe-slugs")).unwrap();
    assert!(!probes.is_empty(), "the account probe ran");
    assert_eq!(projects(&config), ["-", "-Users-me-project"]);
    assert_eq!(
        std::fs::read(config.join("projects/-Users-me-project/mine.jsonl")).unwrap(),
        b"{\"keep\":true}\n"
    );
    assert_eq!(
        std::fs::read(config.join("projects/-/root.jsonl")).unwrap(),
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

/// ADR-0144: the sweep that ends `af task run` records what it removed on the Task the run
/// executed, as one `storage_sweep` observation that `af task show` prints and its JSON
/// carries; a sweep that removed nothing records nothing.
#[test]
fn the_sweep_that_ends_a_run_is_an_observation_of_its_task() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    assert!(
        start(&machine, &repo, &state, "gc-older.json", &[])
            .status
            .success()
    );
    let show = |task: &str, json_output: bool| {
        let mut args = vec!["task", "show", task, "--state", state.to_str().unwrap()];
        if json_output {
            args.push("--json");
        }
        machine.af(&repo, &[], &args)
    };
    // Nothing was removed after the first run: nothing is recorded.
    let first = json(&show("gc-older", true));
    assert!(first.get("storage_sweeps").is_none(), "{first}");
    // Collection by age is off by default; this run turns it on to have something to record.
    let eager = [
        ("AF_STORAGE__AUTO_GC", "true"),
        ("AF_STORAGE__KEEP_DAYS", "0"),
        ("AF_STORAGE__KEEP_TASKS", "1"),
    ];
    let newer = start(&machine, &repo, &state, "gc-newer.json", &eager);
    let (stdout, stderr) = text(&newer);
    assert!(newer.status.success(), "{stdout}\n{stderr}");
    let shown = json(&show("gc-newer", true));
    schemas::valid(&schemas::validator("task-inspection-v14.json"), &shown);
    let [sweep] = shown["storage_sweeps"].as_array().unwrap().as_slice() else {
        panic!("one storage sweep: {shown}");
    };
    schemas::valid(&schemas::validator("task-storage-sweep-v2.json"), sweep);
    assert_eq!(sweep["schema"], "af/TaskStorageSweep@2");
    let removal = sweep["removals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|removal| removal["task_id"] == "gc-older")
        .unwrap_or_else(|| panic!("gc-older collected: {sweep}"));
    assert_eq!(removal["kind"], "task");
    assert_eq!(removal["rule"], "collection");
    assert_eq!(removal["path"], state.display().to_string());
    assert!(sweep["total_before"].as_u64().unwrap() >= sweep["total_after"].as_u64().unwrap());
    assert!(sweep["freed_bytes"].as_u64().unwrap() >= removal["bytes"].as_u64().unwrap());
    assert_eq!(shown["result"], json(&show("gc-newer", true))["result"]);
    let (stdout, stderr) = text(&show("gc-newer", false));
    let line = stdout
        .lines()
        .find(|line| line.starts_with("storage sweep: "))
        .unwrap_or_else(|| panic!("no storage sweep line:\n{stdout}\n{stderr}"));
    assert!(
        line.starts_with("storage sweep: removed 1 entry ("),
        "{line}"
    );
    assert!(line.contains(" budget"), "{line}");
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).unwrap().mode() & 0o777
}

/// `af review run --state DIR`: a run that fails right after it opened its Store, which is all
/// these tests need (no policy revision is given).
fn review_run(machine: &Machine, repo: &Path, state: &Path, env: &[(&str, &str)]) -> Output {
    machine.af(
        repo,
        env,
        &[
            "review",
            "run",
            "--repo",
            repo.to_str().unwrap(),
            "--campaign",
            "keep",
            "--state",
            state.to_str().unwrap(),
        ],
    )
}

fn git_repo(machine: &Machine) -> PathBuf {
    let repo = machine.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    repo
}

/// H2 and M6 of the 2026-10-20 audit: an explicit `--state` directory that holds anything but
/// af's own entries is refused before anything is written or registered; an absent or empty one
/// becomes a Store with the owner marker, private whatever the umask; an af-owned root open to
/// others is narrowed; and what af writes into the user's checkout keeps the user's umask.
#[test]
fn an_explicit_state_directory_must_be_new_empty_or_afs_and_a_new_store_is_private() {
    let machine = Machine::new();
    let repo = git_repo(&machine);
    let mydocs = machine.root.join("mydocs");
    std::fs::create_dir_all(mydocs.join("photos")).unwrap();
    std::fs::write(mydocs.join("important.txt"), b"mine").unwrap();
    std::fs::write(mydocs.join("photos/1.jpg"), b"jpg").unwrap();
    let refused = review_run(&machine, &repo, &mydocs, &[]);
    let (stdout, stderr) = text(&refused);
    assert!(!refused.status.success(), "{stdout}\n{stderr}");
    assert!(stderr.contains("holds what is not af's"), "{stderr}");
    assert!(stderr.contains("becomes af-owned"), "{stderr}");
    let mut names: Vec<String> = std::fs::read_dir(&mydocs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["important.txt", "photos"], "nothing was written");
    let registry = machine.state_home().join("af/stores.toml");
    assert!(
        std::fs::read_to_string(&registry)
            .map_or(true, |text| !text.contains(&mydocs.display().to_string())),
        "nothing was registered"
    );

    let empty = machine.root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let absent = machine.root.join("outside/absent");
    for state in [&empty, &absent] {
        let opened = review_run(&machine, &repo, state, &[]);
        let (_, stderr) = text(&opened);
        assert!(
            !stderr.contains("holds what is not af's"),
            "{}: {stderr}",
            state.display()
        );
        assert!(
            state.join(".af-store").is_file(),
            "{}: {stderr}",
            state.display()
        );
        assert_eq!(mode(state), 0o700, "{}", state.display());
        for file in [
            ".af-store",
            "events.sqlite",
            "events.sqlite-wal",
            "events.sqlite-shm",
        ] {
            if state.join(file).exists() {
                assert_eq!(mode(&state.join(file)), 0o600, "{file}");
            }
        }
        assert!(state.join("events.sqlite").is_file());
        for directory in ["cas", "cas/objects", "git-home"] {
            assert_eq!(mode(&state.join(directory)), 0o700, "{directory}");
        }
    }
    assert_eq!(mode(&machine.state_home().join("af")), 0o700);

    // An af-owned root open to others is narrowed when af opens it again.
    for root in [&absent, &machine.state_home().join("af")] {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let reopened = review_run(&machine, &repo, &absent, &[]);
    let (_, stderr) = text(&reopened);
    assert!(!stderr.contains("holds what is not af's"), "{stderr}");
    assert_eq!(mode(&absent), 0o700);
    assert_eq!(mode(&machine.state_home().join("af")), 0o700);

    // What af writes into the user's checkout keeps the user's umask: no process-wide umask.
    let onboarded = machine.af(
        &repo,
        &[],
        &[
            "onboard",
            "--repo",
            repo.to_str().unwrap(),
            "--runner",
            "codex",
            "--gate",
            "check=true",
            "--apply",
        ],
    );
    let (stdout, stderr) = text(&onboarded);
    assert!(onboarded.status.success(), "{stdout}\n{stderr}");
    let reference = machine.root.join("reference");
    std::fs::create_dir(&reference).unwrap();
    std::fs::write(reference.join("file"), b"").unwrap();
    assert_eq!(mode(&repo.join(".af")), mode(&reference));
    assert_eq!(
        mode(&repo.join(".af/af.toml")),
        mode(&reference.join("file"))
    );
}

/// A Store af created at `--state`, into which the user has since put their own files, with
/// every af entry last used `old` ago.
fn shared_store(machine: &Machine, repo: &Path, name: &str) -> PathBuf {
    let state = machine.root.join(name);
    let opened = review_run(machine, repo, &state, &[]);
    assert!(state.join(".af-store").is_file(), "{}", text(&opened).1);
    std::fs::write(state.join("important.txt"), b"mine").unwrap();
    std::fs::create_dir_all(state.join("photos")).unwrap();
    std::fs::write(state.join("photos/1.jpg"), vec![9_u8; 2 << 20]).unwrap();
    age(&state, 3 * DAY);
    state
}

/// H2 of the 2026-10-20 audit, the reproduction: a registered `--state` Store that also holds
/// the user's files, over a tiny budget. `af storage` counts af's entries alone; `af storage
/// prune --apply` and the sweep that ends a run remove af's entries and leave the user's files
/// and the directory, saying so.
#[test]
fn a_registered_store_keeps_the_users_files_through_prune_and_the_sweep_after_a_run() {
    let machine = Machine::new();
    let repo = git_repo(&machine);
    let mydocs = shared_store(&machine, &repo, "mydocs");

    let shown = json(&machine.af(&machine.root, &[], &["storage", "--json"]));
    let entry = shown["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == mydocs.display().to_string())
        .unwrap_or_else(|| panic!("{shown}"));
    assert_eq!(entry["kind"], "campaign");
    assert!(
        entry["bytes"].as_u64().unwrap() < 1 << 20,
        "the user's 2 MiB are not af's: {entry}"
    );

    let budget = [("AF_STORAGE__MAX_BYTES", "1KiB")];
    let pruned = json(&machine.af(
        &machine.root,
        &budget,
        &["storage", "prune", "--apply", "--json"],
    ));
    schemas::valid(&schemas::validator("storage-prune-v4.json"), &pruned);
    let removal = pruned["removals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|removal| removal["path"] == mydocs.display().to_string())
        .unwrap_or_else(|| panic!("{pruned}"));
    assert_eq!(
        removal["left"],
        serde_json::json!([
            mydocs.display().to_string(),
            mydocs.join("important.txt").display().to_string(),
            mydocs.join("photos").display().to_string(),
        ]),
        "{pruned}"
    );
    assert_eq!(
        std::fs::read(mydocs.join("important.txt")).unwrap(),
        b"mine"
    );
    assert!(mydocs.join("photos/1.jpg").is_file());
    assert!(!mydocs.join("events.sqlite").exists() && !mydocs.join("cas").exists());

    // The sweep that ends a run, a refused one included, keeps them the same way.
    let other = shared_store(&machine, &repo, "other");
    let refused = machine.af(
        &machine.root,
        &budget,
        &["task", "run", "no-such-task", "--execute", "--verbose"],
    );
    let (_, stderr) = text(&refused);
    assert!(!refused.status.success());
    assert!(
        stderr.contains(&format!(
            "removed af's entries of {} and left the directory with what is not af's",
            other.display()
        )),
        "{stderr}"
    );
    assert!(other.join("important.txt").is_file() && other.join("photos/1.jpg").is_file());
    assert!(!other.join("events.sqlite").exists());

    // A marked Store that holds nothing but af's entries still goes whole.
    let whole = machine.root.join("whole");
    let opened = review_run(&machine, &repo, &whole, &[]);
    assert!(whole.join(".af-store").is_file(), "{}", text(&opened).1);
    age(&whole, 3 * DAY);
    let pruned = json(&machine.af(
        &machine.root,
        &budget,
        &["storage", "prune", "--apply", "--json"],
    ));
    assert!(!whole.exists(), "{pruned}");
}

/// Every directory below `root`, made readable and writable again, so the machine's temporary
/// root can go.
fn unseal(root: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(root) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
    for entry in std::fs::read_dir(root).unwrap() {
        unseal(&entry.unwrap().path());
    }
}

/// ADR-0144 clause 4 with ADR-0159 clause 13: the sweep that ends a run, one of whose removals
/// stopped part way and could not measure what it left, is recorded on the Task all the same:
/// `af/TaskStorageSweep@2` with the removal that finished, the one that failed, the bytes that
/// left the disk, and its total after it and its stop `unknown`, never a number. The Task's log
/// replays it: `af task show` prints and carries it.
#[test]
fn a_sweep_whose_remainder_cannot_be_measured_leaves_a_replayable_receipt_on_its_task() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let campaigns = machine.state_home().join("af/review/campaigns");
    // Two removals dead processes left claimed: one the sweep finishes, and one in which a
    // directory turns unreadable just before its removal, as another process could make it.
    let whole = crate::common::af_claim(&campaigns, 999_999_999);
    let partly = crate::common::af_claim(&campaigns, 999_999_998);
    for (claim, directories) in [(&whole, &["free"][..]), (&partly, &["free", "kept"][..])] {
        for directory in directories {
            std::fs::create_dir_all(claim.join(directory)).unwrap();
            std::fs::write(claim.join(directory).join("00"), vec![7_u8; 1 << 20]).unwrap();
        }
    }
    let sealed = partly.join("kept");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = std::fs::read_dir(&sealed).is_err();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !enforced {
        eprintln!("skipped: directory permissions are not enforced for this user");
        return;
    }

    let run = start(
        &machine,
        &repo,
        &state,
        "gc-older.json",
        &[(
            "AF_TEST_STORAGE_SEAL_BEFORE_REMOVAL",
            sealed.to_str().unwrap(),
        )],
    );
    unseal(&campaigns);
    let (stdout, stderr) = text(&run);
    assert!(run.status.success(), "{stdout}\n{stderr}");
    // One summary line says so; `--verbose` would say it line by line.
    assert!(
        stderr.contains("; what af holds now is unknown; see `af storage`"),
        "{stderr}"
    );
    assert!(!stderr.contains("was not recorded on Task"), "{stderr}");
    assert!(!whole.exists(), "the whole claim is finished");

    let show = |json_output: bool| {
        let mut args = vec![
            "task",
            "show",
            "gc-older",
            "--state",
            state.to_str().unwrap(),
        ];
        if json_output {
            args.push("--json");
        }
        machine.af(&repo, &[], &args)
    };
    let shown = json(&show(true));
    schemas::valid(&schemas::validator("task-inspection-v14.json"), &shown);
    let [sweep] = shown["storage_sweeps"].as_array().unwrap().as_slice() else {
        panic!("one storage sweep: {shown}");
    };
    schemas::valid(&schemas::validator("task-storage-sweep-v2.json"), sweep);
    assert_eq!(sweep["schema"], "af/TaskStorageSweep@2");
    assert_eq!(sweep["total_after"], "unknown", "{sweep}");
    assert_eq!(sweep["stop"], "unknown", "{sweep}");
    // The deletion outcome: the claim that went, listed with its bytes, and the one that
    // stopped part way among the failures.
    let finished = sweep["removals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|removal| removal["path"] == whole.to_str().unwrap())
        .unwrap_or_else(|| panic!("the finished claim is listed: {sweep}"));
    assert_eq!(finished["kind"], "claim");
    assert_eq!(finished["rule"], "recovery");
    let freed = finished["bytes"].as_u64().unwrap();
    assert!(freed >= 1 << 20, "{finished}");
    let failures = sweep["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1, "{sweep}");
    assert!(
        failures[0].as_str().unwrap().contains("cannot be measured"),
        "{sweep}"
    );
    assert!(
        !sweep["removals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|removal| removal["path"] == partly.to_str().unwrap()),
        "a failed removal is no removal: {sweep}"
    );
    // What left the disk is what was measured to leave: the finished claim's bytes, and none
    // the partial removal could not vouch for.
    assert_eq!(sweep["freed_bytes"].as_u64().unwrap(), freed, "{sweep}");
    assert!(sweep["total_before"].as_u64().unwrap() >= freed);

    // Replayed again, the text says the same.
    let (stdout, stderr) = text(&show(false));
    let line = stdout
        .lines()
        .find(|line| line.starts_with("storage sweep: "))
        .unwrap_or_else(|| panic!("no storage sweep line:\n{stdout}\n{stderr}"));
    assert!(line.contains(", 1 removal failed; "), "{line}");
    assert!(line.contains(" → unknown of a "), "{line}");
    assert!(line.ends_with("what it left cannot be measured"), "{line}");
}

/// Finding K18: `[storage] max_bytes = "auto"`, the default, holds one warm key on a roomy host:
/// 20 GiB, or 40 GiB when the cache volume has more than 100 GiB free; `af storage` says which
/// and why.
#[test]
fn the_default_budget_fits_one_warm_key_on_a_roomy_host() {
    let machine = Machine::new();
    for (free, budget, why) in [
        (
            "53687091200",
            20_u64 << 30,
            "50.0 GiB free on the cache volume; 40.0 GiB above",
        ),
        (
            "214748364800",
            40_u64 << 30,
            "200.0 GiB free on the cache volume, more than",
        ),
    ] {
        let env = [("AF_TEST_FREE_BYTES", free)];
        let shown = json(&machine.af(&machine.root, &env, &["storage", "--json"]));
        assert_eq!(shown["max_bytes"], budget, "{free}");
        let listed = machine.af(&machine.root, &env, &["storage"]);
        let (stdout, stderr) = text(&listed);
        assert!(listed.status.success(), "{stderr}");
        assert!(
            stdout.contains("[storage] max_bytes = \"auto\": ") && stdout.contains(why),
            "{stdout}"
        );
    }
    // A machine layer's byte count is the budget whatever the host.
    let set = json(&machine.af(
        &machine.root,
        &[
            ("AF_TEST_FREE_BYTES", "214748364800"),
            ("AF_STORAGE__MAX_BYTES", "\"30GiB\""),
        ],
        &["storage", "--json"],
    ));
    assert_eq!(set["max_bytes"], 30_u64 << 30);
}

/// Findings C14 and K18: under budget pressure a Task command prints one summary line naming
/// the class it freed and never the sweep's own lines; `--verbose` prints those. A warm key the
/// budget is too small for is said once, with the budget that fits; the newest key of a project
/// checked in the last 7 days stays.
#[test]
fn a_task_command_under_pressure_prints_one_summary_line() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let idle = warm_key(&machine, '6', 600 * 1024, 30 * DAY);
    let mut project = idle.parent().unwrap().to_path_buf();
    project.set_file_name("b".repeat(64));
    let active = project.join("7".repeat(64));
    std::fs::create_dir_all(active.join("cargo_target")).unwrap();
    std::fs::write(active.join("cargo_target/blob"), vec![7_u8; 600 * 1024]).unwrap();
    std::fs::write(active.join("warm.lock"), b"").unwrap();
    age(&active, 2 * DAY);
    let pressure = [("AF_STORAGE__MAX_BYTES", "1KiB")];
    let run = start(&machine, &repo, &state, "gc-older.json", &pressure);
    let (stdout, stderr) = text(&run);
    assert!(run.status.success(), "{stdout}\n{stderr}");
    let summaries: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("storage: freed "))
        .collect();
    let [summary] = summaries.as_slice() else {
        panic!("one summary line: {stderr}");
    };
    assert!(summary.contains("(1 warm key)"), "{summary}");
    assert!(
        summary.contains("nothing more may go now; see `af storage`"),
        "{summary}"
    );
    assert!(
        !stderr.contains("af storage: ") && !stderr.contains("storage sweep:"),
        "{stderr}"
    );
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("storage: warning: warm key "))
            .count(),
        1,
        "{stderr}"
    );
    assert!(!idle.exists(), "a project idle for 30 days loses its key");
    assert!(active.exists(), "the newest key of an active project stays");
    // The Task just finished is no record the budget takes.
    let listed = json(&machine.af(
        &repo,
        &[],
        &["task", "list", "--state", state.to_str().unwrap(), "--json"],
    ));
    assert!(
        listed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|task| task.get("collected").is_none()),
        "{listed}"
    );

    // `--verbose`: the sweep's lines, as `af storage prune` prints them.
    let again = warm_key(&machine, '8', 600 * 1024, 30 * DAY);
    let verbose = machine.af(
        &repo,
        &pressure,
        &[
            "task",
            "run",
            "gc-older",
            "--execute",
            "--verbose",
            "--state",
            state.to_str().unwrap(),
        ],
    );
    let (stdout, stderr) = text(&verbose);
    assert!(
        stderr.contains(&format!("af storage: removed warm_key {}", again.display())),
        "{stderr}"
    );
    assert!(!stderr.contains("storage: freed "), "{stderr}");
    // The Task's recorded sweep is shown by a `--verbose` run and by `af task show`, never by a
    // plain run's output.
    assert!(stdout.contains("storage sweep: "), "{stdout}");
    let plain = machine.af(
        &repo,
        &pressure,
        &[
            "task",
            "run",
            "gc-older",
            "--execute",
            "--state",
            state.to_str().unwrap(),
        ],
    );
    let (stdout, stderr) = text(&plain);
    assert!(!stdout.contains("storage sweep: "), "{stdout}");
    assert!(!stderr.contains("af storage: "), "{stderr}");
    let shown = machine.af(
        &repo,
        &[],
        &[
            "task",
            "show",
            "gc-older",
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert!(
        text(&shown).0.contains("storage sweep: "),
        "{}",
        text(&shown).0
    );
}

/// Findings I17 and T18: the floor measures the sandbox root af actually uses, `[storage]
/// sandbox_root` when set, and the refusal names that volume, both numbers and how to move it.
#[test]
fn the_floor_names_the_configured_sandbox_root() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    let sandboxes = machine.root.join("sandboxes");
    let refused = start(
        &machine,
        &repo,
        &state,
        "gc-older.json",
        &[
            ("AF_TEST_FREE_BYTES", "2040109465"),
            ("AF_STORAGE__SANDBOX_ROOT", sandboxes.to_str().unwrap()),
        ],
    );
    let (stdout, stderr) = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{stdout}\n{stderr}");
    for needle in [
        "insufficient_disk: ",
        ": 1.9 GiB (2040109465 bytes) free, floor 10.0 GiB (10737418240 bytes); ",
        &format!(
            "the sandbox root is {}; set TMPDIR or [storage] sandbox_root to a volume with room",
            sandboxes.display()
        ),
    ] {
        assert!(stderr.contains(needle), "{needle}: {stderr}");
    }
    assert!(sandboxes.is_dir(), "the configured root is made");
}

/// Findings I9 and C3 items 1-2: a delivered Workspace protects its source Task by the Task ID
/// its `WorkspaceDelivered@1` records, even when the Task ID was taken as a label (`gc-older`,
/// so the Workspace is `gc-older-<hex>`); and while a Project Store cannot be read, no finished
/// Task is collected at all, and the sweep names that Project Store.
#[test]
fn a_delivered_task_is_kept_whatever_its_label_and_an_unreadable_project_store_keeps_all() {
    let machine = Machine::new();
    let (repo, state) = task_gc::fixture(&machine.root);
    for file in ["gc-older.json", "gc-newer.json"] {
        let output = start(&machine, &repo, &state, file, &[]);
        assert!(output.status.success(), "{:?}", text(&output));
    }
    // `gc-older` is taken as a label before the delivery registers its worktree.
    let taken = machine.af(&repo, &[], &["project", "workspace", "new", "gc-older"]);
    assert!(taken.status.success(), "{:?}", text(&taken));
    let worktree = machine.root.join("delivered");
    let delivered = machine.af(
        &repo,
        &[],
        &[
            "task",
            "deliver",
            "gc-older",
            "--state",
            state.to_str().unwrap(),
            "--confirm",
            "gc-older",
            "--branch",
            "delivered/gc-older",
            "--worktree",
            worktree.to_str().unwrap(),
        ],
    );
    assert!(delivered.status.success(), "{:?}", text(&delivered));
    let workspaces = json(&machine.af(&repo, &[], &["project", "workspaces", "--json"]));
    let label = workspaces["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workspace| workspace["origin"] == "delivered")
        .map(|workspace| workspace["label"].as_str().unwrap().to_string())
        .unwrap_or_else(|| panic!("a delivered Workspace in {workspaces}"));
    assert!(
        label.starts_with("gc-older-"),
        "the label collided: {label}"
    );

    // Collection by age with nothing kept, and a budget nothing fits.
    let eager = [
        ("AF_STORAGE__KEEP_DAYS", "0"),
        ("AF_STORAGE__KEEP_TASKS", "0"),
        ("AF_STORAGE__MAX_BYTES", "1KiB"),
    ];
    let removed = |pruned: &Value| -> Vec<String> {
        pruned["removals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|removal| removal["task_id"].as_str().map(str::to_string))
            .collect()
    };

    // An unreadable Project Store: nothing finished is collected, and the sweep says why.
    let projects = machine.state_home().join("af/project");
    let store = std::fs::read_dir(&projects)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("events.sqlite").is_file())
        .expect("a Project Store");
    let log = store.join("events.sqlite");
    let kept = std::fs::read(&log).unwrap();
    std::fs::write(&log, b"not a database, so not a log this release reads").unwrap();
    let pruned = json(&machine.af(&repo, &eager, &["storage", "prune", "--apply", "--json"]));
    assert_eq!(removed(&pruned), Vec::<String>::new(), "{pruned}");
    let failures = pruned["failures"].to_string();
    assert!(
        failures.contains(&format!(
            "the delivered Workspaces of the Project Store {} cannot be read, so no finished \
             Task is collected",
            store.display()
        )),
        "{failures}"
    );
    let list = listed(&machine, &repo, &state);
    assert!(!collected(&list, "gc-older") && !collected(&list, "gc-newer"));

    // Readable again: the delivered Task stays under its suffixed Workspace, the other goes.
    std::fs::write(&log, kept).unwrap();
    let pruned = json(&machine.af(&repo, &eager, &["storage", "prune", "--apply", "--json"]));
    assert_eq!(removed(&pruned), ["gc-newer"], "{pruned}");
    let list = listed(&machine, &repo, &state);
    assert!(!collected(&list, "gc-older"), "{list}");
    assert!(collected(&list, "gc-newer"), "{list}");
}

/// C3 item 6: below the floor, `af storage --json` names each volume the floor measures, its
/// role, its free bytes, that it is below the floor and the next command, as the text does.
#[test]
fn storage_json_names_each_volume_below_the_floor() {
    let machine = Machine::new();
    let sandboxes = machine.root.join("sandboxes");
    let env = [
        ("AF_TEST_FREE_BYTES", "1024"),
        ("AF_STORAGE__SANDBOX_ROOT", sandboxes.to_str().unwrap()),
    ];
    let shown = json(&machine.af(&machine.root, &env, &["storage", "--json"]));
    schemas::valid(&schemas::validator("storage-v5.json"), &shown);
    assert_eq!(shown["schema"], "af/storage@5");
    let volumes = shown["volumes"].as_array().unwrap();
    let roles: Vec<&str> = volumes
        .iter()
        .map(|volume| volume["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["sandbox", "cache"], "{shown}");
    for volume in volumes {
        assert_eq!(volume["free_bytes"], 1024, "{volume}");
        assert_eq!(volume["below_floor"], true, "{volume}");
        assert_eq!(
            volume["next_action"], "af storage prune --apply",
            "{volume}"
        );
        assert!(
            volume["volume"]
                .as_str()
                .is_some_and(|path| !path.is_empty())
        );
    }
    assert_eq!(volumes[0]["root"], sandboxes.to_str().unwrap());
    assert!(
        volumes[0]["fix"]
            .as_str()
            .unwrap()
            .contains("set TMPDIR or [storage] sandbox_root to a volume with room"),
        "{shown}"
    );
    let (text, _) = text(&machine.af(&machine.root, &env, &["storage"]));
    assert!(text.contains("below the floor"), "{text}");

    // Above the floor nothing is to be done.
    let roomy = json(&machine.af(
        &machine.root,
        &[("AF_TEST_FREE_BYTES", "53687091200")],
        &["storage", "--json"],
    ));
    for volume in roomy["volumes"].as_array().unwrap() {
        assert_eq!(volume["below_floor"], false, "{volume}");
        assert!(volume["next_action"].is_null() && volume["fix"].is_null());
    }
}

/// C3 item 24: a version-1 Store registry an earlier release wrote is replaced by the first
/// write that meets it and said once; the next run prints nothing about it.
#[test]
fn a_version_1_store_registry_is_replaced_once_and_said_once() {
    let machine = Machine::new();
    let repo = machine.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        ["init", "-q", "-b", "main"].as_slice(),
        ["commit", "-q", "--allow-empty", "-m", "first"].as_slice(),
    ] {
        let output = Command::new("git")
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let registry = machine.state_home().join("af/stores.toml");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(
        &registry,
        "version = 1\n\n[[store]]\npath = \"/elsewhere/state\"\nkind = \"task\"\n\
         first_use_unix_ms = 1\nlast_use_unix_ms = 1\n",
    )
    .unwrap();
    let mut said = Vec::new();
    for _ in 0..2 {
        let output = machine.af(&repo, &[], &["project", "show"]);
        let (stdout, stderr) = text(&output);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        said.extend(
            stderr
                .lines()
                .filter(|line| line.contains("stores.toml"))
                .map(str::to_string),
        );
    }
    assert_eq!(
        said,
        [format!(
            "af: {} was the Store registry of an earlier release (version 1), which this \
             release does not read, so it was replaced by an empty one; nothing is lost: Stores \
             register again as commands use them, and the Stores under af's default roots were \
             never in it",
            registry.display()
        )],
    );
    let text = std::fs::read_to_string(&registry).unwrap();
    assert!(text.contains("\nversion = 2\n"), "{text}");
    assert!(!text.contains("/elsewhere/state"), "{text}");
    assert!(text.contains("kind = \"project\""), "{text}");
}
