//! Common CLI factory currentness: account changes inside one process never rebind a Task.
use super::*;
use review_store::{Cas, EventStore};
use serde_json::Value;

fn account_fixture(directory: &Path, change_during_identity: bool) -> (PathBuf, PathBuf, String) {
    let (repo, home, state) = native_diff_fixture(directory);
    let auth = home.join("codex-auth");
    std::fs::write(auth.join("email"), "fixture@example.test").unwrap();
    if change_during_identity {
        std::fs::write(auth.join("change-during-identity"), b"fixture").unwrap();
    }
    std::fs::write(home.join("codex"), r#"#!/usr/bin/python3
import os,sys,json
home=os.environ['CODEX_HOME']
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  request=json.loads(line)
  if request.get('id')==1: print(json.dumps({'id':1,'result':{}}),flush=True)
  if request.get('id')==2:
   email=open(home+'/email').read()
   if os.path.isfile(home+'/change-during-identity'):
    with open(home+'/email','w') as f: f.write('private-other@example.test')
   print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':email}}}),flush=True)
 sys.exit(0)
request=sys.stdin.read()
with open(home+'/calls','a') as f: f.write(open(home+'/email').read()+' '+('probe' if request=='Reply with exactly: OK\n' else 'worker')+'\n')
assert request=='Reply with exactly: OK\n', 'private Reviewer context must not reach this changed account'
with open(sys.argv[sys.argv.index('-o')+1],'w') as f: f.write('OK')
with open(home+'/email','w') as f: f.write('private-other@example.test')
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':1,'output_tokens':1}}))
"#).unwrap();
    (repo, home, state)
}

fn invoke(repo: &Path, home: &Path, state: &str, doctor: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_af"));
    command.args(if doctor {
        ["provider", "doctor"]
    } else {
        ["review", "run"]
    });
    command
        .args([
            "--repo",
            repo.to_str().unwrap(),
            "--pipeline",
            PIPELINE,
            "--campaign",
            "currentness",
            "--state",
            state,
            "--heavy",
            "--policy-rev",
            "HEAD",
            "--base",
            "HEAD^",
            "--candidate",
            "HEAD",
            "--provider",
            "reviewer=test-codex",
            "--json",
        ])
        .current_dir(repo)
        .env("HOME", home)
        .env("USER", "loop-test")
        .env(
            "AF_PROVIDERS_FILE",
            home.join(".config/afactory/providers.toml"),
        )
        .env(
            "PATH",
            std::env::join_paths(
                std::iter::once(home.to_path_buf())
                    .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
            )
            .unwrap(),
        )
        .output()
        .unwrap()
}

#[test]
fn review_account_change_after_admission_refuses_private_context_and_keeps_admission() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = account_fixture(directory.path(), false);
    let output = invoke(&repo, &home, &state, false);
    assert_eq!(
        output.status.code(),
        Some(4),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    contracts::valid("review-outcome-v2.json", &value);
    assert_eq!(value["task"]["committed_tokens"], "2");
    let id = value["task"]["task_id"].as_str().unwrap();
    let cas = Cas::open(Path::new(&state).join("cas")).unwrap();
    let store = EventStore::open_read_only(Path::new(&state).join("events.sqlite")).unwrap();
    let projection = store.task_projection(&cas, id).unwrap().unwrap();
    let execution = projection.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 2);
    let providers: Vec<_> = execution
        .graph
        .nodes
        .iter()
        .filter(|(_, node)| {
            matches!(
                node.operator,
                review_graph::task::CompiledOperator::ProviderAdmission { .. }
                    | review_graph::task::CompiledOperator::ProviderAdmissionBrokered { .. }
            )
        })
        .collect();
    assert_eq!(providers.len(), 1);
    assert!(
        execution.outputs.contains_key(providers[0].0),
        "succeeded admission was lost"
    );
    let calls = std::fs::read_to_string(home.join("codex-auth/calls")).unwrap();
    assert_eq!(calls, "fixture@example.test probe\n");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !text.contains("private-other@example.test"),
        "account status leaked to ordinary output"
    );
    let shown = Command::new(env!("CARGO_BIN_EXE_af"))
        .args(["task", "show", id, "--state", &state, "--json"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["chargeable_tokens"], "2");
    assert_eq!(
        std::fs::read_to_string(home.join("codex-auth/calls")).unwrap(),
        calls
    );
}

#[test]
fn doctor_account_change_before_probe_refuses_known_zero_without_business_or_report() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = account_fixture(directory.path(), true);
    let output = invoke(&repo, &home, &state, true);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    contracts::valid("provider-doctor-v2.json", &value);
    assert_eq!(value["ready"], false);
    assert_eq!(value["gates_run"], false);
    assert_eq!(value["workers_dispatched"], false);
    assert_eq!(value["task"]["committed_tokens"], "0");
    assert_eq!(value["task"]["begun_attempts"], "1");
    assert!(!home.join("codex-auth/calls").exists());
    let cas = Cas::open(Path::new(&state).join("cas")).unwrap();
    let store = EventStore::open_read_only(Path::new(&state).join("events.sqlite")).unwrap();
    let task = store
        .task_projection(&cas, value["task"]["task_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert!(task.run_reports.is_empty());
    assert_eq!(task.execution.unwrap().budget.committed_tokens(), 0);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains("private-other@example.test"));
}
