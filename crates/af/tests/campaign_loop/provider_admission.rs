//! Actual common CLI/Store capture and replay, using a deterministic local native protocol.
//! The synthetic cached/cold observations are sizing controls, not future Provider guarantees.
use super::*;
use review_store::{Cas, EventStore};
use serde_json::{Value, json};

struct Fixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    state: String,
}
impl Fixture {
    fn new(actual: u64, run_cap: Option<u64>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (repo, home, state) = native_diff_fixture(root.path());
        std::fs::write(home.join("codex"), r#"#!/usr/bin/python3 -B
import os,sys,json
home=os.environ['CODEX_HOME']
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  request=json.loads(line)
  if request.get('id')==1: print(json.dumps({'id':1,'result':{}}),flush=True)
  if request.get('id')==2: print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':'fixture@example.test'}}}),flush=True)
 sys.exit(0)
request=sys.stdin.read()
probe=request=='Reply with exactly: OK\n'
with open(home+'/calls','a') as f: f.write('admission\n' if probe else 'reviewer\n')
message='OK' if probe else json.dumps({'verdict':'approve','summary':None,'findings':[],'benchmark_demands':[],'disputes':[]})
with open(sys.argv[sys.argv.index('-o')+1],'w') as f: f.write(message)
usage=json.load(open(home+'/usage.json')) if probe else {'input_tokens':1,'output_tokens':1}
print(json.dumps({'type':'turn.completed','usage':usage}))
"#).unwrap();
        let usage = match actual {
            5712 => json!({"input_tokens":16331,"cached_input_tokens":10624,"output_tokens":5}),
            16336 => json!({"input_tokens":16331,"cached_input_tokens":0,"output_tokens":5}),
            _ => json!({"input_tokens":actual,"output_tokens":0}),
        };
        std::fs::write(
            home.join("codex-auth/usage.json"),
            serde_json::to_vec(&usage).unwrap(),
        )
        .unwrap();
        if let Some(run) = run_cap {
            let pipeline = std::fs::read_to_string(pipeline_path(&repo)).unwrap();
            set_pipeline(
                &repo,
                &format!(
                    "{pipeline}\n[budgets]\nunit = \"tokens\"\nattempt = 20000\nrun = {run}\n"
                ),
            );
            git(&repo, &home, &["add", "-A"]);
            git(
                &repo,
                &home,
                &["commit", "-qm", "capture finite Review authority"],
            );
        }
        Self {
            _root: root,
            repo,
            home,
            state,
        }
    }
    fn cli(&self, mode: &[&str], bounds: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_af"));
        cmd.args(mode)
            .args([
                "--repo",
                self.repo.to_str().unwrap(),
                "--pipeline",
                PIPELINE,
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
            .args(bounds);
        if mode != ["review", "plan"] {
            cmd.args(["--campaign", "admission-cost", "--state", &self.state]);
        }
        cmd.current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USER", "loop-test")
            .env(
                "AF_PROVIDERS_FILE",
                self.home.join(".config/af/providers.toml"),
            )
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(self.home.clone())
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            )
            .output()
            .unwrap()
    }
    fn calls(&self) -> String {
        std::fs::read_to_string(self.home.join("codex-auth/calls")).unwrap_or_default()
    }
    fn projection(&self, id: &str) -> review_store::store::task::TaskProjection {
        let cas = Cas::open_existing(Path::new(&self.state).join("cas")).unwrap();
        EventStore::open_read_only(Path::new(&self.state).join("events.sqlite"))
            .unwrap()
            .task_projection(&cas, id)
            .unwrap()
            .unwrap()
    }
}
fn value(out: std::process::Output, code: i32) -> Value {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
const OLD: &[&str] = &[
    "--provider-admission-tokens",
    "4096",
    "--provider-admission-wall-ms",
    "45000",
];
const NEW: &[&str] = &[
    "--provider-admission-tokens",
    "32768",
    "--provider-admission-wall-ms",
    "45000",
];

#[test]
fn common_review_captures_admission_cost_and_retains_exact_overruns() {
    for (actual, bounds, reserved, accepted) in [
        (5712, &[][..], 32768, true),
        (16336, NEW, 32768, true),
        (5712, OLD, 4096, false),
        (32769, NEW, 32768, false),
    ] {
        let f = Fixture::new(actual, None);
        let plan = value(f.cli(&["review", "plan"], bounds), 0);
        assert_eq!(
            plan["provider_admission"],
            json!({"tokens":reserved,"wall_ms":45000})
        );
        assert!(f.calls().is_empty());
        assert!(
            !Path::new(&f.state).exists(),
            "token-free planning created Campaign state"
        );
        let done = value(
            f.cli(&["review", "run"], bounds),
            if accepted { 0 } else { 4 },
        );
        let id = done["task"]["task_id"].as_str().unwrap();
        let p = f.projection(id);
        let e = p.execution.unwrap();
        assert_eq!(e.budget.breached(), !accepted);
        assert_eq!(
            e.budget.committed_tokens(),
            u128::from(actual + if accepted { 2 } else { 0 })
        );
        let attempts = e.attempt_accounting();
        let admission = attempts
            .iter()
            .find(|a| a.reservation.node.starts_with("root.providers."))
            .unwrap();
        assert_eq!(admission.reservation.tokens, reserved);
        assert_eq!(admission.charged_tokens, u128::from(actual));
        assert!(e.outputs.contains_key(&admission.reservation.node));
        assert_eq!(
            attempts.iter().any(|a| matches!(
                &e.graph.nodes[&a.reservation.node].operator,
                review_graph::task::CompiledOperator::ReviewDomain {
                    operation: review_graph::task::ReviewOperation::Reviewer { .. },
                    ..
                }
            ) && a.started),
            accepted
        );
        assert_eq!(
            f.calls(),
            if accepted {
                "admission\nreviewer\n"
            } else {
                "admission\n"
            }
        );
        let again = f.projection(id);
        assert_eq!(
            again.execution.unwrap().budget.committed_tokens(),
            e.budget.committed_tokens()
        );
    }
}

#[test]
fn doctor_resume_keeps_old_cost_and_refuses_changed_tokens_or_wall_before_events() {
    let f = Fixture::new(2, None);
    let first = value(f.cli(&["provider", "doctor"], OLD), 0);
    let id = first["task"]["task_id"].as_str().unwrap();
    let before = f.projection(id);
    let cas = Cas::open_existing(Path::new(&f.state).join("cas")).unwrap();
    let policy = cas
        .get_artifact(&before.revision.authority.policy_id)
        .unwrap();
    assert_eq!(
        policy.payload["settings"]["review"]["provider_admission"],
        json!({"tokens":4096,"wall_ms":45000})
    );
    // The live checkout cannot replace captured Review policy or package bytes on resume.
    std::fs::write(pipeline_path(&f.repo), "not a pipeline").unwrap();
    std::fs::write(
        f.repo.join(".af/workers/tester/reviewer.md"),
        "untrusted replacement",
    )
    .unwrap();
    // Defaults changed in the CLI, but omission and exact replay use the old captured pair.
    for bounds in [&[][..], OLD] {
        value(f.cli(&["provider", "doctor"], bounds), 0);
        let after = f.projection(id);
        assert_eq!(before.revision, after.revision);
        assert_eq!(before.plan_id, after.plan_id);
        assert_eq!(
            cas.get_artifact(&before.revision.authority.policy_id)
                .unwrap(),
            policy
        );
        assert_eq!(f.calls(), "admission\n");
    }
    for bounds in [
        NEW,
        &[
            "--provider-admission-tokens",
            "4096",
            "--provider-admission-wall-ms",
            "45001",
        ],
    ] {
        let store = EventStore::open_read_only(Path::new(&f.state).join("events.sqlite")).unwrap();
        let run = review_store::store::task::task_run_id(id).unwrap();
        let campaign = first["run_id"].as_str().unwrap();
        let prefix = (store.replay(&run).unwrap(), store.replay(campaign).unwrap());
        assert!(!prefix.0.is_empty());
        drop(store);
        let refused = f.cli(&["provider", "doctor"], bounds);
        assert!(!refused.status.success());
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("differs from the original captured Review Task")
        );
        let store = EventStore::open_read_only(Path::new(&f.state).join("events.sqlite")).unwrap();
        assert_eq!(
            (store.replay(&run).unwrap(), store.replay(campaign).unwrap()),
            prefix
        );
        assert_eq!(f.calls(), "admission\n");
    }
    // The successful Doctor's original admission still authorizes the later real Reviewer.
    value(f.cli(&["review", "run"], &[]), 0);
    assert_eq!(f.calls(), "admission\nreviewer\n");
    let after = f.projection(id);
    assert_eq!(before.revision, after.revision);
    assert_eq!(before.plan_id, after.plan_id);
    assert_eq!(after.execution.unwrap().budget.committed_tokens(), 4);
}

#[test]
fn admission_plus_mandatory_reviewer_must_fit_before_any_paid_attempt() {
    for mode in [["review", "run"], ["provider", "doctor"]] {
        // Both individual reservations fit; their 52768-token sum exceeds captured 52767.
        let f = Fixture::new(5712, Some(52767));
        let out = f.cli(&mode, NEW);
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("mandatory"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            f.calls().is_empty(),
            "aggregate refusal dispatched a paid capability"
        );
        let store = EventStore::open_read_only(Path::new(&f.state).join("events.sqlite")).unwrap();
        let cas = Cas::open_existing(Path::new(&f.state).join("cas")).unwrap();
        assert!(
            store.task_ids(&cas).unwrap().is_empty(),
            "infeasible capture opened a Task"
        );
    }
}

#[test]
fn admission_bounds_are_paired_positive_finite_cli_values() {
    let f = Fixture::new(5712, None);
    for bounds in [
        vec!["--provider-admission-tokens", "32768"],
        vec![
            "--provider-admission-tokens",
            "0",
            "--provider-admission-wall-ms",
            "45000",
        ],
        vec![
            "--provider-admission-tokens",
            "32768",
            "--provider-admission-wall-ms",
            "0",
        ],
        vec![
            "--provider-admission-tokens",
            "9007199254740992",
            "--provider-admission-wall-ms",
            "45000",
        ],
    ] {
        assert_eq!(
            f.cli(&["provider", "doctor"], &bounds).status.code(),
            Some(2)
        );
        assert!(!Path::new(&f.state).exists());
        assert!(f.calls().is_empty());
    }
}
