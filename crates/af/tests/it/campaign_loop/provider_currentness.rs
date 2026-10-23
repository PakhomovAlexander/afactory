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
    std::fs::write(home.join("codex"), crate::provider_cli::with_help("codex", &crate::provider_cli::with_probe(r#"#!/usr/bin/python3
import os,sys,json
home=os.environ['CODEX_HOME']
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  request=json.loads(line)
  if request.get('id')==1: print(json.dumps({'id':1,'result':{}}),flush=True)
  if request.get('id')==2:
   with open(home+'/identity-calls','a') as f: f.write('account\n')
   email=open(home+'/email').read()
   if os.path.isfile(home+'/change-during-identity'):
    with open(home+'/email','w') as f: f.write('private-other@example.test')
   print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':email}}}),flush=True)
 sys.exit(0)
request=sys.stdin.read()
with open(home+'/calls','a') as f: f.write(open(home+'/email').read()+' '+('probe' if request==PROBE_INPUT else 'worker')+'\n')
assert request==PROBE_INPUT, 'private Reviewer context must not reach this changed account'
with open(sys.argv[sys.argv.index('-o')+1],'w') as f: f.write('OK')
with open(home+'/email','w') as f: f.write('private-other@example.test')
if os.path.isfile(home+'/disable-after-admission'):
 import time
 path=os.path.dirname(home)+'/.config/af/provider-disables-v1.json'
 with open(path,'w') as f: json.dump({'schema':'af.provider-disables/1','providers':[{'id':'test-codex','expires_at':{'unix_seconds':int(time.time())+172800,'nanoseconds':0}}]},f)
 os.chmod(path,0o600)
 with open(home+'/accounts-at-disable','w') as f: f.write(open(home+'/identity-calls').read())
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':1,'output_tokens':1}}))
"#))).unwrap();
    (repo, home, state)
}

fn invoke(repo: &Path, home: &Path, state: &str, doctor: bool) -> std::process::Output {
    let mut command = crate::common::af();
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
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("USER", "loop-test")
        .env("AF_PROVIDERS_FILE", home.join(".config/af/providers.toml"))
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
    contracts::valid("review-outcome-v3.json", &value);
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
    let shown = crate::common::af()
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AF_SELF_OFFLINE", "1")
        .args(["task", "show", id, "--state", &state, "--json"])
        .output()
        .unwrap();
    assert!(
        shown.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&shown.stdout),
        String::from_utf8_lossy(&shown.stderr)
    );
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

#[test]
fn review_disable_after_admission_refuses_captured_private_send_before_account_probe() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = account_fixture(directory.path(), false);
    std::fs::write(home.join("codex-auth/disable-after-admission"), b"fixture").unwrap();
    let output = invoke(&repo, &home, &state, false);
    assert_eq!(
        output.status.code(),
        Some(4),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    contracts::valid("review-outcome-v3.json", &value);
    assert_eq!(value["task"]["committed_tokens"], "2");
    assert_eq!(
        std::fs::read_to_string(home.join("codex-auth/calls")).unwrap(),
        "fixture@example.test probe\n"
    );
    assert_eq!(
        std::fs::read(home.join("codex-auth/identity-calls")).unwrap(),
        std::fs::read(home.join("codex-auth/accounts-at-disable")).unwrap(),
        "disabled Provider was asked about its account"
    );
    let id = value["task"]["task_id"].as_str().unwrap();
    let cas = Cas::open(Path::new(&state).join("cas")).unwrap();
    let store = EventStore::open_read_only(Path::new(&state).join("events.sqlite")).unwrap();
    let execution = store
        .task_projection(&cas, id)
        .unwrap()
        .unwrap()
        .execution
        .unwrap();
    let mut refused = 0;
    for attempt in execution.attempt_accounting() {
        if let Some(review_core::task::execution::TaskAttemptResultV1::Failed {
            diagnostic_id,
            ..
        }) = &attempt.result
        {
            let diagnostic = cas.get_json(diagnostic_id).unwrap().to_string();
            if diagnostic.contains("disabled until ") && diagnostic.contains("Z (UTC)") {
                assert!(
                    diagnostic.contains("af provider enable test-codex"),
                    "{diagnostic}"
                );
                assert_eq!(attempt.charged_tokens, 0);
                refused += 1;
            }
        }
    }
    assert!(
        refused > 0,
        "no disabled private-send diagnostic was recorded"
    );
}
