use crate::common;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Output;

struct Setup {
    root: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    plan: Value,
}

fn document(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)));
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../schemas/agent-bench-native-v2.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&value), "{value}");
    value
}

fn plan(binary: &Path, libraries: Option<&Path>, native_fixture: Option<(&Path, &str)>) -> Setup {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let state = root.path().join("state");
    std::fs::create_dir(&repo).unwrap();
    let mut command = common::af();
    command
        .args(["agent-bench", "plan", "--smoke", "--json", "--repo"])
        .arg(&repo)
        .arg("--state")
        .arg(&state)
        .arg("--rust-analyzer")
        .arg(binary)
        .arg("--rust-analyzer-sha256")
        .arg(sha256(&std::fs::read(binary).unwrap()));
    if let Some(libraries) = libraries {
        command.arg("--rust-analyzer-library-dir").arg(libraries);
    }
    if let Some((codex, endpoint)) = native_fixture {
        command.arg("--native-fixture-codex").arg(codex);
        command.arg("--fixture-endpoint").arg(endpoint);
    }
    let plan = document(command.output().unwrap());
    let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
    let policy = cas
        .get_artifact(plan["schedule_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(policy.artifact_type, "af/AgentBenchPolicy@2");
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../schemas/agent-bench-native-v2.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&policy.payload));
    assert_eq!(
        policy.payload["analyzer_libraries"].is_array(),
        libraries.is_some()
    );
    let mut retired = policy.payload;
    retired
        .as_object_mut()
        .unwrap()
        .remove("analyzer_libraries");
    assert!(!validator.is_valid(&retired));
    Setup {
        root,
        repo,
        state,
        plan,
    }
}

impl Setup {
    fn id(&self) -> &str {
        self.plan["task_id"].as_str().unwrap()
    }

    fn report(&self) -> Output {
        common::af()
            .args(["agent-bench", "report", self.id(), "--json", "--repo"])
            .arg(&self.repo)
            .arg("--state")
            .arg(&self.state)
            .output()
            .unwrap()
    }

    fn run(&self, confirm: bool) -> Output {
        let mut command = common::af();
        let temporary = self.root.path().join("tmp");
        std::fs::create_dir_all(&temporary).unwrap();
        command
            .env("TMPDIR", temporary)
            .args(["task", "run", self.id(), "--repo"])
            .arg(&self.repo)
            .arg("--state")
            .arg(&self.state);
        if confirm {
            command
                .arg("--confirm-plan")
                .arg(self.plan["plan_id"].as_str().unwrap());
        }
        command.output().unwrap()
    }
}

#[test]
fn installed_cli_freezes_pairs_and_replays_unstarted_observations_offline() {
    let fixture = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(fixture.path(), b"synthetic unavailable analyzer, never run").unwrap();
    let bench = plan(fixture.path(), None, None);
    let refusal = bench.run(false);
    assert!(!refusal.status.success());
    assert!(String::from_utf8_lossy(&refusal.stderr).contains("--confirm-plan"));
    let first = bench.report();
    let value = document(bench.report());
    assert!(first.status.success());
    assert_eq!(first.stdout, bench.report().stdout);
    assert_eq!(value["schedule_id"], bench.plan["schedule_id"]);
    assert_eq!(value["inference"], "synthetic_fixture");
    assert_eq!(value["grader"], "synthetic_anchor_oracle");
    let slots = value["slots"].as_array().unwrap();
    assert_eq!(slots.len(), 6);
    assert_eq!(value["pairs"].as_array().unwrap().len(), 3);
    for pair in slots.chunks_exact(2) {
        assert_eq!(pair[0]["case"], pair[1]["case"]);
        assert_ne!(pair[0]["arm"], pair[1]["arm"]);
        for slot in pair {
            assert_eq!(slot["status"], "unstarted");
            assert_eq!(slot["attempts"], serde_json::json!([]));
            assert!(slot["inclusive_native_tokens"].is_null());
            assert!(slot["elapsed_ns"].is_null());
            assert!(slot["receipt"].is_null());
            assert!(slot["grade"].is_null());
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        af_contracts::hex::encode(&Sha256::digest(bytes))
    )
}

#[test]
fn external_plan_requires_exact_packages_and_refuses_an_oracle_committed_as_source() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let state = root.path().join("state");
    let binary = root.path().join("analyzer");
    let cases = root.path().join("cases.json");
    let oracle = root.path().join("oracle.json");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .current_dir(&repo)
            .env_clear()
            .env("PATH", af_process::sanitized_path())
            .env("HOME", root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    let source = b"pub fn example() {}\n";
    std::fs::write(repo.join("lib.rs"), source).unwrap();
    git(&["add", "--", "lib.rs"]);
    let commit = || {
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "synthetic source",
        ]);
        git(&["rev-parse", "HEAD"])
    };
    let revision = commit();
    let manifest = af_source_git::Manifest::new(vec![af_source_git::Entry {
        path: "lib.rs".into(),
        kind: af_source_git::EntryKind::File,
        content: af_source_git::digest_bytes(source),
        size: source.len() as u64,
    }])
    .unwrap();
    let digest = manifest.content_digest();
    let cases_bytes = serde_json::to_vec(&serde_json::json!({
        "schema":"af.agent-bench-native-cases/1", "source_digest":digest, "repetitions":2,
        "project":[{"root":"lib.rs","edition":"2021","dependencies":[]}],
        "cases":[{"id":"declaration", "question":"Where is example declared?", "literal":"example",
            "position":{"path":"lib.rs","line":0,"character":8},"operation":"definition"}],
    }))
    .unwrap();
    let oracle_bytes = serde_json::to_vec(&serde_json::json!({
        "schema":"af.agent-bench-oracle/1", "source_digest":digest,
        "expected":{"declaration":[{"path":"lib.rs","line":1}]},
    }))
    .unwrap();
    std::fs::write(&binary, b"synthetic analyzer bytes for planning").unwrap();
    std::fs::write(&cases, &cases_bytes).unwrap();
    std::fs::write(&oracle, &oracle_bytes).unwrap();
    let plan = |revision: &str, oracle_pin: &str| {
        common::af()
            .args(["agent-bench", "plan", "--json", "--repo"])
            .arg(&repo)
            .arg("--state")
            .arg(&state)
            .arg("--source-revision")
            .arg(revision)
            .arg("--cases")
            .arg(&cases)
            .arg("--cases-sha256")
            .arg(sha256(&cases_bytes))
            .arg("--oracle")
            .arg(&oracle)
            .arg("--oracle-sha256")
            .arg(oracle_pin)
            .arg("--rust-analyzer")
            .arg(&binary)
            .arg("--rust-analyzer-sha256")
            .arg(sha256(b"synthetic analyzer bytes for planning"))
            .output()
            .unwrap()
    };
    let wrong = plan(&revision, &sha256(b"different"));
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("Pin differs"));
    let plan_value = document(plan(&revision, &sha256(&oracle_bytes)));
    assert_eq!(plan_value["slots"], 4);
    std::fs::write(repo.join("oracle.json"), &oracle_bytes).unwrap();
    git(&["add", "--", "oracle.json"]);
    let leaked = plan(&commit(), &sha256(&oracle_bytes));
    assert!(!leaked.status.success());
    assert!(
        String::from_utf8_lossy(&leaked.stderr).contains("present in the proposed source Manifest")
    );
}

#[test]
#[ignore = "requires the pinned Docker image and Linux analyzer plus its pinned libraries"]
fn installed_cli_runs_confined_rust_analyzer_for_treatment_only() {
    let binary = PathBuf::from(
        std::env::var_os("AF_TEST_RUST_ANALYZER_1_88").expect("supply rust-analyzer 1.88.0"),
    );
    let libraries = PathBuf::from(
        std::env::var_os("AF_TEST_RUST_ANALYZER_LIB_DIR")
            .expect("supply the pinned runtime libraries"),
    );
    assert_eq!(
        sha256(&std::fs::read(&binary).unwrap()),
        af_sandbox::navigation::semantic::LINUX_ANALYZER_SHA256
    );
    let bench = plan(&binary, Some(&libraries), None);
    // Run must use the captured copy, even when the input checkout contains a different source.
    std::fs::write(
        bench.repo.join("private-oracle.json"),
        "<oracle outside SourceTree>",
    )
    .unwrap();
    let run = bench.run(true);
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let report = document(bench.report());
    assert!(!report.to_string().contains("<oracle outside SourceTree>"));
    let slots = report["slots"].as_array().unwrap();
    for slot in slots {
        assert_eq!(slot["status"], "ok", "{slot}");
        assert_eq!(slot["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(slot["af_charged_tokens"], "0");
        assert!(slot["inclusive_native_tokens"].is_null());
        assert!(slot["elapsed_ns"].as_u64().unwrap() > 0);
        let receipt = &slot["receipt"];
        assert_eq!(receipt["source_digest"], report["source_digest"]);
        let listing = af_sandbox::navigation::Response::ListPaths {
            paths: vec!["app/lib.rs".into(), "library/lib.rs".into()],
            next_offset: None,
        };
        assert_eq!(
            receipt["trace"][0]["response"],
            af_sandbox::navigation::Broker::response_identity(&listing)
        );
        assert_eq!(
            &receipt["tools"].as_array().unwrap()[..3],
            &["af_list_paths", "af_read_range", "af_search"]
        );
        if slot["arm"] == "treatment" {
            assert_eq!(slot["grade"], "correct", "{slot}");
            let expected = match slot["case"].as_str().unwrap() {
                "definition" => serde_json::json!([{"path":"library/lib.rs","line":4}]),
                "references" => {
                    serde_json::json!([{"path":"app/lib.rs","line":2},{"path":"library/lib.rs","line":4}])
                }
                "implementation" => serde_json::json!([{"path":"library/lib.rs","line":3}]),
                other => panic!("unknown smoke case {other}"),
            };
            assert_eq!(receipt["answer"]["anchors"], expected);
            assert_eq!(receipt["trace"].as_array().unwrap().len(), 4);
        } else {
            assert_eq!(receipt["tools"].as_array().unwrap().len(), 3);
            assert_eq!(receipt["trace"].as_array().unwrap().len(), 3);
        }
    }
    // The same checked Task prefix produces exactly the same offline report.
    assert_eq!(bench.report().stdout, bench.report().stdout);
}

#[test]
#[ignore = "requires the pinned Linux Codex and analyzer runtime; planning is offline"]
fn installed_native_fixture_plan_replays_before_any_endpoint_contact() {
    let input = |name| PathBuf::from(std::env::var_os(name).expect(name));
    let codex = input("AF_NAVIGATION_CODEX");
    let analyzer = input("AF_TEST_RUST_ANALYZER_1_88");
    let libraries = input("AF_TEST_RUST_ANALYZER_LIB_DIR");
    let bench = plan(&analyzer, Some(&libraries), Some((&codex, "127.0.0.1:1")));
    let report = document(bench.report());
    assert_eq!(report["inference"], "native_codex_dummy_endpoint");
    assert_eq!(report["slots"].as_array().unwrap().len(), 6);
    for slot in report["slots"].as_array().unwrap() {
        assert_eq!(slot["status"], "unstarted");
        assert!(slot["inclusive_native_tokens"].is_null());
    }
    assert_eq!(bench.report().stdout, bench.report().stdout);
}

#[test]
#[ignore = "requires the pinned Linux Codex and analyzer runtime; binds a dummy loopback endpoint"]
fn installed_native_fixture_cli_replays_both_arms_and_dummy_usage() {
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let input = |name| PathBuf::from(std::env::var_os(name).expect(name));
    let codex = input("AF_NAVIGATION_CODEX");
    let analyzer = input("AF_TEST_RUST_ANALYZER_1_88");
    let libraries = input("AF_TEST_RUST_ANALYZER_LIB_DIR");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let bench = plan(&analyzer, Some(&libraries), Some((&codex, &endpoint)));
    let initial = document(bench.report());
    assert_eq!(initial["inference"], "native_codex_dummy_endpoint");
    let stopped = AtomicBool::new(false);
    let (run, registrations) = std::thread::scope(|scope| {
        let serve = scope.spawn(|| {
            let mut registrations = Vec::new();
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") && header.len() < 16_384 {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                }
                let header = String::from_utf8(header).unwrap();
                assert!(header.contains("Authorization: Bearer dummy-native-access\r\n"));
                let bytes = header.lines().find_map(|line| {
                    line.to_ascii_lowercase().strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap())
                }).unwrap();
                assert!(bytes <= 4 * 1024 * 1024);
                let mut body = vec![0; bytes];
                stream.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                for name in ["af_list_paths", "af_read_range", "af_search"] {
                    assert!(body.contains(name), "{name} missing from registered tools");
                }
                registrations.push(body.contains("af_definition") && body.contains("af_references") && body.contains("af_implementation"));
                let item = json!({"id":"msg-fixture","type":"message","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":r#"{"anchors":[],"truncated":false}"#,"annotations":[]}]});
                let body = format!("data: {}\n\ndata: {}\n\ndata: {}\n\n",
                    json!({"type":"response.output_item.added","output_index":0,"item":item}),
                    json!({"type":"response.output_item.done","output_index":0,"item":item}),
                    json!({"type":"response.completed","response":{"id":"resp-fixture","status":"completed","output":[],
                        "usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":20},"output_tokens":10,"total_tokens":110}}}));
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            registrations
        });
        let run = bench.run(true);
        stopped.store(true, Ordering::Release);
        (run, serve.join().unwrap())
    });
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let report = document(bench.report());
    let slots = report["slots"].as_array().unwrap();
    assert_eq!(registrations.len(), slots.len());
    for (slot, semantic_registered) in slots.iter().zip(registrations) {
        assert_eq!(semantic_registered, slot["arm"] == "treatment");
        assert_eq!(slot["status"], "ok", "{slot}");
        assert_eq!(slot["inclusive_native_tokens"], 110);
        assert_eq!(slot["af_charged_tokens"], "0");
        let call = &slot["receipt"]["native_calls"][0];
        let evidence = slot["attempts"][0]["raw_artifact_ids"].as_array().unwrap();
        assert!(evidence.contains(&call["intent"]) && evidence.contains(&call["raw"]));
        assert_eq!(slot["grade"], "incorrect");
    }
    assert_eq!(bench.report().stdout, bench.report().stdout);
}

struct ExternalInputs {
    root: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    revision: String,
    bundle_path: PathBuf,
    schedule_path: PathBuf,
    graph_path: PathBuf,
    bundle_pin: String,
    schedule_pin: String,
    graph_pin: String,
    bundle: Value,
    schedule: Value,
    graph: Value,
}

fn write_external_input(path: &Path, value: &Value) -> String {
    let bytes = serde_json::to_vec_pretty(value).unwrap();
    std::fs::write(path, &bytes).unwrap();
    sha256(&bytes)
}

impl ExternalInputs {
    fn new() -> Self {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let state = root.path().join("state");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let result = std::process::Command::new("git")
                .current_dir(&repo)
                .env_clear()
                .env("PATH", af_process::sanitized_path())
                .env("HOME", root.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            String::from_utf8(result.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("lib.rs"), "pub fn example() {}\n").unwrap();
        std::fs::write(
            repo.join("client.rs"),
            "pub fn client() { lib::example(); }\n",
        )
        .unwrap();
        git(&["add", "--", "lib.rs", "client.rs"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "synthetic source",
        ]);
        let revision = git(&["rev-parse", "HEAD"]);

        let instruction = "Use the registered tools to answer. Preserve Unicode: β.\n";
        let cases: Vec<_> = (1..=12).map(|i| json!({
        "id":format!("N{i:02}"), "prompt":format!("Complete question {i}?\nSecond line (λ).  ")
    })).collect();
        let bundle = json!({
            "schema":"af.agent-bench-subject/1", "common_instruction":instruction, "cases":cases
        });
        // Deliberately interleave repetitions and reverse case order. Neither sort order is authority.
        let blocks: Vec<_> = (1..=12)
            .rev()
            .flat_map(|i| {
                (1..=3).map(move |r| json!({
        "pair_id":format!("R{r}-N{i:02}"), "case_id":format!("N{i:02}"), "repetition":r,
        "order":if (i + r) % 2 == 1 { ["V1", "V2"] } else { ["V2", "V1"] }
    }))
            })
            .collect();
        let schedule = json!({
            "schema":"af.agent-bench-pair-schedule/1", "status":"frozen", "seed":123,
            "ordering":"listed", "blocks":blocks
        });
        let graph = json!([
            {"root":"lib.rs", "edition":"2021", "dependencies":[]},
            {"root":"client.rs", "edition":"2024", "dependencies":[{"name":"lib","index":0}]}
        ]);
        let bundle_path = root.path().join("bundle.json");
        let schedule_path = root.path().join("schedule.json");
        let graph_path = root.path().join("graph.json");
        let bundle_pin = write_external_input(&bundle_path, &bundle);
        let schedule_pin = write_external_input(&schedule_path, &schedule);
        let graph_pin = write_external_input(&graph_path, &graph);
        Self {
            root,
            repo,
            state,
            revision,
            bundle_path,
            schedule_path,
            graph_path,
            bundle_pin,
            schedule_pin,
            graph_pin,
            bundle,
            schedule,
            graph,
        }
    }

    fn plan(
        &self,
        phase: &str,
        revision: &str,
        schedule_pin: &str,
        bundle_pin: &str,
    ) -> std::process::Command {
        let mut command = common::af();
        command
            .args(["agent-bench", "plan", "--json", "--repo"])
            .arg(&self.repo)
            .arg("--state")
            .arg(&self.state)
            .args(["--phase", phase, "--source-revision", revision])
            .arg("--bundle")
            .arg(&self.bundle_path)
            .args(["--bundle-sha256", bundle_pin])
            .arg("--schedule")
            .arg(&self.schedule_path)
            .args(["--schedule-sha256", schedule_pin])
            .arg("--project-graph")
            .arg(&self.graph_path)
            .args(["--project-graph-sha256", &self.graph_pin]);
        command
    }

    fn fixture_plan(&self, endpoint: &str) -> Output {
        self.fixture_phase("smoke", endpoint, None)
    }

    fn fixture_phase(&self, phase: &str, endpoint: &str, predecessor: Option<&str>) -> Output {
        use serde_json::json;
        let input = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let declaration = self.root.path().join("fixture-transport.json");
        let pin = write_external_input(
            &declaration,
            &json!({
                "executable": input("AF_NAVIGATION_CODEX"),
                "executable_sha256": af_runner_codex::navigation::LINUX_X86_64_SHA256,
                "backend": {"kind":"loopback_fixture", "endpoint":endpoint}
            }),
        );
        let mut command = self.plan(phase, &self.revision, &self.schedule_pin, &self.bundle_pin);
        if let Some(id) = predecessor {
            command.args(["--predecessor", id]);
        }
        command
            .arg("--external-transport")
            .arg(&declaration)
            .args(["--external-transport-sha256", &pin, "--rust-analyzer"])
            .arg(input("AF_TEST_RUST_ANALYZER_1_88"))
            .args([
                "--rust-analyzer-sha256",
                af_sandbox::navigation::semantic::LINUX_ANALYZER_SHA256,
                "--rust-analyzer-library-dir",
            ])
            .arg(input("AF_TEST_RUST_ANALYZER_LIB_DIR"))
            .output()
            .unwrap()
    }

    fn report(&self, id: &str) -> Output {
        common::af()
            .args(["agent-bench", "report", id, "--json", "--repo"])
            .arg(&self.repo)
            .arg("--state")
            .arg(&self.state)
            .output()
            .unwrap()
    }

    fn status(&self, id: &str) -> Value {
        let output = common::af()
            .args(["task", "status", id, "--json", "--repo"])
            .arg(&self.repo)
            .arg("--state")
            .arg(&self.state)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn refuses(output: Output, error: &str) {
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains(error), "{diagnostic}");
}

#[test]
fn installed_external_evaluator_checks_the_full_case_set_without_retaining_private_material() {
    use serde_json::json;

    let inputs = ExternalInputs::new();
    let output = inputs
        .plan(
            "smoke",
            &inputs.revision,
            &inputs.schedule_pin,
            &inputs.bundle_pin,
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    let task_id = plan["task_id"].as_str().unwrap();
    let cas = af_store::Cas::open_existing(inputs.state.join("cas")).unwrap();
    let store = af_store::EventStore::open_read_only(inputs.state.join("events.sqlite")).unwrap();
    let task = store.task_projection(&cas, task_id).unwrap().unwrap();
    let original_report = inputs.report(task_id);
    assert!(
        original_report.status.success(),
        "{}",
        String::from_utf8_lossy(&original_report.stderr)
    );
    std::fs::remove_file(&inputs.bundle_path).unwrap();
    let private_text = "private synthetic oracle β";
    let cases: Vec<_> = (1..=12)
        .map(|i| {
            json!({
                "id": format!("N{i:02}"), "family": "synthetic",
                "abstention": i == 12, "oracle_text_verbatim": private_text,
            })
        })
        .collect();
    let evaluator = json!({
        "schema": "af.agent-bench-evaluator/1", "status": "synthetic",
        "source_snapshot": task.revision.inputs["source"].snapshot_id,
        "report_sha256": &sha256(b"synthetic backing report")[7..],
        "report_artifact": sha256(b"synthetic-report"),
        "verification_artifact": sha256(b"synthetic-verification"),
        "rubric_text_verbatim": private_text,
        "cases": cases,
    });
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../schemas/agent-bench-evaluator-v1.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&evaluator));

    let path = inputs.root.path().join("evaluator.json");
    let run = |file: &Path, digest: &str| {
        let output = common::af()
            .args(["agent-bench", "check-evaluator", task_id, "--repo"])
            .arg(&inputs.repo)
            .arg("--state")
            .arg(&inputs.state)
            .arg("--evaluator")
            .arg(file)
            .args(["--evaluator-sha256", digest])
            .output()
            .unwrap();
        for stream in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(stream).contains(private_text));
        }
        output
    };
    let digest = write_external_input(&path, &evaluator);
    let output = run(&path, &digest);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let blob = af_store::canonical::blob_content_id(&std::fs::read(&path).unwrap());
    assert!(cas.get(&blob).is_err());
    refuses(run(&path, &sha256(b"different bytes")), "digest differs");
    let checkout_path = inputs.repo.join("evaluator.json");
    std::fs::copy(&path, &checkout_path).unwrap();
    refuses(run(&checkout_path, &digest), "outside the checkout");

    let mut changed = evaluator.clone();
    changed["source_snapshot"] = json!(sha256(b"another source"));
    refuses(
        run(&path, &write_external_input(&path, &changed)),
        "source provenance",
    );
    changed = evaluator.clone();
    changed["cases"].as_array_mut().unwrap().remove(0);
    assert!(!validator.is_valid(&changed));
    refuses(
        run(&path, &write_external_input(&path, &changed)),
        "every captured subject case",
    );
    changed = evaluator.clone();
    changed["cases"][0]["id"] = json!("N12");
    refuses(
        run(&path, &write_external_input(&path, &changed)),
        "repeated case",
    );
    changed = evaluator.clone();
    changed["cases"][0]["id"] = json!("N13");
    assert!(!validator.is_valid(&changed));
    refuses(
        run(&path, &write_external_input(&path, &changed)),
        "foreign",
    );
    changed = evaluator.clone();
    changed["cases"][0]["unexpected"] = json!(private_text);
    assert!(!validator.is_valid(&changed));
    refuses(
        run(&path, &write_external_input(&path, &changed)),
        "Invalid private evaluator JSON",
    );
    assert_eq!(inputs.report(task_id).stdout, original_report.stdout);
}

#[test]
fn external_native_phases_freeze_and_replay_the_prompt_only_input() {
    use serde_json::json;

    let inputs = ExternalInputs::new();
    let ExternalInputs {
        repo,
        state,
        revision,
        bundle_path,
        schedule_path,
        graph_path,
        bundle_pin,
        schedule_pin,
        graph_pin,
        bundle,
        schedule,
        graph,
        ..
    } = &inputs;
    let instruction = &bundle["common_instruction"];
    let cases = bundle["cases"].as_array().unwrap();
    let blocks = schedule["blocks"].as_array().unwrap();
    let write = write_external_input;
    let command = |phase: &str, revision: &str, schedule_pin: &str, bundle_pin: &str| {
        inputs.plan(phase, revision, schedule_pin, bundle_pin)
    };
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../schemas/agent-bench-external-v1.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(bundle));
    assert!(validator.is_valid(schedule));
    let checked = |output: Output| -> Value {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value = serde_json::from_slice(&output.stdout).unwrap();
        if let Err(error) = validator.validate(&value) {
            panic!("{error}: {value}");
        }
        value
    };
    let mut plans = Vec::new();
    for (phase, count) in [("smoke", 4), ("aa", 72), ("ab", 72)] {
        let plan = checked(
            command(phase, revision, schedule_pin, bundle_pin)
                .output()
                .unwrap(),
        );
        assert_eq!(plan["slots"], count);
        assert_eq!(plan["execution"], "unavailable");
        assert_eq!(
            plan["prerequisite"],
            if phase == "smoke" { "none" } else { "pending" }
        );
        plans.push(plan);
    }
    let smoke_id = plans[0]["task_id"].as_str().unwrap();
    refuses(
        command("aa", revision, schedule_pin, bundle_pin)
            .args(["--predecessor", smoke_id])
            .output()
            .unwrap(),
        "no accepted result",
    );
    refuses(
        command("ab", revision, &sha256(b"wrong"), bundle_pin)
            .output()
            .unwrap(),
        "Pin differs",
    );
    refuses(
        command("aa", "HEAD", schedule_pin, bundle_pin)
            .output()
            .unwrap(),
        "exact Git commit ID",
    );
    refuses(
        command("smoke", revision, schedule_pin, bundle_pin)
            .args(["--fixture-endpoint", "127.0.0.1:9"])
            .output()
            .unwrap(),
        "--native-fixture-codex",
    );

    let mut invalid = schedule.clone();
    invalid["blocks"][0]["pair_id"] = json!("../bad");
    let pin = write(schedule_path, &invalid);
    refuses(
        command("ab", revision, &pin, bundle_pin).output().unwrap(),
        "Invalid external pair",
    );
    invalid = schedule.clone();
    let repeated_id = invalid["blocks"][1]["pair_id"].clone();
    invalid["blocks"][0]["pair_id"] = repeated_id;
    let pin = write(schedule_path, &invalid);
    refuses(
        command("ab", revision, &pin, bundle_pin).output().unwrap(),
        "Invalid external pair",
    );
    invalid = schedule.clone();
    invalid["blocks"][0]["order"] = json!(["V1", "V1"]);
    let pin = write(schedule_path, &invalid);
    refuses(
        command("ab", revision, &pin, bundle_pin).output().unwrap(),
        "Invalid external pair",
    );
    let original_order = schedule["blocks"][0]["order"].as_array().unwrap();
    invalid["blocks"][0]["order"] = json!([original_order[1], original_order[0]]);
    let pin = write(schedule_path, &invalid);
    refuses(
        command("ab", revision, &pin, bundle_pin).output().unwrap(),
        "six V1-first",
    );

    write(schedule_path, schedule);
    let mut invalid_case = bundle.clone();
    invalid_case["cases"][0]["id"] = json!("N13");
    let pin = write(bundle_path, &invalid_case);
    refuses(
        command("aa", revision, schedule_pin, &pin)
            .output()
            .unwrap(),
        "case IDs must be N01..N12",
    );
    for field in ["literal", "position", "operation", "answer", "oracle"] {
        let mut invalid = bundle.clone();
        invalid["cases"][0][field] = json!("private hint");
        let pin = write(bundle_path, &invalid);
        refuses(
            command("aa", revision, schedule_pin, &pin)
                .output()
                .unwrap(),
            "unknown field",
        );
    }
    for path in [bundle_path, schedule_path, graph_path] {
        std::fs::remove_file(path).unwrap();
    }
    let cas = af_store::Cas::open_existing(state.join("cas")).unwrap();
    let store = af_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    for plan in plans {
        let id = plan["task_id"].as_str().unwrap();
        let plan_id = plan["plan_id"].as_str().unwrap();
        let projection = store.task_projection(&cas, id).unwrap().unwrap();
        assert!(projection.execution.is_none());
        assert!(projection.execution_epochs.is_empty());
        assert!(projection.revision.authority.data_destinations.is_empty());
        assert_eq!(projection.revision.limits.tokens, 0);
        let captured = cas.get_artifact(plan_id).unwrap();
        assert_eq!(captured.artifact_type, "af/AgentBenchExternalPlan@1");
        assert!(validator.is_valid(&captured.payload));
        for (key, digest, input) in [
            ("bundle", bundle_pin, bundle),
            ("schedule", schedule_pin, schedule),
            ("project_graph", graph_pin, graph),
        ] {
            let recorded = &captured.payload["pins"][key];
            assert_eq!(recorded["sha256"], *digest);
            let object_id = recorded["object_id"].as_str().unwrap();
            assert!(captured.input_artifacts.contains(&object_id.to_owned()));
            assert_eq!(
                cas.get(object_id).unwrap(),
                serde_json::to_vec_pretty(input).unwrap()
            );
        }

        let report = || {
            common::af()
                .args(["agent-bench", "report", id, "--json", "--repo"])
                .arg(repo)
                .arg("--state")
                .arg(state)
                .output()
                .unwrap()
        };
        let first = report();
        assert_eq!(first.stdout, report().stdout);
        let value = checked(first);
        assert_eq!(&value["common_instruction"], instruction);
        assert_eq!(&value["pins"]["source_revision"], revision.as_str());
        assert_eq!(value["pins"], captured.payload["pins"]);
        let slots = value["slots"].as_array().unwrap();
        assert_eq!(slots.len() as u64, plan["slots"].as_u64().unwrap());
        for (pair, block) in slots.chunks_exact(2).zip(blocks) {
            for (slot, variant) in pair.iter().zip(block["order"].as_array().unwrap()) {
                assert_eq!(slot["pair_id"], block["pair_id"]);
                assert_eq!(slot["case_id"], block["case_id"]);
                assert_eq!(slot["repetition"], block["repetition"]);
                assert_eq!(slot["variant"], *variant);
                let case = cases
                    .iter()
                    .find(|case| case["id"] == slot["case_id"])
                    .unwrap();
                assert_eq!(slot["question"], case["prompt"]);
                let tool_set = if plan["phase"] != "aa" && *variant == "V2" {
                    af_runner_codex::navigation::ToolSet::Semantic
                } else {
                    af_runner_codex::navigation::ToolSet::Text
                };
                assert_eq!(slot["tool_set_id"], tool_set.identity());
                let schemas = tool_set.schemas();
                let tools: Vec<_> = schemas
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tool| &tool["name"])
                    .collect();
                assert_eq!(slot["tools"], json!(tools));
                assert_eq!(slot["status"], "unstarted");
                assert_eq!(slot["attempts"], json!([]));
                assert!(slot["receipt"].is_null());
                assert!(slot["grade"].is_null());
                assert!(slot["inclusive_native_tokens"].is_null());
                assert!(slot["charged_tokens"].is_null());
            }
        }
        for (index, slot) in slots.iter().enumerate() {
            assert_eq!(slot["index"], index);
        }
        refuses(
            common::af()
                .args(["task", "run", id, "--confirm-plan", plan_id, "--repo"])
                .arg(repo)
                .arg("--state")
                .arg(state)
                .output()
                .unwrap(),
            "offline only",
        );
    }
}

fn external_document(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../schemas/agent-bench-external-v3.json"
    ))
    .unwrap();
    let mut raw: Value = serde_json::from_str(include_str!(
        "../../../../schemas/codex-navigation-call-v1.json"
    ))
    .unwrap();
    raw.as_object_mut().unwrap().remove("$schema");
    raw.as_object_mut().unwrap().remove("title");
    assert_eq!(schema["$defs"]["raw"], raw);
    if let Err(error) = jsonschema::validator_for(&schema).unwrap().validate(&value) {
        panic!("{error}: {value}");
    }
    value
}

#[test]
#[ignore = "requires the pinned Linux Codex and analyzer runtime; refused transport uses inert auth"]
fn installed_external_smoke_plan_replays_without_original_bindings() {
    let inputs = ExternalInputs::new();
    let plan = external_document(inputs.fixture_plan("127.0.0.1:1"));
    let id = plan["task_id"].as_str().unwrap();
    let cas = af_store::Cas::open_existing(inputs.state.join("cas")).unwrap();
    let store = af_store::EventStore::open_read_only(inputs.state.join("events.sqlite")).unwrap();
    let task = store.task_projection(&cas, id).unwrap().unwrap();
    let bindings = cas
        .get_artifact(&task.revision.inputs["bindings"].artifact_ids[0])
        .unwrap();
    assert_eq!(bindings.artifact_type, "af/AgentBenchExternalBindings@2");
    let declaration = std::fs::read(inputs.root.path().join("fixture-transport.json")).unwrap();
    let pin = &bindings.payload["declaration"];
    assert_eq!(pin["sha256"], sha256(&declaration));
    assert_eq!(
        cas.get(pin["object_id"].as_str().unwrap()).unwrap(),
        declaration
    );
    let report = external_document(inputs.report(id));
    assert_eq!(report["execution"], "loopback_fixture");
    assert_eq!(report["plan_id"], plan["plan_id"]);
    assert!(
        report["slots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["status"] == "unstarted")
    );
    for path in [
        &inputs.bundle_path,
        &inputs.schedule_path,
        &inputs.graph_path,
        &inputs.root.path().join("fixture-transport.json"),
    ] {
        std::fs::remove_file(path).unwrap();
    }
    let run = common::af()
        .args(["task", "run", id, "--repo"])
        .arg(&inputs.repo)
        .arg("--state")
        .arg(&inputs.state)
        .arg("--execute")
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("requires --confirm-plan"));
    let wrong = common::af()
        .args([
            "task",
            "run",
            id,
            "--confirm-plan",
            &sha256(b"another plan"),
            "--repo",
        ])
        .arg(&inputs.repo)
        .arg("--state")
        .arg(&inputs.state)
        .output()
        .unwrap();
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("Plan confirmation differs"));
    assert_eq!(external_document(inputs.report(id)), report);
    let run = common::af()
        .args([
            "task",
            "run",
            id,
            "--confirm-plan",
            plan["plan_id"].as_str().unwrap(),
            "--repo",
        ])
        .arg(&inputs.repo)
        .arg("--state")
        .arg(&inputs.state)
        .output()
        .unwrap();
    assert!(!run.status.success());
    let failed = external_document(inputs.report(id));
    let slots = failed["slots"].as_array().unwrap();
    assert_eq!(
        slots[0]["status"],
        "failed",
        "{}
{failed}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(slots[0]["attempts"][0]["started"], true);
    assert_eq!(slots[0]["charged_tokens"], "0");
    assert!(slots[0]["inclusive_native_tokens"].is_null());
    assert!(slots[1..].iter().all(|slot| slot["status"] == "unstarted"));
    assert_eq!(failed, external_document(inputs.report(id)));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExternalResponse {
    ToolThenExplanation,
    ForbiddenTool,
    MissingUsage,
    InclusiveStop,
    SubjectOvershoot,
    HttpError,
    Interrupted,
}

#[test]
#[ignore = "requires pinned Linux Codex, the analyzer runtime, Docker, and IPv4 loopback"]
fn installed_external_smoke_uses_two_turns_and_settles_offline() {
    for scenario in [
        ExternalResponse::ToolThenExplanation,
        ExternalResponse::ForbiddenTool,
        ExternalResponse::MissingUsage,
        ExternalResponse::InclusiveStop,
        ExternalResponse::SubjectOvershoot,
        ExternalResponse::HttpError,
        ExternalResponse::Interrupted,
    ] {
        external_fixture_scenario(scenario);
    }
}

fn external_fixture_scenario(scenario: ExternalResponse) {
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let inputs = ExternalInputs::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let plan = external_document(inputs.fixture_plan(&listener.local_addr().unwrap().to_string()));
    let id = plan["task_id"].as_str().unwrap();
    let started = external_document(inputs.report(id));
    let first_variant = started["slots"][0]["variant"].as_str().unwrap();
    let run_id = af_store::store::task::task_run_id(id).unwrap();
    let stopped = AtomicBool::new(false);
    let (interrupt, interrupted) = std::sync::mpsc::sync_channel(1);
    let (output, sends) = std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let store = af_store::EventStore::open_read_only(inputs.state.join("events.sqlite")).unwrap();
            let cas = af_store::Cas::open_existing(inputs.state.join("cas")).unwrap();
            let mut sends = 0;
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") && header.len() < 16_384 {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                }
                let header = String::from_utf8(header).unwrap();
                // The fixture accepts only the native transport's inert test auth.
                assert!(header.contains("Authorization: Bearer dummy-native-access\r\n"));
                let length = header.lines().find_map(|line| {
                    line.to_ascii_lowercase().strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap())
                }).unwrap();
                assert!(length <= 4 * 1024 * 1024);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).unwrap();
                let request = String::from_utf8(bytes).unwrap();
                let task = store.task_projection(&cas, id).unwrap().unwrap();
                let active = task.execution.unwrap().attempt_accounting().into_iter()
                    .find(|a| a.started && a.result.is_none()).expect("native Attempt started");
                let calls = store.task_attempt_sends(&run_id, &active.attempt_id).unwrap();
                let second = request.contains("function_call_output");
                assert_eq!(calls.len(), if second { 2 } else { 1 });
                assert!(calls.last().unwrap().1.is_none());
                if second {
                    let raw = cas.get_json(calls[0].1.as_deref().unwrap()).unwrap();
                    assert_eq!(raw["usage"]["reconciled"]["inclusive"], 110);
                    assert!(request.contains("lib.rs"), "missing broker response");
                }
                assert!(!request.contains("R1-N12") && !request.contains("R2-N12"));
                if scenario == ExternalResponse::Interrupted {
                    interrupt.send(()).unwrap();
                    break;
                }

                let semantic = request.contains("af_definition");
                let slot = &started["slots"][sends / 2];
                assert_eq!(semantic, slot["variant"] == "V2");
                let tool = if semantic || scenario == ExternalResponse::ForbiddenTool {
                    "af_definition"
                } else {
                    "af_list_paths"
                };
                let item = if second {
                    json!({"id":"msg-fixture","type":"message","role":"assistant","status":"completed",
                        "content":[{"type":"output_text","text":"The call resolves to lib.rs:1. I abstain on other callers.","annotations":[]}]})
                } else {
                    let args = if tool == "af_definition" {
                        json!({"path":"client.rs","line":0,"character":23})
                    } else {
                        json!({"offset":0})
                    };
                    json!({"id":"fc-fixture","type":"function_call","call_id":"fixture-tool","name":tool,"arguments":args.to_string()})
                };
                let mut response = json!({"id":format!("resp-{sends}"),"status":"completed","output":[]});
                if scenario != ExternalResponse::MissingUsage {
                    let input = match scenario {
                        ExternalResponse::InclusiveStop => 99_990,
                        ExternalResponse::SubjectOvershoot => 99_991,
                        _ => 100,
                    };
                    response["usage"] = json!({"input_tokens":input,"input_tokens_details":{"cached_tokens":20},
                        "output_tokens":10,"total_tokens":input + 10});
                }
                let body = format!("data: {}\n\ndata: {}\n\ndata: {}\n\n",
                    json!({"type":"response.output_item.added","output_index":0,"item":item}),
                    json!({"type":"response.output_item.done","output_index":0,"item":item}),
                    json!({"type":"response.completed","response":response}));
                let status = if scenario == ExternalResponse::HttpError { "503 Unavailable" } else { "200 OK" };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                sends += 1;
            }
            sends
        });
        let mut command = common::af();
        command
            .args([
                "task",
                "run",
                id,
                "--confirm-plan",
                plan["plan_id"].as_str().unwrap(),
                "--json",
                "--repo",
            ])
            .arg(&inputs.repo)
            .arg("--state")
            .arg(&inputs.state);
        let output = if scenario == ExternalResponse::Interrupted {
            let mut child = command
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let signal = interrupted.recv_timeout(Duration::from_secs(45));
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            assert!(
                signal.is_ok(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        } else {
            command.output().unwrap()
        };
        stopped.store(true, Ordering::Release);
        (output, server.join().unwrap())
    });
    let stderr = String::from_utf8_lossy(&output.stderr);
    let report = external_document(inputs.report(id));
    assert_eq!(report, external_document(inputs.report(id)));
    assert!(!report.to_string().contains("dummy-native-access"));
    let slots = report["slots"].as_array().unwrap();
    if scenario == ExternalResponse::ToolThenExplanation {
        assert!(output.status.success(), "{stderr}");
        assert_eq!(inputs.status(id)["state"]["outcome"], "verified");
        assert_eq!(sends, 8, "{stderr}");
        for slot in slots {
            assert_eq!(
                slot["status"], "collected",
                "{slot}
{stderr}"
            );
            assert_eq!(slot["inclusive_native_tokens"], 220);
            assert_eq!(slot["charged_tokens"], "0");
            assert_eq!(
                slot["receipt"]["answer"],
                "The call resolves to lib.rs:1. I abstain on other callers."
            );
            assert!(slot["grade"].is_null());
        }
    } else {
        assert!(!output.status.success(), "{stderr}");
        assert_eq!(first_variant, "V1");
        let first = &slots[0];
        assert!(
            matches!(first["status"].as_str(), Some("failed" | "missing")),
            "{first}
{stderr}"
        );
        assert!(slots[1..].iter().all(|slot| slot["status"] == "unstarted"));
        let call = &first["attempts"][0]["calls"][0];
        if scenario == ExternalResponse::Interrupted {
            assert!(call["raw"].is_null());
            assert!(first["charged_tokens"].is_null());
        } else {
            assert_eq!(sends, 1, "{stderr}");
            assert!(call["raw"].is_object());
            if scenario == ExternalResponse::InclusiveStop {
                assert_eq!(first["inclusive_native_tokens"], 100_000);
            }
            if scenario == ExternalResponse::SubjectOvershoot {
                assert_eq!(first["inclusive_native_tokens"], 100_001);
            }
        }
    }
}

#[test]
#[ignore = "requires pinned Linux Codex, the analyzer runtime, and IPv4 loopback"]
fn installed_external_grading_blinds_native_attempts_and_replays() {
    for scenario in [
        GradingFixture::Resolved,
        GradingFixture::Overshoot,
        GradingFixture::MissingUsage,
    ] {
        external_grading_fixture(scenario);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GradingFixture {
    Resolved,
    Overshoot,
    MissingUsage,
}

fn external_grading_fixture(scenario: GradingFixture) {
    use serde_json::json;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let checked = |output: Output| -> Value {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let inputs = ExternalInputs::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let subject = external_document(inputs.fixture_plan(&endpoint));
    let subject_id = subject["task_id"].as_str().unwrap();
    let grade_id = format!("{subject_id}-grading");
    let done = AtomicBool::new(false);
    let private = "Synthetic rubric unique to the grader";
    let answer =
        |slot| format!("Evidence for fixture answer [{slot}] is the caller in client.rs:1.");
    let oracle_marker = |id: &str| format!("private oracle marker [{id}]");
    let scheduled_cases: Vec<_> = inputs.schedule["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .flat_map(|block| std::iter::repeat_n(block["case_id"].as_str().unwrap(), 2))
        .map(|id| {
            inputs.bundle["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["id"] == id)
                .unwrap()
        })
        .collect();
    let grading_slots = [0, 0, 1, 1, 1, 2, 2, 3, 3];
    let run = |plan: &Value| {
        common::af()
            .args([
                "task",
                "run",
                plan["task_id"].as_str().unwrap(),
                "--confirm-plan",
                plan["plan_id"].as_str().unwrap(),
                "--repo",
            ])
            .arg(&inputs.repo)
            .arg("--state")
            .arg(&inputs.state)
            .output()
            .unwrap()
    };

    let (before, after, subject_sends, grading_sends) = std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let store = af_store::EventStore::open_read_only(inputs.state.join("events.sqlite")).unwrap();
            let cas = af_store::Cas::open_existing(inputs.state.join("cas")).unwrap();
            let mut subjects = 0;
            let mut graders = 0;
            let mut invocations = std::collections::BTreeSet::new();
            while !done.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(conn) => conn,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                    assert!(headers.len() < 16_384);
                }
                assert!(headers.contains("Authorization: Bearer dummy-native-access\r\n"));
                let size: usize = headers.lines().find_map(|line| line.to_ascii_lowercase()
                    .strip_prefix("content-length:").map(|n| n.trim().parse().unwrap())).unwrap();
                let mut body = vec![0; size];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                let wire = String::from_utf8(body).unwrap();
                let grading = request["model"] == "gpt-6.1-terra";
                let id = if grading { &grade_id } else { subject_id };
                let task = store.task_projection(&cas, id).unwrap().unwrap();
                let active = task.execution.unwrap().attempt_accounting().into_iter()
                    .find(|attempt| attempt.started && attempt.result.is_none()).unwrap();
                let sends = store.task_attempt_sends(&af_store::store::task::task_run_id(id).unwrap(), &active.attempt_id).unwrap();
                assert_eq!(sends.len(), 1);
                assert!(sends[0].1.is_none());
                let intent = cas.get_json(&sends[0].0).unwrap();
                assert!(invocations.insert(intent["bindings"]["task_attempt_invocation"].as_str().unwrap().to_owned()));
                let (text, bill) = if grading {
                    assert_eq!(request["reasoning"]["effort"], "high");
                    let slot = grading_slots[graders];
                    let expected = scheduled_cases[slot];
                    let expected_id = expected["id"].as_str().unwrap();
                    let projections: Vec<Value> = request["input"]
                        .as_array().unwrap().iter()
                        .filter(|item| item["role"] == "user")
                        .flat_map(|item| item["content"].as_array().unwrap())
                        .filter_map(|part| serde_json::from_str(part["text"].as_str()?).ok())
                        .collect();
                    assert_eq!(projections.len(), 1);
                    assert_eq!(projections[0]["question"], expected["prompt"]);
                    assert_eq!(projections[0]["answer"], answer(slot));
                    assert_eq!(projections[0]["rubric"], private);
                    assert_eq!(projections[0]["oracle"], json!({
                        "text": oracle_marker(expected_id), "abstention": false
                    }));
                    for case in inputs.bundle["cases"].as_array().unwrap() {
                        let id = case["id"].as_str().unwrap();
                        assert_eq!(wire.contains(&oracle_marker(id)), id == expected_id);
                    }
                    for other in 0..scheduled_cases.len() {
                        assert_eq!(wire.contains(&answer(other)), other == slot);
                    }
                    assert!(!wire.contains("fixture-grade-rationale"));
                    assert!(!wire.contains("af_definition"));
                    assert!(!wire.contains(&grade_id));
                    assert!(!wire.contains("tool_set_id") && !wire.contains("pair_id"));
                    let text = match graders {
                        0 | 1 | 2 | 4 => json!({"verdict":"correct", "rationale":"fixture-grade-rationale for equivalent evidence"}),
                        3 => json!({"verdict":"incorrect", "rationale":"fixture-grade-rationale"}),
                        _ => json!({"verdict":"disqualified", "rationale":"fixture-grade-rationale"}),
                    };
                    graders += 1;
                    (text.to_string(), 50)
                } else {
                    assert_eq!(request["model"], "gpt-6.1-sol");
                    assert_eq!(request["reasoning"]["effort"], "medium");
                    assert!(!wire.contains(private) && !wire.contains("private oracle marker ["));
                    let text = answer(subjects);
                    subjects += 1;
                    (text, 59_990)
                };
                let item = json!({"id":"msg-fixture","type":"message","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":text,"annotations":[]}]});
                let bill = if grading && scenario == GradingFixture::Overshoot && graders == 1 {
                    100_000
                } else {
                    bill
                };
                let mut response = json!({"id":format!("resp-{subjects}-{graders}"),"status":"completed","output":[],
                    "usage":{"input_tokens":bill,"input_tokens_details":{"cached_tokens":0},
                        "output_tokens":10,"total_tokens":bill + 10}});
                if grading && scenario == GradingFixture::MissingUsage && graders == 1 {
                    response.as_object_mut().unwrap().remove("usage");
                }
                let body = format!("data: {}\n\ndata: {}\n\ndata: {}\n\n",
                    json!({"type":"response.output_item.added","output_index":0,"item":item}),
                    json!({"type":"response.output_item.done","output_index":0,"item":item}),
                    json!({"type":"response.completed","response":response}));
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            (subjects, graders)
        });

        // Release the fixture even when a CLI assertion fails.
        let outcome = std::panic::catch_unwind(|| {
            let executed = run(&subject);
            assert!(executed.status.success(), "{executed:?}");
            let status = inputs.status(subject_id);
            assert_eq!(status["state"]["phase"], "finished");
            assert_eq!(status["state"]["outcome"], "verified");
            assert_eq!(status["remaining_tokens"], "400000");
            let before = external_document(inputs.report(subject_id));
            for row in before["slots"].as_array().unwrap() {
                assert_eq!(
                    row["status"],
                    "collected",
                    "{}",
                    String::from_utf8_lossy(&executed.stderr)
                );
                assert_eq!(row["inclusive_native_tokens"], 60_000);
            }
            let grade_plan = plan_fixture_grading(&inputs, subject_id, &endpoint, private);
            assert_eq!(grade_plan["task_id"], grade_id);
            let output = run(&grade_plan);
            let after = checked(inputs.report(&grade_id));
            assert_eq!(
                output.status.success(),
                scenario == GradingFixture::Resolved,
                "{output:?}"
            );
            assert_eq!(
                plan_fixture_grading(&inputs, subject_id, &endpoint, private),
                grade_plan
            );
            assert_eq!(run(&grade_plan).status.code(), output.status.code());
            assert_eq!(checked(inputs.report(&grade_id)), after);
            (before, after)
        });
        done.store(true, Ordering::Release);
        let (subjects, graders) = server.join().unwrap();
        let (before, after) = outcome.unwrap();
        (before, after, subjects, graders)
    });
    assert_eq!(subject_sends, 4);
    assert_eq!(before, external_document(inputs.report(subject_id)));
    assert_eq!(after, checked(inputs.report(&grade_id)));
    assert_eq!(after["mode"], "loopback_fixture");
    assert_eq!(after["subject_inclusive_tokens"], 240_000);
    assert!(!after.to_string().contains(private));
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../schemas/agent-bench-phase-evidence-v1.json"
    ))
    .unwrap();
    assert!(
        jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&after["phase_evidence"])
    );
    assert_eq!(after["phase_evidence"]["slots"], 4);
    assert_eq!(after["phase_evidence"]["collected"], 4);
    let status = inputs.status(&grade_id);
    assert_eq!(status["state"]["phase"], "finished");
    assert_eq!(status["remaining_tokens"], "1200000");
    if scenario != GradingFixture::Resolved {
        assert_eq!(grading_sends, 1);
        let slots = after["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 4);
        assert!(slots.iter().all(|slot| slot["decision"].is_null()));
        let stages: Vec<_> = slots
            .iter()
            .flat_map(|slot| slot["stages"].as_array().unwrap())
            .collect();
        assert_eq!(stages.len(), 12);
        assert_eq!(stages[0]["status"], "failed");
        assert_eq!(stages[0]["calls"].as_array().unwrap().len(), 1);
        let raw = &stages[0]["calls"][0]["raw"];
        assert!(raw.is_object());
        for stage in &stages[1..] {
            assert_eq!(stage["status"], "unavailable");
            assert_eq!(stage["calls"], json!([]));
            assert!(stage["judgment"].is_null());
            assert!(stage["inclusive_native_tokens"].is_null());
            assert_eq!(stage["af_charged_tokens"], "0");
        }
        if scenario == GradingFixture::Overshoot {
            assert_eq!(raw["failure"], "native output budget overshoot");
            assert_eq!(stages[0]["known_inclusive_tokens"], 100_010);
            assert_eq!(stages[0]["inclusive_native_tokens"], 100_010);
            assert_eq!(after["grading_inclusive_tokens"], 100_010);
            assert_eq!(after["known_grading_inclusive_tokens"], 100_010);
            assert_eq!(status["state"]["outcome"], "exhausted");
            assert_eq!(status["state"]["cause"], "budget");
        } else {
            assert!(raw["usage"]["reconciled"].is_null());
            assert!(stages[0]["inclusive_native_tokens"].is_null());
            assert!(after["grading_inclusive_tokens"].is_null());
            assert_eq!(after["known_grading_inclusive_tokens"], 0);
            assert_eq!(status["state"]["outcome"], "execution_failed");
        }
        assert_eq!(after["phase_evidence"]["resolved"], 0);
        assert_eq!(after["phase_evidence"]["ready"], false);
        refuses(
            inputs.fixture_phase("aa", &endpoint, Some(&grade_id)),
            "complete preceding phase evidence",
        );
        return;
    }
    assert_eq!(status["state"]["outcome"], "verified");
    assert!(status["state"]["cause"].is_null());
    assert_eq!(grading_sends, 9);
    assert_eq!(after["grading_inclusive_tokens"], 540);
    assert_eq!(after["slots"][0]["decision"], "correct");
    assert_eq!(after["slots"][0]["stages"][2]["status"], "not_required");
    assert_eq!(after["slots"][1]["stages"][2]["status"], "graded");
    assert_eq!(after["slots"][1]["decision"], "correct");
    assert_eq!(after["slots"][2]["decision"], "disqualified");
    assert_eq!(after["phase_evidence"]["resolved"], 4);
    assert_eq!(after["phase_evidence"]["ready"], true);
    assert_eq!(after["phase_evidence"]["overhead"]["tokens"], 540);
    let aa = external_document(inputs.fixture_phase("aa", &endpoint, Some(&grade_id)));
    assert_eq!(aa["slots"], 72);
    let aa_id = aa["task_id"].as_str().unwrap();
    let report = external_document(inputs.report(aa_id));
    let slots = report["slots"].as_array().unwrap();
    assert_eq!(slots.len(), 72);
    assert!(
        slots
            .iter()
            .all(|slot| slot["status"] == "unstarted"
                && slot["tools"].as_array().unwrap().len() == 3)
    );
    let duplicate = inputs.fixture_phase("aa", &endpoint, Some(&grade_id));
    if duplicate.status.success() {
        assert_eq!(external_document(duplicate), aa);
    }
    refuses(
        inputs.fixture_phase("ab", &endpoint, Some(aa_id)),
        "no accepted result",
    );
    refuses(
        inputs.fixture_phase("ab", &endpoint, Some(&grade_id)),
        "preceding phase",
    );
}

fn plan_fixture_grading(
    inputs: &ExternalInputs,
    subject_id: &str,
    endpoint: &str,
    private: &str,
) -> Value {
    use serde_json::json;
    let cas = af_store::Cas::open_existing(inputs.state.join("cas")).unwrap();
    let store = af_store::EventStore::open_read_only(inputs.state.join("events.sqlite")).unwrap();
    let task = store.task_projection(&cas, subject_id).unwrap().unwrap();
    let evaluator = json!({
        "schema":"af.agent-bench-evaluator/1", "status":"synthetic",
        "source_snapshot":task.revision.inputs["source"].snapshot_id,
        "report_sha256":&sha256(b"fixture report")[7..],
        "report_artifact":sha256(b"fixture report art"), "verification_artifact":sha256(b"fixture verifier"),
        "rubric_text_verbatim":private,
        "cases":(1..=12).map(|i| json!({"id":format!("N{i:02}"),"family":"synthetic",
            "abstention":false,"oracle_text_verbatim":format!("private oracle marker [N{i:02}]")})).collect::<Vec<_>>()
    });
    let path = inputs.root.path().join("grading-evaluator.json");
    let pin = write_external_input(&path, &evaluator);
    let codex = std::env::var_os("AF_NAVIGATION_CODEX").unwrap();
    let planned = common::af()
        .args(["agent-bench", "plan-grading", subject_id, "--repo"])
        .arg(&inputs.repo)
        .arg("--state")
        .arg(&inputs.state)
        .arg("--evaluator")
        .arg(&path)
        .args([
            "--evaluator-sha256",
            &pin,
            "--fixture-endpoint",
            endpoint,
            "--native-fixture-codex",
        ])
        .arg(codex)
        .output()
        .unwrap();
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    std::fs::remove_file(path).unwrap();
    serde_json::from_slice(&planned.stdout).unwrap()
}

#[test]
#[ignore = "requires the pinned Linux Codex and analyzer runtime"]
fn installed_external_grading_replays_absent_answers() {
    let inputs = ExternalInputs::new();
    let endpoint = "127.0.0.1:1";
    let subject = external_document(inputs.fixture_plan(endpoint));
    let subject_id = subject["task_id"].as_str().unwrap();
    let run = |plan: &Value| {
        common::af()
            .args([
                "task",
                "run",
                plan["task_id"].as_str().unwrap(),
                "--confirm-plan",
                plan["plan_id"].as_str().unwrap(),
                "--repo",
            ])
            .arg(&inputs.repo)
            .arg("--state")
            .arg(&inputs.state)
            .output()
            .unwrap()
    };
    let output = run(&subject);
    assert!(!output.status.success());
    let before = external_document(inputs.report(subject_id));
    assert_eq!(before["slots"][0]["status"], "failed");
    let unprovisioned = inputs.root.path().join("unused-live-grading-input.json");
    refuses(
        common::af()
            .args(["agent-bench", "plan-grading", subject_id, "--repo"])
            .arg(&inputs.repo)
            .arg("--state")
            .arg(&inputs.state)
            .arg("--evaluator")
            .arg(&unprovisioned)
            .args(["--evaluator-sha256", &sha256(b"unused synthetic evaluator")])
            .arg("--grading-transport")
            .arg(&unprovisioned)
            .args([
                "--grading-transport-sha256",
                &sha256(b"unused live declaration"),
            ])
            .output()
            .unwrap(),
        "Grading transport must match the retained subject's backend",
    );
    let plan = plan_fixture_grading(&inputs, subject_id, endpoint, "synthetic omitted oracle");
    let output = run(&plan);
    let id = plan["task_id"].as_str().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(inputs.status(id)["state"]["outcome"], "execution_failed");
    let report = inputs.report(id);
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let value: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(value["slots"][0]["subject_status"], "failed");
    assert_eq!(value["slots"][1]["subject_status"], "unstarted");
    assert_eq!(value["phase_evidence"]["ready"], false);
    assert_eq!(value["phase_evidence"]["slots"], 4);
    refuses(
        inputs.fixture_phase("aa", endpoint, Some(id)),
        "complete preceding phase evidence",
    );
    for slot in value["slots"].as_array().unwrap() {
        assert!(slot["decision"].is_null());
        for stage in slot["stages"].as_array().unwrap() {
            assert_eq!(
                stage["status"],
                "unavailable",
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(stage["calls"], serde_json::json!([]));
        }
    }
    assert_eq!(report.stdout, inputs.report(id).stdout);
    assert_eq!(before, external_document(inputs.report(subject_id)));
}
