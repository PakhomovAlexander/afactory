//! ADR-0141 through the real `af` binary, Store and a credential-free Codex protocol substitute
//! whose authentication is a file: `revoked` makes inference fail with an expired token while
//! the token-free status still reports a signed-in account, `logged_out` makes the status
//! report none, and `quota` fails inference with a rate limit. No network or credential is used.
use super::*;
use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
use std::os::unix::fs::PermissionsExt;

const NATIVE: &str = r#"#!/usr/bin/python3 -B
import json,os,sys
home=os.environ['CODEX_HOME']
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  request=json.loads(line)
  if request.get('id')==1: print(json.dumps({'id':1,'result':{}}),flush=True)
  if request.get('id')==2:
   account=None if os.path.exists(home+'/logged_out') else {'type':'chatgpt','email':'fixture@example.invalid'}
   print(json.dumps({'id':2,'result':{'account':account}}),flush=True)
 sys.exit(0)
request=sys.stdin.read()
kind='admission' if request=='Reply with exactly: OK\n' else 'author'
with open(home+'/calls','a') as f: f.write(kind+'\n')
usage={'input_tokens':3,'cached_input_tokens':0,'output_tokens':2,'reasoning_output_tokens':0,'cache_write_input_tokens':0}
for name,text in [('revoked','Your access token has expired; access_token=FIXTURE_SECRET'),('quota','Rate limit reached: HTTP 429')]:
 if os.path.exists(home+'/'+name):
  print(json.dumps({'type':'turn.failed','error':{'message':text}}))
  print(json.dumps({'type':'turn.completed','usage':usage}))
  sys.exit(1)
if kind=='admission':
 message='OK'
else:
 r=json.loads(request)
 s=r['inputs']['sources'][0]['payload']['sources']
 message=json.dumps({'schema':'af.worker-reply/1','outputs':{'draft':[{'schema':'af.document-draft/1','title':'Release notes','sections':[{'heading':'Summary','body':'\n\n'.join(s[k]['text'] for k in sorted(s))}],'citations':sorted(s)}]}})
if '-o' in sys.argv:
 with open(sys.argv[sys.argv.index('-o')+1],'w') as f: f.write(message)
print(json.dumps({'type':'thread.started','thread_id':'synthetic-'+kind}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':message}}))
print(json.dumps({'type':'turn.completed','usage':usage}))
"#;

struct Fixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    home: PathBuf,
    path: std::ffi::OsString,
}

impl Fixture {
    fn new(recovery: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = setup(root.path());
        let home = root.path().join("home");
        let bin = root.path().join("bin");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("codex"), NATIVE).unwrap();
        std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::write(home.join("providers.toml"), toml::to_string(&json!({"version":1,"providers":[{"id":"codex-personal","kind":"codex","auth_dir":home}]})).unwrap()).unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        catalog["provider_admission"] = json!({"tokens":64,"wall_ms":45000});
        if recovery {
            catalog["provider_recovery"] =
                json!({"probes":2,"tokens_per_probe":32,"wall_ms_per_probe":45000});
        }
        catalog["providers"] = json!({"builtin/document-author":"codex-personal"});
        let package = repo.join(
            catalog["packages"]["builtin/document-author"]["path"]
                .as_str()
                .unwrap(),
        );
        let mut worker: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(package.join("worker.toml")).unwrap()).unwrap();
        worker.runner = TaskWorkerRunner::Model {
            provider_kind: "codex".into(),
            model: "codex-fixture-1".into(),
            effort: "high".into(),
        };
        worker.signature.attempt.as_mut().unwrap().tokens = 16384;
        worker.signature.attempt.as_mut().unwrap().wall_ms = 180000;
        std::fs::write(
            package.join("worker.toml"),
            toml::to_string(&worker).unwrap(),
        )
        .unwrap();
        catalog["packages"]["builtin/document-author"]["digest"] = json!(
            review_config::lock::package_digest("builtin/document-author", &package).unwrap()
        );
        let package = repo.join(
            catalog["packages"]["builtin/release-notes"]["path"]
                .as_str()
                .unwrap(),
        );
        let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
            toml::from_str(&std::fs::read_to_string(package.join("pipeline.toml")).unwrap())
                .unwrap();
        pipeline.max_attempts = 6;
        std::fs::write(
            package.join("pipeline.toml"),
            toml::to_string(&pipeline).unwrap(),
        )
        .unwrap();
        catalog["packages"]["builtin/release-notes"]["digest"] =
            json!(review_config::lock::package_digest("builtin/release-notes", &package).unwrap());
        std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        let task_path = repo.join("document.json");
        let mut task: Value = serde_json::from_slice(&std::fs::read(&task_path).unwrap()).unwrap();
        task["limits"] = json!({"tokens":40000,"max_attempts":10,"wall_ms":600000,"verification":{"tokens":0,"attempts":2,"wall_ms":10000}});
        std::fs::write(task_path, serde_json::to_vec(&task).unwrap()).unwrap();
        commit(&repo);
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Self {
            _root: root,
            repo,
            state,
            home,
            path,
        }
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USER", "fixture")
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", self.home.join("providers.toml"))
            .args(args)
            .args(["--json", "--state"])
            .arg(&self.state)
            .output()
            .unwrap()
    }

    /// Run and check the exit code; stdout must be exactly one secret-free JSON document.
    fn json(&self, args: &[&str], code: i32) -> Value {
        let out = self.cli(args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(code),
            "{args:?}\n{stdout}\n{stderr}"
        );
        for text in [&stdout, &stderr] {
            assert!(!text.contains("FIXTURE_SECRET"), "{args:?} leaked: {text}");
        }
        let value: Value = serde_json::from_str(&stdout).unwrap();
        if value["schema"] == "af/task-auth-recovery@1" {
            crate::schemas::valid(
                &crate::schemas::validator("task-auth-recovery-v1.json"),
                &value,
            );
            assert_eq!(value["exit_code"], code);
        }
        value
    }

    fn set(&self, flag: &str, on: bool) {
        let path = self.home.join(flag);
        if on {
            std::fs::write(path, b"").unwrap();
        } else {
            let _ = std::fs::remove_file(path);
        }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.home.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn assert_no_secret_in_state(&self) {
        let events = std::fs::read(self.state.join("events.sqlite")).unwrap();
        assert!(
            !String::from_utf8_lossy(&events).contains("FIXTURE_SECRET"),
            "the Store retained native auth text"
        );
        for entry in walk(&self.state.join("cas")) {
            let bytes = std::fs::read(&entry).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("FIXTURE_SECRET"),
                "{} retained native auth text",
                entry.display()
            );
        }
    }
}

fn walk(path: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path);
            }
        }
    }
    found
}

const SUSPENDED: &str = "needs_provider_auth";

#[test]
fn a_revoked_session_suspends_and_verified_recovery_continues_the_same_task_once() {
    let f = Fixture::new(true);
    let planned = success(f.cli(&["task", "plan", "--file", "document.json"]));
    assert_eq!(
        planned["graph"]["auth_recovery"],
        json!({"probes":2,"tokens_per_probe":32,"wall_ms_per_probe":45000})
    );
    assert_eq!(
        planned["graph"]["allowances"]["root.providers.admit0"]["max_attempts"],
        3
    );
    // The status says signed in; the real inference says the token expired.
    f.set("revoked", true);
    let run = f.json(
        &[
            "task",
            "run",
            "--execute",
            "release-notes",
            "--requester-ref",
            "user-1",
            "--coordinator-ref",
            "chat-1",
        ],
        3,
    );
    assert_eq!(run["phase"]["reason"], SUSPENDED, "{run}");
    assert_eq!(f.calls(), vec!["admission"]);
    // Running again spends nothing: only verification continues a suspended Task.
    let again = f.json(&["task", "run", "--execute", "release-notes"], 3);
    assert_eq!(again["schema"], "af/task-auth-recovery@1");
    assert_eq!(again["state"], "suspended");
    assert_eq!(f.calls(), vec!["admission"]);

    // A stale "authenticated" status earns one bounded probe; it fails, so the coordinator is
    // asked for the private login and nothing is reported as recovered.
    let stale = f.json(&["task", "recover", "release-notes"], 3);
    assert_eq!(stale["state"], "login_required", "{stale}");
    assert_eq!(
        stale["login"],
        json!([{"provider":"codex-personal","provider_kind":"codex"}])
    );
    assert_eq!(stale["contexts"][0]["status"], "failed");
    assert_eq!(stale["contexts"][0]["generation"], 1);
    assert_eq!(stale["accounting"]["chargeable_tokens"], "10");
    assert_eq!(stale["accounting"]["verification_tokens"], "5");
    assert_eq!(f.calls(), vec!["admission", "admission"]);
    let notified: Vec<_> = stale["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["outcome"].clone(), n["coordinator_ref"].clone()))
        .collect();
    assert_eq!(notified, vec![(json!("login_required"), json!("chat-1"))]);

    // Signed out entirely: the same low-effort handoff, with no paid call at all.
    f.set("revoked", false);
    f.set("logged_out", true);
    let missing = f.json(&["task", "recover", "release-notes"], 3);
    assert_eq!(missing["state"], "login_required");
    assert_eq!(missing["accounting"]["chargeable_tokens"], "10");
    assert_eq!(f.calls().len(), 2);
    // A login reference that names no completed private login is refused, not recorded.
    let out = f.cli(&[
        "task",
        "recover",
        "release-notes",
        "--login-ref",
        &"a".repeat(64),
    ]);
    assert!(!out.status.success());

    // The private login completed (outside af's ordinary streams); verification passes and
    // the original Task continues: the admission retries under its captured allowance and the
    // author runs exactly once.
    f.set("logged_out", false);
    let resumed = f.json(&["task", "recover", "release-notes"], 0);
    assert_eq!(resumed["state"], "terminal", "{resumed}");
    assert_eq!(resumed["result"]["acceptance"], "satisfied");
    assert_eq!(resumed["continuation"], json!({"available": false}));
    assert_eq!(
        f.calls(),
        vec!["admission", "admission", "admission", "admission", "author"]
    );
    assert_eq!(resumed["accounting"]["chargeable_tokens"], "25");
    assert_eq!(resumed["accounting"]["verification_tokens"], "10");
    // The ordinary run's four Attempts, plus the failed admission and the two probes.
    assert_eq!(resumed["accounting"]["begun_attempts"], 4 + 1 + 2);
    // A retried recovery after interruption runs nothing again.
    let replay = f.json(&["task", "recover", "release-notes"], 0);
    assert_eq!(replay["accounting"], resumed["accounting"]);
    assert_eq!(f.calls().len(), 5);

    // The originating coordinator is told once per outcome; acknowledging is idempotent and
    // fenced to that coordinator.
    let pending = resumed["notifications"].as_array().unwrap().clone();
    let outcomes: Vec<_> = pending.iter().map(|n| n["outcome"].clone()).collect();
    assert_eq!(outcomes, vec![json!("login_required"), json!("resumed")]);
    for notification in &pending {
        let sequence = notification["outcome_sequence"].to_string();
        let context = notification["context_key"].as_str().unwrap();
        let wrong = f.cli(&[
            "task",
            "recover",
            "release-notes",
            "--acknowledge",
            &sequence,
            "--context",
            context,
            "--coordinator-ref",
            "chat-2",
            "--delivery-ref",
            "message-1",
        ]);
        assert!(!wrong.status.success());
        for _ in 0..2 {
            f.json(
                &[
                    "task",
                    "recover",
                    "release-notes",
                    "--acknowledge",
                    &sequence,
                    "--context",
                    context,
                    "--coordinator-ref",
                    "chat-1",
                    "--delivery-ref",
                    "message-1",
                ],
                0,
            );
        }
    }
    let done = f.json(&["task", "recover", "release-notes"], 0);
    assert_eq!(done["notifications"], json!([]));
    f.assert_no_secret_in_state();
}

#[test]
fn quota_never_asks_for_login_and_a_terminal_task_continues_only_through_a_linked_successor() {
    let f = Fixture::new(false);
    success(f.cli(&["task", "plan", "--file", "document.json"]));
    f.set("quota", true);
    let failed = f.json(&["task", "run", "--execute", "release-notes"], 4);
    assert!(failed["result"].is_object(), "{failed}");
    let terminal = f.json(&["task", "recover", "release-notes"], 0);
    assert_eq!(
        terminal["state"], "terminal",
        "a quota failure is no login problem"
    );
    assert!(terminal.get("login").is_none());
    assert_eq!(terminal["continuation"]["available"], true);
    let charged = terminal["accounting"]["chargeable_tokens"].clone();
    let result_id = terminal["result"]["result_id"].as_str().unwrap().to_owned();
    f.set("quota", false);
    // The link needs the exact recorded result; the predecessor is never reopened.
    let wrong = f.cli(&[
        "task",
        "continue",
        "release-notes",
        "--task-id",
        "release-notes-2",
        "--confirm-result",
        &format!("sha256:{}", "0".repeat(64)),
    ]);
    assert!(!wrong.status.success());
    let successor = success(f.cli(&[
        "task",
        "continue",
        "release-notes",
        "--task-id",
        "release-notes-2",
        "--confirm-result",
        &result_id,
    ]));
    assert!(successor["admitted"] == json!(false) || successor["phase"]["kind"] != "running");
    let limits = &successor["plan"]["limits"];
    assert_eq!(
        limits["tokens"].as_u64().unwrap(),
        40000 - charged.as_str().unwrap().parse::<u64>().unwrap()
    );
    let again = f.cli(&[
        "task",
        "continue",
        "release-notes",
        "--task-id",
        "release-notes-3",
        "--confirm-result",
        &result_id,
    ]);
    assert!(!again.status.success(), "one finished Task, one successor");
    let plan = successor["plan_id"].as_str().unwrap().to_owned();
    let ran = f.json(
        &["task", "run", "release-notes-2", "--confirm-plan", &plan],
        0,
    );
    assert_eq!(ran["result"]["acceptance"], "satisfied", "{ran}");
    let predecessor = f.json(&["task", "recover", "release-notes"], 0);
    assert_eq!(predecessor["accounting"]["chargeable_tokens"], charged);
    assert_eq!(
        predecessor["continuation"],
        json!({"available": false, "successor_task_id": "release-notes-2"})
    );
}
