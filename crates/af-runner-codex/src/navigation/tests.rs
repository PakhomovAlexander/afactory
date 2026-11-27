use super::*;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

pub(super) fn usage_value() -> Value {
    json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":20},"output_tokens":10,"output_tokens_details":{"reasoning_tokens":4},"total_tokens":110})
}
pub(super) fn terminal(usage: Option<Value>) -> Vec<u8> {
    let mut response = json!({"id":"resp-fixture","status":"completed","output":[]});
    if let Some(usage) = usage {
        response["usage"] = usage;
    }
    format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response})
    )
    .into_bytes()
}
fn tool_stream(tool: &str) -> Vec<u8> {
    let args = match tool {
        "af_list_paths" => json!({"offset":0}),
        "af_read_range" => json!({"path":"src/lib.rs","offset":0,"length":10}),
        "af_search" => json!({"query":"fn","offset":0}),
        "af_definition" | "af_references" | "af_implementation" => {
            json!({"path":"app/lib.rs","line":0,"character":32})
        }
        _ => json!({}),
    };
    let item = json!({"type":"function_call","id":"fc-fixture","call_id":"call-fixture","name":tool,"arguments":args.to_string()});
    let mut stream = format!(
        "data: {}\n\n",
        json!({"type":"response.output_item.done","item":item})
    )
    .into_bytes();
    stream.extend(terminal(Some(usage_value())));
    stream
}

#[test]
fn raw_usage_preserves_presence_and_reconciles_subsets_once() {
    let raw = usage::capture(&terminal(Some(usage_value())));
    let counts = raw.reconciled.unwrap();
    assert_eq!((counts.inclusive, counts.chargeable), (110, 90));
    assert!(raw.failure.is_none());
    for value in [None, Some(Value::Null), Some(json!({"input_tokens":"100"}))] {
        let raw = usage::capture(&terminal(value.clone()));
        assert_eq!(
            raw.value,
            value.map(RawField::Present).unwrap_or(RawField::Absent)
        );
        assert_eq!(
            serde_json::from_value::<RawUsage>(serde_json::to_value(&raw).unwrap()).unwrap(),
            raw
        );
        assert!(raw.reconciled.is_none());
        assert!(raw.failure.is_some());
    }
}

#[test]
fn raw_usage_rejects_missing_late_duplicate_contradictory_and_malformed_streams() {
    let good = terminal(Some(usage_value()));
    let mut duplicate = good.clone();
    duplicate.extend(&good);
    let mut late = good.clone();
    late.extend(b"data: {\"type\":\"response.created\"}\n\n");
    let mut malformed = good.clone();
    malformed.extend(b"data: nope\n\n");
    for body in [
        vec![],
        duplicate,
        late,
        malformed,
        good[..good.len() - 1].to_vec(),
    ] {
        assert!(usage::capture(&body).failure.is_some());
    }
    for (field, value) in [
        ("total_tokens", json!(999)),
        ("output_tokens", json!(-1)),
        ("input_tokens", json!(null)),
    ] {
        let mut usage = usage_value();
        usage[field] = value;
        assert!(usage::capture(&terminal(Some(usage))).failure.is_some());
    }
    let mut cache = usage_value();
    cache["input_tokens_details"]["cached_tokens"] = json!(101);
    assert!(usage::capture(&terminal(Some(cache))).failure.is_some());
    let mut reasoning = usage_value();
    reasoning["output_tokens_details"]["reasoning_tokens"] = json!(11);
    assert!(usage::capture(&terminal(Some(reasoning))).failure.is_some());
}

#[test]
fn end_markers_and_terminal_status_cannot_hide_contradictory_usage() {
    let good = terminal(Some(usage_value()));
    let mut early = b"data: [DONE]\n\n".to_vec();
    early.extend(&good);
    let mut after = good.clone();
    after.extend(b"data: [DONE]\n\n");
    after.extend(&good);
    let mut duplicated = good.clone();
    duplicated.extend(&good);
    for stream in [early, after, duplicated] {
        let raw = usage::capture(&stream);
        assert!(raw.failure.is_some());
        assert!(raw.reconciled.is_some());
    }
    let raw = usage::capture(&good);
    assert_eq!(raw.observations.len(), 1);
    let mut event = raw.observations[0].clone();
    event["response"]["status"] = json!("failed");
    let contradicted = format!("data: {event}\n\n");
    assert!(usage::capture(contradicted.as_bytes()).failure.is_some());
}

#[test]
fn dispatch_is_narrow_and_strict() {
    assert!(dispatch("af_list_paths", &json!({"offset":0})).is_ok());
    assert!(
        dispatch(
            "af_read_range",
            &json!({"path":"src/lib.rs","offset":0,"length":3})
        )
        .is_ok()
    );
    assert!(dispatch("af_search", &json!({"query":"fn","offset":0})).is_ok());
    for args in [
        json!({"path":"../oracle","offset":0,"length":1}),
        json!({"path":"src/lib.rs","offset":0,"length":65537}),
        json!({"path":"src/lib.rs","start_line":1,"line_count":1}),
    ] {
        assert!(dispatch("af_read_range", &args).is_err());
    }

    for name in [
        "request_user_input",
        "apply_patch",
        "exec",
        "exec_command",
        "spawn_agent",
        "collaboration.spawn_agent",
        "web_search",
        "view_image",
        "read_mcp_resource",
        "computer",
    ] {
        assert!(dispatch(name, &json!({})).is_err(), "{name}");
    }
    for args in [
        json!({"prefix":"","limit":0}),
        json!({"prefix":"","limit":257}),
        json!({"prefix":"","limit":2,"command":"sh"}),
    ] {
        assert!(dispatch("af_list_paths", &args).is_err());
    }
}

pub(super) struct Hooks {
    pub(super) order: Mutex<Vec<String>>,
    pub(super) refuse: bool,
    pub(super) bad_ack: bool,
}
impl NativeHooks for Hooks {
    fn before_send(&self, intent: &CallIntent, _: Instant) -> Result<DurableAck, String> {
        self.order
            .lock()
            .unwrap()
            .push(format!("guard:{}", intent.ordinal));
        if self.refuse {
            return Err("refused".into());
        }
        Ok(DurableAck {
            intent_id: if self.bad_ack {
                "wrong".into()
            } else {
                intent.id.clone()
            },
        })
    }
    fn collect(&self, record: &RawCallRecord) -> Result<DurableAck, String> {
        self.order
            .lock()
            .unwrap()
            .push(format!("collect:{}", record.intent.ordinal));
        Ok(DurableAck {
            intent_id: record.intent.id.clone(),
        })
    }
}
struct Broker<'a>(AtomicUsize, Option<&'a Hooks>);
impl NavigationHost for Broker<'_> {
    fn navigate(&self, request: Navigation, _: Instant) -> Result<String, String> {
        assert!(matches!(
            request,
            Navigation::ListPaths { .. } | Navigation::ReadRange { .. } | Navigation::Search { .. }
        ));
        if let Some(hooks) = self.1 {
            assert_eq!(
                hooks.order.lock().unwrap().last().map(String::as_str),
                Some("collect:1")
            );
        }
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok("[\"src/lib.rs\"]".into())
    }

    fn semantic(
        &self,
        _: SemanticOperation,
        position: SemanticPosition,
        _: Instant,
    ) -> Result<String, String> {
        assert_eq!(
            (position.path.as_str(), position.line, position.character),
            ("app/lib.rs", 0, 32)
        );
        assert_eq!(
            self.1.unwrap().order.lock().unwrap().last().unwrap(),
            "collect:1"
        );
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(r#"{"anchors":[{"path":"library/lib.rs","line":1}],"truncated":false}"#.into())
    }
}
pub(super) fn hooks() -> Hooks {
    Hooks {
        order: Mutex::new(vec![]),
        refuse: false,
        bad_ack: false,
    }
}

pub(super) fn request(auth: &std::path::Path, actual: bool) -> Request {
    let executable = if actual {
        std::env::var_os("AF_NAVIGATION_CODEX").map(PathBuf::from).expect("set AF_NAVIGATION_CODEX to the pinned 0.167.2 executable; this check is NOT RUN in ordinary gates")
    } else {
        std::env::var_os("AF_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("crates/af-runner-codex/src/navigation/fixtures/app_server.py")
    };
    let executable =
        std::fs::canonicalize(executable).expect("pinned app-server prerequisite unavailable");
    if actual && cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        assert_eq!(
            sha256(&std::fs::read(&executable).unwrap()),
            "sha256:97bc82fd46f117cf10efb95e6ff0c81dc4784f5797bcdc2cb75093ba09e4def0",
            "installed native executable pin mismatch"
        );
    }
    if actual && cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        assert_eq!(
            sha256(&std::fs::read(&executable).unwrap()),
            LINUX_X86_64_SHA256,
            "installed native executable pin mismatch"
        );
    }
    let catalog = include_bytes!("fixtures/models.json").to_vec();
    let question = "List the source paths.".to_owned();
    let bindings = Bindings {
        task_attempt_invocation: "task:attempt:invocation".into(),
        source: sha256(b"source-fixture"),
        prompt: prompt_identity(&question),
        tools: sha256(&serde_json::to_vec(&tool_schemas()).unwrap()),
        model_catalog: sha256(&catalog),
        executable: sha256(&std::fs::read(&executable).unwrap()),
        account: "dummy-fixture-account".into(),
        protocol: PROTOCOL.into(),
        config: config_identity(),
    };
    Request {
        profile: WorkerProfile::Subject,
        pin: Pin {
            executable,
            executable_sha256: bindings.executable.clone(),
            catalog,
            catalog_sha256: bindings.model_catalog.clone(),
        },
        native_auth_home: auth.into(),
        bindings,
        question,
        tools: ToolSet::Text,
        deadline: Instant::now() + Duration::from_secs(60),
        max_inclusive_tokens: 1000,
        max_sends: 4,
    }
}

pub(super) fn fixture_http(stream: &mut TcpStream) -> (String, Vec<u8>) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut header = vec![];
    while !header.ends_with(b"\r\n\r\n") {
        let mut b = [0];
        stream.read_exact(&mut b).unwrap();
        header.push(b[0]);
        assert!(header.len() < 16384);
    }
    let text = String::from_utf8(header).unwrap();
    let len = text
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; len];
    stream.read_exact(&mut body).unwrap();
    (text, body)
}

fn e2e(tool: &str, actual: bool) -> (Outcome, usize, Vec<String>) {
    e2e_with_navigation(tool, actual, None, ToolSet::Text)
}
fn e2e_with_navigation(
    tool: &str,
    actual: bool,
    navigation: Option<(&dyn NavigationHost, &str)>,
    tools: ToolSet,
) -> (Outcome, usize, Vec<String>) {
    let mut request = request(std::path::Path::new("/unused"), actual);
    request.tools = tools;
    request.bindings.tools = tools.identity();
    if let Some((_, source)) = navigation {
        request.bindings.source = source.into();
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let hooks = hooks();
    let broker = Broker(AtomicUsize::new(0), Some(&hooks));
    let outcome = std::thread::scope(|scope| {
        let stop_ref = &stop;
        let serving=scope.spawn(move || {
            let mut count=0;
            while !stop_ref.load(Ordering::Acquire) {
                let (mut stream,_) = match listener.accept() { Ok(s)=>s,Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>{ std::thread::sleep(Duration::from_millis(5));continue; },Err(e)=>panic!("{e}") };
                let (headers,body)=fixture_http(&mut stream);
                assert!(headers.starts_with("POST /responses HTTP/1.1\r\n"));
                assert!(headers.contains("Authorization: Bearer dummy-native-access\r\n"));
                assert!(headers.contains("ChatGPT-Account-ID: fixture-account\r\n"));
                assert!(!headers.to_ascii_lowercase().contains("x-af-gateway"));
                let input:Value=serde_json::from_slice(&body).unwrap();
                let names:Vec<_>=proxy::request_tools(&input).unwrap().iter().map(|t|t["name"].as_str().unwrap()).collect();
                let schemas = tools.schemas();
                assert_eq!(names.len(), schemas.as_array().unwrap().len() + 1, "actual registered catalog: {names:?}");
                for schema in schemas.as_array().unwrap() {
                    assert!(names.contains(&schema["name"].as_str().unwrap()), "{names:?}");
                }
                assert!(names.contains(&"request_user_input"));
                let body=if count==0 {tool_stream(tool)} else { answer_stream() };
                count+=1;
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();stream.write_all(&body).unwrap();
            }
            count
        });
        let std::net::SocketAddr::V4(address) = address else {
            unreachable!()
        };
        let outcome = run_fixture(
            request,
            address,
            navigation.map(|(host, _)| host).unwrap_or(&broker),
            &hooks,
        )
        .unwrap();
        stop.store(true, Ordering::Release);
        let count = serving.join().unwrap();
        (outcome, count)
    });
    assert!(
        !outcome.0.calls.is_empty(),
        "no inference for {tool}: {:?}",
        outcome.0.failure
    );
    (
        outcome.0,
        broker.0.load(Ordering::Relaxed),
        hooks.order.into_inner().unwrap(),
    )
}
fn answer_stream() -> Vec<u8> {
    let item = json!({"id":"msg-fixture","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"src/lib.rs","annotations":[]}]});
    let mut body = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"response.output_item.added","output_index":0,"item":item}),
        json!({"type":"response.output_item.done","output_index":0,"item":item})
    )
    .into_bytes();
    body.extend(terminal(Some(usage_value())));
    body
}

#[test]
fn fixture_app_server_registration_dispatch_and_usage_barrier() {
    let (outcome, dispatches, order) = e2e("af_list_paths", false);
    assert_eq!(dispatches, 1, "{:?}", outcome.failure);
    assert_eq!(outcome.calls.len(), 2, "{:?}", outcome.failure);
    assert_eq!(order, ["guard:1", "collect:1", "guard:2", "collect:2"]);
    assert_ne!(outcome.calls[0].intent.id, outcome.calls[1].intent.id);
    assert_eq!(
        outcome.calls[0]
            .usage
            .as_ref()
            .unwrap()
            .reconciled
            .as_ref()
            .unwrap()
            .chargeable,
        90
    );
    assert!(outcome.failure.is_none(), "{outcome:?}");
    assert!(outcome.cleanup_confirmed);
    assert_eq!(outcome.answer.as_deref(), Some("src/lib.rs"));
}

#[test]
fn fixture_app_server_denies_owner_and_forbidden_dispatch() {
    for tool in [
        "af_definition",
        "request_user_input",
        "apply_patch",
        "exec_command",
        "spawn_agent",
        "collaboration.spawn_agent",
        "web_search",
        "view_image",
        "read_mcp_resource",
    ] {
        let (outcome, dispatches, order) = e2e(tool, false);
        assert!(outcome.failure.is_some(), "{tool}");
        assert_eq!(dispatches, 0, "{tool}");
        assert_eq!(outcome.calls.len(), 1, "{tool}");
        assert_eq!(order, ["guard:1", "collect:1"]);
    }
}

#[test]
#[ignore = "requires pinned local Codex 0.167.2; run exact documented conformance command with dummy auth only"]
fn installed_app_server_conformance() {
    for tool in ["af_list_paths", "af_read_range", "af_search"] {
        let (outcome, dispatches, order) = e2e(tool, true);
        assert_eq!(dispatches, 1, "{tool}: {outcome:?}");
        assert_eq!(outcome.calls.len(), 2, "{tool}: {outcome:?}");
        assert_eq!(order, ["guard:1", "collect:1", "guard:2", "collect:2"]);
        assert!(
            outcome.calls.iter().all(|call| call.failure.is_none()),
            "{tool}: {outcome:?}"
        );
        assert!(outcome.process_timing.is_some(), "{tool}: {outcome:?}");
        assert!(outcome.failure.is_none(), "{tool}: {outcome:?}");
        assert!(outcome.cleanup_confirmed, "{tool}: {outcome:?}");
        assert_eq!(outcome.answer.as_deref(), Some("src/lib.rs"));
    }
    for tool in ["af_definition", "af_references", "af_implementation"] {
        let (outcome, dispatches, order) = e2e_with_navigation(tool, true, None, ToolSet::Semantic);
        assert!(outcome.failure.is_none(), "{tool}: {outcome:?}");
        assert_eq!(dispatches, 1);
        assert_eq!(order, ["guard:1", "collect:1", "guard:2", "collect:2"]);
    }
    for tool in [
        "af_definition",
        "request_user_input",
        "apply_patch",
        "exec_command",
        "spawn_agent",
        "web_search",
        "view_image",
        "read_mcp_resource",
    ] {
        let (outcome, dispatches, _) = e2e(tool, true);
        assert!(outcome.failure.is_some(), "{tool}");
        assert_eq!(dispatches, 0, "{tool}");
        assert_eq!(outcome.calls.len(), 1, "{tool}");
    }
}

#[test]
fn bindings_and_ambient_configuration_fail_before_network() {
    let home = tempfile::tempdir().unwrap();
    let hooks = hooks();
    let broker = Broker(AtomicUsize::new(0), None);
    for mutation in 0..4 {
        let mut request = request(home.path(), false);
        match mutation {
            0 => request.bindings.prompt = "wrong".into(),
            1 => request.deadline = Instant::now(),
            2 => request.pin.catalog_sha256 = "wrong".into(),
            _ => std::fs::write(home.path().join("config.toml"), "model='other'").unwrap(),
        };
        let outcome = server::run(request, &broker, &hooks, proxy::Upstream::Official);
        assert!(outcome.failure.is_some());
        assert!(outcome.calls.is_empty());
    }
    assert!(hooks.order.into_inner().unwrap().is_empty());
}

#[test]
fn raw_record_schema_and_strict_reader_agree() {
    let home = tempfile::tempdir().unwrap();
    let request = request(home.path(), false);
    let record = RawCallRecord {
        schema: CallRecordSchema::V1,
        intent: CallIntent {
            id: "unique".into(),
            ordinal: 1,
            route: "/responses".into(),
            request_sha256: sha256(b"request"),
            bindings: request.bindings,
            started_mono_ns: 1,
        },
        http_status: Some(200),
        response_sha256: sha256(b"response"),
        response_bytes: 12,
        usage: Some(usage::capture(&terminal(Some(usage_value())))),
        drained_mono_ns: 2,
        failure: None,
    };
    let schema: Value = serde_json::from_str(include_str!(
        "../../../../schemas/codex-navigation-call-v1.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let value = serde_json::to_value(&record).unwrap();
    assert!(validator.is_valid(&value));
    assert_eq!(
        serde_json::from_value::<RawCallRecord>(value.clone()).unwrap(),
        record
    );
    for (key, change) in [
        ("extra", json!(true)),
        ("schema", json!("af/codex-navigation-call/0")),
    ] {
        let mut invalid = value.clone();
        invalid[key] = change;
        assert!(!validator.is_valid(&invalid));
        assert!(serde_json::from_value::<RawCallRecord>(invalid).is_err());
    }
    for native in [
        None,
        Some(Value::Null),
        Some(json!({"input_tokens":"invalid"})),
    ] {
        let mut record = record.clone();
        record.usage = Some(usage::capture(&terminal(native)));
        let value = serde_json::to_value(&record).unwrap();
        assert!(validator.is_valid(&value));
        assert_eq!(
            serde_json::from_value::<RawCallRecord>(value).unwrap(),
            record
        );
    }
}

// Exercise real OS APIs beneath the same launcher used by the app-server. No model,
// credentials, shell, or network; failure to enforce the policy fails the check.
#[cfg(target_os = "macos")]
#[test]
fn macos_launcher_denies_fork_spawn_and_subprocess() {
    let script = r#"
import errno, os, subprocess
for operation in [lambda: os.fork(),
                  lambda: os.posix_spawn('/usr/bin/true', ['true'], {}),
                  lambda: subprocess.run(['/usr/bin/true'], check=True)]:
    try:
        result = operation()
    except OSError as error:
        assert error.errno == errno.EPERM, error
    else:
        if isinstance(result, int) and result == 0:
            os._exit(99)
        raise AssertionError('process creation unexpectedly permitted')
print('fork posix_spawn subprocess: EPERM')
"#;
    let mut command = server::transport_command(
        std::path::Path::new("/usr/bin/python3"),
        Instant::now() + Duration::from_secs(10),
    )
    .expect("pinned macOS launcher unavailable: conformance NOT RUN");
    command.env_clear().args(["-I", "-c", script]);
    let output = af_process::run_supervised_captured_bounded(
        &mut command,
        None,
        Duration::from_secs(10),
        af_process::ExitPolicy::KillDescendants,
        None,
        4096,
    );
    assert!(
        output.status.as_ref().is_ok_and(|status| status.success()),
        "status: {:?}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"fork posix_spawn subprocess: EPERM\n");
    assert!(!output.stderr_held);
    assert!(!output.truncation.is_truncated());
}

#[test]
#[ignore = "requires actual Docker and pinned local image; credential-free integration conformance"]
fn docker_broker_transport_unicode_and_pagination_conformance() {
    use af_sandbox::navigation::{Broker as SourceBroker, Response};
    use std::collections::BTreeMap;
    struct RealNavigation {
        broker: Mutex<SourceBroker>,
        seen: Mutex<Vec<(Navigation, Response)>>,
    }
    impl NavigationHost for RealNavigation {
        fn navigate(&self, request: Navigation, deadline: Instant) -> Result<String, String> {
            let response = self
                .broker
                .lock()
                .unwrap()
                .execute_until(&request, deadline)
                .map_err(|e| e.to_string())?;
            self.seen.lock().unwrap().push((request, response.clone()));
            serde_json::to_string(&response).map_err(|e| e.to_string())
        }
    }
    let mut files: BTreeMap<_, _> = (0..1100).map(|n| (format!("file{n:04}"), vec![])).collect();
    files.insert("binary".into(), vec![0xff, 0]);
    files.insert("src/lib.rs".into(), "é\nfn line\n".as_bytes().to_vec());
    let broker = SourceBroker::new(files).unwrap();
    let source = broker.source_identity().to_owned();
    let navigation = RealNavigation {
        broker: Mutex::new(broker),
        seen: Mutex::new(vec![]),
    };
    for tool in ["af_list_paths", "af_read_range", "af_search"] {
        let (outcome, _, order) =
            e2e_with_navigation(tool, false, Some((&navigation, &source)), ToolSet::Text);
        assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
        assert!(outcome.cleanup_confirmed);
        assert_eq!(order, ["guard:1", "collect:1", "guard:2", "collect:2"]);
        assert!(
            outcome
                .calls
                .iter()
                .all(|call| call.intent.bindings.source == source)
        );
    }
    let seen = navigation.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(
        matches!(&seen[0].1, Response::ListPaths { paths, next_offset:Some(256) } if paths.len() == 256)
    );
    assert_eq!(
        seen[1].0,
        Navigation::ReadRange {
            path: "src/lib.rs".into(),
            offset: 0,
            length: 10
        }
    );
    assert_eq!(
        seen[1].1,
        Response::ReadRange {
            text: Some("é\nfn line".into())
        }
    );
    assert!(
        matches!(&seen[2].1, Response::Search {hits, next_offset:None, non_text_paths} if hits.len() == 1 && hits[0].line == 2 && non_text_paths == &["binary"])
    );
}
