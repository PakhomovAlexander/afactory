//! Issue #263, end to end: a fake `claude` answers the Provider-admission probe exactly like the
//! result Claude Code returned on 2026-10-13, 2026-10-14 and 2026-10-15, when the safety
//! classifier of the bound `claude-fable-5-1` flagged the probe and `claude-opus-5` answered.
//! `af review run` refuses the Provider before any business Worker starts, charges the 9496
//! tokens the result reports, and says on its own stderr what happened and what to do next
//! (ADR-0147).
use std::process::Command;

use serde_json::Value;

use crate::task_cli::fixture_named;

/// The recorded result, byte for byte in its numbers.
const RECORDED_FALLBACK: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"OK","num_turns":2,"safety_stops":2,"stop_reason":"end_turn","fallback_credit":null,"usage":{"input_tokens":4,"output_tokens":4,"cache_creation_input_tokens":9488,"cache_read_input_tokens":3001},"modelUsage":{"claude-fable-5-1":{"inputTokens":2,"outputTokens":0,"cacheCreationInputTokens":3210,"cacheReadInputTokens":3001,"canonicalModel":"claude-fable-5-1"},"claude-opus-5":{"inputTokens":2,"outputTokens":4,"cacheCreationInputTokens":6278,"cacheReadInputTokens":0,"canonicalModel":"claude-opus-5"}}}"#;

/// The review fixture with both business Workers bound to `model` at effort `xhigh` on one Claude
/// Provider, whose fake `claude` answers every model call with `result`.
struct Fixture {
    _directory: tempfile::TempDir,
    repo: std::path::PathBuf,
    state: std::path::PathBuf,
    home: std::path::PathBuf,
    path: std::ffi::OsString,
    registry: std::path::PathBuf,
}

impl Fixture {
    fn new(model: &str, result: &str) -> Self {
        Self::with_models([model, model], result)
    }

    /// The fixture with `fixture/bugs` bound to `models[0]` and `fixture/correctness` to
    /// `models[1]`, both on the one Claude Provider `claude-personal`.
    fn with_models(models: [&str; 2], result: &str) -> Self {
        use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = fixture_named(directory.path(), "review");
        let home = directory.path().join("home");
        let bin = directory.path().join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(home.join("help"), crate::provider_cli::CLAUDE_HELP).unwrap();
        std::fs::write(home.join("version"), "2.1.300 (Claude Code)\n").unwrap();
        std::fs::write(home.join("result"), result).unwrap();
        // The fake records every process af starts, every model call with the guard it
        // received, and the stdin of the first model call, and answers each model call with
        // `result`; any request other than the admission probe is a business Worker. It serves
        // no model catalog: it ends plan time's token-free catalog session at once (ADR-0148).
        let stub = r#"#!/usr/bin/python3
import os,json,sys
home=os.environ['CLAUDE_CONFIG_DIR']
with open(home+'/started','a') as f: f.write(' '.join(sys.argv[1:3])+'\n')
if sys.argv[1:] in (['--help'],['--version']):
 sys.stdout.write(open(home+'/'+sys.argv[1][2:]).read())
 sys.exit(0)
if sys.argv[1:3]==['auth','status']:
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':'developer@example.test'}))
 sys.exit(0)
if '--input-format' in sys.argv:
 sys.exit(0)
request=sys.stdin.read()
if not os.path.exists(home+'/first-stdin'): open(home+'/first-stdin','w').write(request)
kind='admission' if request==PROBE_INPUT else 'business'
with open(home+'/calls','a') as f: f.write(kind+' '+os.environ.get('CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK','unset')+' '+os.environ.get('CLAUDE_CODE_NO_MODEL_FALLBACK','unset')+'\n')
sys.stdout.write(open(home+'/result').read())
"#;
        let installed = bin.join("claude");
        std::fs::write(&installed, crate::provider_cli::with_probe(stub)).unwrap();
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
        let registry = home.join("providers.toml");
        std::fs::write(&registry,toml::to_string(&serde_json::json!({"version":1,"providers":[{"id":"claude-personal","kind":"claude","auth_dir":home}]})).unwrap()).unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        let mut providers = toml::map::Map::new();
        for (name, model) in ["bugs", "correctness"].into_iter().zip(models) {
            let package = format!("fixture/{name}");
            let path = repo.join(format!(".af/task-packages/{package}"));
            let mut worker: TaskWorkerManifest =
                toml::from_str(&std::fs::read_to_string(path.join("worker.toml")).unwrap())
                    .unwrap();
            worker.runner = TaskWorkerRunner::Model {
                provider_kind: "claude".into(),
                model: model.into(),
                effort: "xhigh".into(),
            };
            let attempt = worker.signature.attempt.as_mut().unwrap();
            attempt.tokens = 1000;
            attempt.wall_ms = super::NATIVE_REVIEWER_WALL_MS;
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
        file["limits"] = super::native_model_limits();
        std::fs::write(file_path, serde_json::to_vec(&file).unwrap()).unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "model bindings"]] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let path = std::env::join_paths(
            std::iter::once(bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Self {
            _directory: directory,
            repo,
            state,
            home,
            path,
            registry,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        crate::common::af()
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USER", "fixture")
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", &self.registry)
            .args(args)
            .args(["--json", "--state"])
            .arg(&self.state)
            .output()
            .unwrap()
    }

    /// The model calls the fake received, one line each: admission or business, then the two
    /// guards.
    fn calls(&self) -> String {
        std::fs::read_to_string(self.home.join("calls")).unwrap_or_default()
    }

    /// The exact stdin of the first model call, the admission probe; empty when none ran.
    fn first_stdin(&self) -> Vec<u8> {
        std::fs::read(self.home.join("first-stdin")).unwrap_or_default()
    }

    /// Every process the fake was started as, one line each with its first two arguments; empty
    /// when none started.
    fn started(&self) -> String {
        std::fs::read_to_string(self.home.join("started")).unwrap_or_default()
    }

    /// The lines on `stderr` that name a failed Provider-admission Attempt.
    fn admission_lines(stderr: &str) -> Vec<&str> {
        stderr
            .lines()
            .filter(|line| line.starts_with("af: root.providers."))
            .collect()
    }

    /// `af task show review-cli`, and the diagnostics of its failed Attempts.
    fn shown(&self) -> (Value, Vec<String>) {
        let shown = self.run(&["task", "show", "review-cli"]);
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        let diagnostics = shown["execution_records"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|record| record["diagnostic"]["error"].as_str())
            .map(str::to_owned)
            .collect();
        (shown, diagnostics)
    }
}

#[test]
fn a_safety_classifier_fallback_at_admission_is_refused_charged_and_named_on_stderr() {
    let fixture = Fixture::new("claude-fable-5-1", RECORDED_FALLBACK);
    let output = fixture.run(&["review", "run", "--file", "review.json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stdout}\n{stderr}");
    // Only the admission probe ran, with the fallback turned off and model substitution kept.
    assert_eq!(fixture.calls(), "admission 1 unset\n", "{stderr}");
    // One line on the run's own stderr names the node, the Provider, the bound model and effort,
    // both models with their output tokens, the counts, the cause, af's refusal and the next step.
    let lines = Fixture::admission_lines(&stderr);
    assert_eq!(lines.len(), 1, "{stderr}");
    let line = lines[0];
    for part in [
        "failed: Provider claude-personal (claude), model claude-fable-5-1, effort xhigh: ",
        "the safety classifier of claude-fable-5-1 flagged the request and Claude Code switched models, a safety-classifier fallback (model_refused)",
        "the Worker bound claude-fable-5-1 (effort xhigh), which wrote 0 output tokens",
        "claude-opus-5 (4 output tokens) answered (safety_stops 2, num_turns 2)",
        "af refused the reply, because af never runs or accepts a model the Worker did not bind",
        "this is neither a credential nor a model-spelling problem",
        "next: af already sets CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1",
    ] {
        assert!(line.contains(part), "{part}: {line}");
    }
    let probe = std::str::from_utf8(review_runner::task::provider::PROBE_INPUT).unwrap();
    for absent in [probe.trim_end(), "developer@example.test", "transport:"] {
        assert!(!line.contains(absent), "{absent}: {line}");
    }
    // The probe is charged exactly what its result reports, and `af task show` keeps the full
    // diagnostic.
    let (shown, diagnostics) = fixture.shown();
    assert_eq!(shown["chargeable_tokens"], "9496", "{shown}");
    assert_eq!(diagnostics.len(), 1, "{shown}");
    assert!(
        diagnostics[0]
            .starts_with("Claude Worker failed: the safety classifier of claude-fable-5-1"),
        "{}",
        diagnostics[0]
    );
    assert!(
        diagnostics[0].ends_with("; transport: exited 0"),
        "{}",
        diagnostics[0]
    );
}

/// Round 2, (1): the recorded fallback with its top-level `output_tokens` malformed. The
/// diagnostic starts with the switch hint, the accounting reason follows it, and `af task run`
/// prints the hint on its stderr.
#[test]
fn a_malformed_top_level_usage_still_names_the_fallback_on_task_run_stderr() {
    let malformed = RECORDED_FALLBACK.replacen(r#""output_tokens":4"#, r#""output_tokens":"4""#, 1);
    assert_ne!(malformed, RECORDED_FALLBACK);
    let fixture = Fixture::new("claude-fable-5-1", &malformed);
    let plan = fixture.run(&["review", "plan", "--file", "review.json"]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    assert_eq!(fixture.calls(), "", "planning made a model call");
    let output = fixture.run(&["task", "run", "--execute", "review-cli"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert_eq!(fixture.calls(), "admission 1 unset\n", "{stderr}");
    let lines = Fixture::admission_lines(&stderr);
    assert_eq!(lines.len(), 1, "{stderr}");
    for part in [
        "failed: Provider claude-personal (claude), model claude-fable-5-1, effort xhigh: the safety classifier of claude-fable-5-1 flagged the request and Claude Code switched models, a safety-classifier fallback (model_refused)",
        "the Worker bound claude-fable-5-1 (effort xhigh), which wrote 0 output tokens, but claude-opus-5 (4 output tokens) answered (safety_stops 2, num_turns 2)",
    ] {
        assert!(lines[0].contains(part), "{part}: {}", lines[0]);
    }
    let (shown, diagnostics) = fixture.shown();
    assert_eq!(diagnostics.len(), 1, "{shown}");
    let found = review_runner_claude::hint::find(&diagnostics[0]).unwrap();
    assert!(lines[0].ends_with(found), "{}", lines[0]);
    assert!(
        diagnostics[0].contains(&format!(
            "Claude Worker failed: {found}; Claude top-level usage is malformed"
        )),
        "{}",
        diagnostics[0]
    );
}

/// Round 2, (2): a binding whose `--model` is a path is refused by `af task plan` and `af review
/// run` before any process starts, and no output names the path.
#[test]
fn a_binding_outside_g_is_refused_before_any_process_and_never_printed() {
    let fixture = Fixture::new("/Users/alice/keys-1", RECORDED_FALLBACK);
    for args in [
        &["task", "plan", "--file", "review.json"][..],
        &["review", "run", "--file", "review.json"][..],
    ] {
        let output = fixture.run(args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}: {stdout}\n{stderr}");
        assert!(
            stderr.contains("its --model value (19 bytes) is not a model spelling af accepts"),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains(review_runner_claude::hint::PRINTABLE_RULE),
            "{stderr}"
        );
        for text in [&stdout, &stderr] {
            for part in ["/Users", "alice", "keys-1"] {
                assert!(!text.contains(part), "{args:?} {part}: {text}");
            }
        }
    }
    assert!(
        !fixture.home.join("started").exists(),
        "a binding outside G started a process"
    );
}

/// Round 2, (3): an error result whose `result` carries the safeguards text and whose `errors`
/// is `[]`, `null` or an object without a string `message` is `model_refused`: not retried, its
/// hint at the head of the diagnostic and on the run's stderr.
#[test]
fn a_safeguards_refusal_beside_stringless_errors_is_named_and_not_retried() {
    for errors in ["[]", "null", r#"{"code":2}"#] {
        let result = format!(
            r#"{{"type":"result","subtype":"success","is_error":true,"result":"Claude Fable 5.1's safeguards flagged this message (https://www.anthropic.com/legal/aup). This sometimes happens with safe, normal conversations.","errors":{errors},"usage":{{"input_tokens":2,"output_tokens":0,"cache_creation_input_tokens":3210,"cache_read_input_tokens":3001}},"modelUsage":{{"claude-fable-5-1":{{"inputTokens":2,"outputTokens":0,"cacheCreationInputTokens":3210,"cacheReadInputTokens":3001}}}}}}"#
        );
        let fixture = Fixture::new("claude-fable-5-1", &result);
        let output = fixture.run(&["review", "run", "--file", "review.json"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{errors}: {stderr}");
        // One Attempt of four allowed: a refusal is never retried.
        assert_eq!(fixture.calls(), "admission 1 unset\n", "{errors}: {stderr}");
        let lines = Fixture::admission_lines(&stderr);
        assert_eq!(lines.len(), 1, "{errors}: {stderr}");
        assert!(
            lines[0].contains(
                "model claude-fable-5-1, effort xhigh: the safety classifier of claude-fable-5-1 \
                 (effort xhigh) flagged the request and claude-fable-5-1 refused it \
                 (model_refused; Claude Code reported that its safeguards flagged this message)"
            ),
            "{errors}: {}",
            lines[0]
        );
        assert!(lines[0].ends_with("then rerun"), "{}", lines[0]);
        let (shown, diagnostics) = fixture.shown();
        assert_eq!(diagnostics.len(), 1, "{shown}");
        assert!(
            review_runner_claude::hint::find(&diagnostics[0]).is_some(),
            "{}",
            diagnostics[0]
        );
        assert_eq!(shown["chargeable_tokens"], "3212", "{shown}");
    }
}

/// Round 3, (1): 32 explicitly unused entries and then one active foreign entry. The switch
/// decision reads the whole map: the diagnostic starts with the hint naming that entry, the
/// map-size reason follows it, and `af task run` prints the hint on its stderr.
#[test]
fn a_foreign_activity_after_32_unused_entries_is_named_on_task_run_stderr() {
    let unused = (0..32)
        .map(|i| format!(r#""claude-unused-{i:02}":{{"inputTokens":0,"outputTokens":0,"cacheCreationInputTokens":0,"cacheReadInputTokens":0}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let result = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"result":"OK","num_turns":2,"safety_stops":2,"stop_reason":"end_turn","usage":{{"input_tokens":2,"output_tokens":4,"cache_creation_input_tokens":6278,"cache_read_input_tokens":0}},"modelUsage":{{{unused},"claude-opus-5":{{"inputTokens":2,"outputTokens":4,"cacheCreationInputTokens":6278,"cacheReadInputTokens":0}}}}}}"#
    );
    let fixture = Fixture::new("claude-fable-5-1", &result);
    let plan = fixture.run(&["review", "plan", "--file", "review.json"]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let output = fixture.run(&["task", "run", "--execute", "review-cli"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert_eq!(fixture.calls(), "admission 1 unset\n", "{stderr}");
    let lines = Fixture::admission_lines(&stderr);
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(
        lines[0].contains(
            "model claude-fable-5-1, effort xhigh: the safety classifier of claude-fable-5-1 \
             flagged the request and Claude Code switched models, a safety-classifier fallback \
             (model_refused): the Worker bound claude-fable-5-1 (effort xhigh), which reported no \
             usage, but claude-opus-5 (4 output tokens) answered (safety_stops 2, num_turns 2)"
        ),
        "{}",
        lines[0]
    );
    let (shown, diagnostics) = fixture.shown();
    assert_eq!(diagnostics.len(), 1, "{shown}");
    let found = review_runner_claude::hint::find(&diagnostics[0]).unwrap();
    assert!(lines[0].ends_with(found), "{}", lines[0]);
    assert!(
        diagnostics[0].contains(&format!(
            "Claude Worker failed: {found}; Claude model usage exceeds its finite nonempty model \
             bound"
        )),
        "{}",
        diagnostics[0]
    );
    // The charge reads at most 32 entries (ADR-0102): the top-level summary is the floor.
    assert_eq!(shown["chargeable_tokens"], "6284", "{shown}");
}

/// Round 3, (4): every Model Worker passes the static rules before any Provider CLI starts. A
/// valid first Worker and an invalid second one are refused by name, with the Provider label and
/// the rule, and the fake `claude` never ran.
#[test]
fn an_invalid_second_binding_is_refused_before_any_provider_process() {
    for (second, rule) in [
        (
            "bad@account",
            "its --model value (11 bytes) is not a model spelling af accepts",
        ),
        (
            "fable",
            "resolves to the exact ID claude-fable-5-1 today, and af binds exact IDs",
        ),
        (
            "claude-fable",
            "Task Model binding requires an explicit versioned model ID",
        ),
    ] {
        let fixture = Fixture::with_models(["claude-fable-5-1", second], RECORDED_FALLBACK);
        for args in [
            &["task", "plan", "--file", "review.json"][..],
            &["review", "run", "--file", "review.json"][..],
        ] {
            let output = fixture.run(args);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{args:?}: {stderr}");
            assert!(
                stderr.contains(
                    "Worker fixture/correctness on Provider claude-personal: af refused the \
                     binding of model "
                ),
                "{args:?}: {stderr}"
            );
            assert!(stderr.contains(rule), "{args:?}: {stderr}");
            assert!(
                stderr.contains("`af provider models claude-personal`"),
                "{args:?}: {stderr}"
            );
            assert!(!stderr.contains("account"), "{stderr}");
        }
        assert_eq!(
            fixture.started(),
            "",
            "{second}: a Provider process started"
        );
        assert_eq!(fixture.calls(), "", "{second}");
    }
}

/// Round 3, (4): with every binding valid, the second pass probes the Provider once per label,
/// however many Workers share it.
#[test]
fn all_valid_bindings_probe_each_provider_label_once() {
    let fixture = Fixture::new("claude-fable-5-1", RECORDED_FALLBACK);
    let output = fixture.run(&["task", "plan", "--file", "review.json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let started = fixture.started();
    assert_eq!(
        started
            .lines()
            .filter(|line| *line == "auth status")
            .count(),
        1,
        "{started}"
    );
    assert_eq!(fixture.calls(), "", "planning made a model call");
}

/// Round 4: the admission Attempt sends exactly [`PROBE_INPUT`], the text that states its purpose,
/// and passes only on the reply `OK`. A clean one-model result answering `OK` admits the Provider
/// and the business Workers start; the same result answering anything else refuses it before
/// any business Worker.
///
/// [`PROBE_INPUT`]: review_runner::task::provider::PROBE_INPUT
#[test]
fn the_admission_probe_sends_the_purpose_stating_text_and_passes_only_on_ok() {
    use review_runner::task::provider::PROBE_INPUT;
    let answer = |reply: &str| {
        format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"result":"{reply}","num_turns":1,"safety_stops":0,"stop_reason":"end_turn","usage":{{"input_tokens":3,"output_tokens":1,"cache_creation_input_tokens":360,"cache_read_input_tokens":0}},"modelUsage":{{"claude-fable-5-1":{{"inputTokens":3,"outputTokens":1,"cacheCreationInputTokens":360,"cacheReadInputTokens":0}}}}}}"#
        )
    };
    let admitted = Fixture::new("claude-fable-5-1", &answer("OK"));
    let output = admitted.run(&["review", "run", "--file", "review.json"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(admitted.first_stdin(), PROBE_INPUT, "{stderr}");
    let calls = admitted.calls();
    assert!(
        calls.starts_with("admission 1 unset\nbusiness 1 unset\n"),
        "{calls}\n{stderr}"
    );
    assert_eq!(calls.matches("admission").count(), 1, "{calls}");
    assert!(Fixture::admission_lines(&stderr).is_empty(), "{stderr}");
    for reply in ["Okay", "NO"] {
        let refused = Fixture::new("claude-fable-5-1", &answer(reply));
        let output = refused.run(&["review", "run", "--file", "review.json"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{reply}: {stderr}");
        assert_eq!(refused.first_stdin(), PROBE_INPUT, "{reply}: {stderr}");
        let calls = refused.calls();
        assert!(!calls.is_empty(), "{reply}: {stderr}");
        assert!(
            calls.lines().all(|line| line == "admission 1 unset"),
            "{reply}: {calls}\n{stderr}"
        );
    }
}
