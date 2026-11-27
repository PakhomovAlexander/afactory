use super::*;
use af_process::{ExitPolicy, run_supervised_captured_bounded, run_supervised_duplex_signals};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read},
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

// System launcher bytes are an admitted capability, not a PATH lookup. An OS update
// changing them requires renewed conformance and a new pin, never a fallback.
pub(super) const MAC_LAUNCHER: &str = "/usr/bin/sandbox-exec";
pub(super) const MAC_LAUNCHER_SHA256: &str =
    "sha256:58839ef01b4eef8aac0d2aa8f9d1c074ae45aafe3533965b030672450064acc8";
pub(super) const MAC_PROFILE: &str = "(version 1) (allow default) (deny process-fork)";

fn lifecycle_identity() -> Value {
    if cfg!(target_os = "macos") {
        json!({"launcher":MAC_LAUNCHER,"sha256":MAC_LAUNCHER_SHA256,"profile":MAC_PROFILE})
    } else {
        json!({"supervisor":"af-process/linux-subreaper"})
    }
}

pub(super) fn lifecycle_confirmed(process_ok: bool) -> bool {
    process_ok
        && (cfg!(target_os = "macos")
            || (cfg!(target_os = "linux") && af_process::descendant_cleanup_fallback().is_none()))
}

pub(super) fn transport_command(
    executable: &std::path::Path,
    deadline: Instant,
) -> Result<Command, String> {
    if cfg!(target_os = "macos") {
        if file_digest(std::path::Path::new(MAC_LAUNCHER), deadline)? != MAC_LAUNCHER_SHA256 {
            return Err("macOS no-descendant launcher pin unavailable".into());
        }
        let mut command = Command::new(MAC_LAUNCHER);
        command.args(["-p", MAC_PROFILE]).arg(executable);
        Ok(command)
    } else if cfg!(target_os = "linux") {
        Ok(Command::new(executable))
    } else {
        Err("no supported navigation lifecycle enforcement".into())
    }
}

/// Identity excludes per-run gateway secret/address and native auth location. Those are
/// transport capabilities, not instructions; no private header is ever hashed into a record.
pub fn config_identity() -> String {
    WorkerProfile::Subject.config_identity()
}

pub fn prompt_identity(question: &str) -> String {
    WorkerProfile::Subject.prompt_identity(question)
}

impl WorkerProfile {
    pub fn config_identity(self) -> String {
        sha256(&serde_json::to_vec(&json!({"lifecycle":lifecycle_identity(),"protocol":PROTOCOL,"model":self.model(),"effort":self.effort(),"official_account_binding":"sha256-native-chatgpt-account-id","features_disabled":FEATURES,"project_doc_max_bytes":0,"skills":false,"fallback":false,"environments":[],"ephemeral":true,"approvalPolicy":"never","sandbox":"read-only","retries":0,"tools":tool_schemas()})).unwrap())
    }

    pub fn prompt_identity(self, question: &str) -> String {
        sha256(&serde_json::to_vec(&json!([self.base(), self.developer(), question])).unwrap())
    }
}

fn file_digest(path: &std::path::Path, deadline: Instant) -> Result<String, String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "cannot inspect pinned executable")?;
    if !metadata.is_file() || metadata.len() > 512 * 1024 * 1024 {
        return Err("pinned executable must be a bounded regular native file".into());
    }
    let mut file = std::fs::File::open(path).map_err(|_| "cannot open pinned executable")?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        if Instant::now() >= deadline {
            return Err("executable pin deadline expired".into());
        }
        let n = file
            .read(&mut buffer)
            .map_err(|_| "cannot hash pinned executable")?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("sha256:{}", hex(&hash.finalize())))
}

fn validate(request: &Request) -> Result<(), String> {
    if request.question.is_empty()
        || request.question.len() > MAX_PROMPT_BYTES
        || request.max_sends == 0
        || request.max_sends > 256
        || request.max_inclusive_tokens == 0
        || Instant::now() >= request.deadline
    {
        return Err("invalid bounded navigation request".into());
    }
    if !request.native_auth_home.is_absolute()
        || !request.pin.executable.is_absolute()
        || file_digest(&request.pin.executable, request.deadline)? != request.pin.executable_sha256
        || sha256(&request.pin.catalog) != request.pin.catalog_sha256
        || request.pin.catalog.len() > MAX_BYTES
    {
        return Err("executable/catalog pin mismatch".into());
    }
    let bindings = &request.bindings;
    if bindings.executable != request.pin.executable_sha256
        || bindings.model_catalog != request.pin.catalog_sha256
        || bindings.protocol != PROTOCOL
        || bindings.prompt != request.profile.prompt_identity(&request.question)
        || bindings.tools != request.tools.identity()
        || bindings.config != request.profile.config_identity()
        || bindings.source.is_empty()
        || bindings.account.is_empty()
        || bindings.task_attempt_invocation.is_empty()
    {
        return Err("unbound navigation identity".into());
    }
    super::validate_worker_catalog(&request.pin.catalog, request.profile)?;
    // The caller supplies a native-owned auth home, never a candidate workspace or its
    // config. Do not read auth.json. Refuse ambient policy that could widen the tool set.
    for entry in
        std::fs::read_dir(&request.native_auth_home).map_err(|_| "native auth home unavailable")?
    {
        let entry = entry.map_err(|_| "native auth home unavailable")?;
        if entry.file_name() != "auth.json" {
            return Err("native auth home contains ambient configuration".into());
        }
        if !entry
            .file_type()
            .map_err(|_| "native auth metadata unavailable")?
            .is_file()
        {
            return Err("native auth must be a regular file".into());
        }
    }
    Ok(())
}

pub(super) fn run(
    request: Request,
    navigation: &dyn NavigationHost,
    hooks: &dyn NativeHooks,
    upstream: proxy::Upstream,
) -> Outcome {
    let started = Instant::now();
    let mut outcome = Outcome {
        answer: None,
        calls: vec![],
        failure: None,
        started,
        drained: started,
        cleanup_confirmed: false,
        process_timing: None,
    };
    if let Err(reason) = validate(&request) {
        outcome.failure = Some(reason);
        return outcome;
    }
    let mut version = match transport_command(&request.pin.executable, request.deadline) {
        Ok(command) => command,
        Err(reason) => {
            outcome.failure = Some(reason);
            return outcome;
        }
    };
    version.env_clear().args(["--version"]);
    let checked = run_supervised_captured_bounded(
        &mut version,
        None,
        request
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(10)),
        ExitPolicy::KillDescendants,
        None,
        4096,
    );
    if !lifecycle_confirmed(checked.status.is_ok_and(|s| s.success()))
        || String::from_utf8_lossy(&checked.stdout).trim() != VERSION
        || checked.stderr_held
        || checked.truncation.is_truncated()
    {
        outcome.failure = Some("pinned Codex protocol version unavailable".into());
        return outcome;
    }
    let runtime = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => {
            outcome.failure = Some("navigation runtime unavailable".into());
            return outcome;
        }
    };
    let catalog = runtime.path().join("models.json");
    if std::fs::write(&catalog, &request.pin.catalog).is_err() {
        outcome.failure = Some("cannot stage pinned metadata".into());
        return outcome;
    }
    let gateway = match proxy::Gateway::new(&request, hooks, upstream, started) {
        Ok(g) => g,
        Err(reason) => {
            outcome.failure = Some(reason);
            return outcome;
        }
    };
    let mut command = match transport_command(&request.pin.executable, request.deadline) {
        Ok(command) => command,
        Err(reason) => {
            outcome.failure = Some(reason);
            return outcome;
        }
    };
    command
        .env_clear()
        .env("HOME", runtime.path())
        .env("CODEX_HOME", &request.native_auth_home)
        .env("PATH", af_process::sanitized_path())
        .env("TMPDIR", runtime.path())
        .current_dir(runtime.path());
    command.env("AF_NATIVE_GATEWAY_SECRET", &gateway.secret);
    let config = |key: &str, value: Value| format!("{key}={value}");
    let settings = [
        config("model", json!(request.profile.model())),
        config("model_reasoning_effort", json!(request.profile.effort())),
        config("model_provider", json!("af_navigation")),
        config("model_catalog_json", json!(catalog)),
        config("project_doc_max_bytes", json!(0)),
        config("skills.include_instructions", json!(false)),
        config("skills.bundled.enabled", json!(false)),
        config("web_search", json!("disabled")),
        config("cli_auth_credentials_store", json!("file")),
        config(
            "model_providers.af_navigation.name",
            json!("Restricted navigation"),
        ),
        config(
            "model_providers.af_navigation.base_url",
            json!(format!("http://{}", gateway.address)),
        ),
        config("model_providers.af_navigation.wire_api", json!("responses")),
        config(
            "model_providers.af_navigation.requires_openai_auth",
            json!(true),
        ),
        config(
            "model_providers.af_navigation.supports_websockets",
            json!(false),
        ),
        config(
            "model_providers.af_navigation.request_max_retries",
            json!(0),
        ),
        config("model_providers.af_navigation.stream_max_retries", json!(0)),
        config(
            "model_providers.af_navigation.env_http_headers.x-af-gateway",
            json!("AF_NATIVE_GATEWAY_SECRET"),
        ),
    ];
    for setting in settings {
        command.args(["-c", &setting]);
    }
    command.args(["-c", "mcp_servers={}"]);
    for feature in FEATURES {
        command.args(["-c", &format!("features.{feature}=false")]);
    }
    command.args(["app-server", "--listen", "stdio://"]);
    let cancel = AtomicBool::new(false);
    let session = Mutex::new(None);
    std::thread::scope(|scope| {
        let gateway_thread = scope.spawn(|| gateway.serve());
        let (send, receive) = mpsc::sync_channel::<Value>(8);
        let deadline = request.deadline;
        let process = run_supervised_duplex_signals(
            &mut command,
            request.deadline.saturating_duration_since(Instant::now()),
            ExitPolicy::KillDescendants,
            &[&cancel],
            move |stdin| -> Result<(), String> {
                while let Ok(value) =
                    receive.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    serde_json::to_writer(&mut *stdin, &value)
                        .map_err(|_| "app-server input failed")?;
                    stdin
                        .write_all(b"\n")
                        .map_err(|_| "app-server input failed")?;
                    stdin.flush().map_err(|_| "app-server input failed")?;
                }
                Ok(())
            },
            |stdout| {
                let result = protocol(stdout, &send, &request, runtime.path(), navigation);
                drop(send);
                if result.is_err() {
                    cancel.store(true, Ordering::Release);
                }
                *session.lock().unwrap() = Some(result);
            },
        );
        gateway.stop.store(true, Ordering::Release);
        gateway_thread.join().unwrap();
        outcome.drained = Instant::now();
        let mut state = gateway.state.lock().unwrap();
        outcome.calls = std::mem::take(&mut state.calls);
        outcome.failure = state.failure.clone();
        if let Some(result) = session.lock().unwrap().take() {
            match result {
                Ok(answer) => outcome.answer = Some(answer),
                Err(reason) => {
                    outcome.failure.get_or_insert(reason);
                }
            }
        } else {
            outcome
                .failure
                .get_or_insert("app-server did not complete".into());
        }
        outcome.process_timing = process.as_ref().ok().map(|p| p.timing);
        let process_ok = process.as_ref().is_ok_and(|p| {
            p.status.success() && p.input.is_ok() && !p.stderr_held && !p.truncation.is_truncated()
        });
        // macOS forbids descendant creation in the kernel before exec of the pinned
        // trusted transport. Successful exit and drained pipes therefore close its entire
        // lifecycle; process-group termination alone never supplies this proof.
        outcome.cleanup_confirmed =
            lifecycle_confirmed(process_ok) && state.helpers_cleanup_confirmed;
        if !process_ok {
            outcome
                .failure
                .get_or_insert("app-server exit/drain failed".into());
        }
        if !outcome.cleanup_confirmed {
            outcome
                .failure
                .get_or_insert("descendant cleanup unconfirmed".into());
        }
        if outcome.drained >= request.deadline {
            outcome
                .failure
                .get_or_insert("navigation deadline expired".into());
        }
        if !outcome
            .calls
            .iter()
            .any(|call| call.intent.route == "/responses")
        {
            outcome
                .failure
                .get_or_insert("no native inference captured".into());
        }
        if outcome.failure.is_some() {
            outcome.answer = None;
        }
    });
    outcome
}

fn emit(send: &mpsc::SyncSender<Value>, value: Value) -> Result<(), String> {
    send.try_send(value)
        .map_err(|_| "app-server input queue full or closed".into())
}

fn protocol(
    stdout: &mut dyn Read,
    send: &mpsc::SyncSender<Value>,
    request: &Request,
    cwd: &std::path::Path,
    navigation: &dyn NavigationHost,
) -> Result<String, String> {
    emit(
        send,
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"af_navigation","version":"1"},"capabilities":{"experimentalApi":true}}}),
    )?;
    let mut reader = BufReader::new(stdout);
    let mut total = 0;
    let mut phase = 1;
    let mut thread = String::new();
    let mut turn = String::new();
    let mut call_ids = BTreeSet::new();
    let mut answer = String::new();
    loop {
        if Instant::now() >= request.deadline {
            return Err("app-server protocol deadline".into());
        }
        let mut line = Vec::new();
        let n = reader
            .by_ref()
            .take((MAX_BYTES - total + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| "app-server output failed")?;
        total += n;
        if total > MAX_BYTES {
            return Err("app-server output bound".into());
        }
        if n == 0 {
            return Err("app-server ended before turn completion".into());
        }
        let event: Value =
            serde_json::from_slice(&line).map_err(|_| "malformed app-server message")?;
        if event.get("error").is_some() {
            return Err("app-server protocol refused configuration".into());
        }
        if let Some(id) = event.get("id").filter(|_| event.get("method").is_none()) {
            if id.as_u64() != Some(phase) {
                return Err("unexpected app-server response".into());
            }
            match phase {
                1 => {
                    emit(send, json!({"method":"initialized"}))?;
                    emit(
                        send,
                        json!({"id":2,"method":"thread/start","params":{"model":request.profile.model(),"modelProvider":"af_navigation","allowProviderModelFallback":false,"environments":[],"ephemeral":true,"approvalPolicy":"never","sandbox":"read-only","cwd":cwd,"baseInstructions":request.profile.base(),"developerInstructions":request.profile.developer(),"dynamicTools":request.tools.schemas()}}),
                    )?;
                    phase = 2;
                }
                2 => {
                    let result = &event["result"];
                    if result["model"] != request.profile.model()
                        || result["modelProvider"] != "af_navigation"
                        || result["approvalPolicy"] != "never"
                        || result["sandbox"]["type"] != "readOnly"
                        || result
                            .get("instructionSources")
                            .is_some_and(|v| v != &json!([]))
                    {
                        return Err("app-server effective configuration mismatch".into());
                    }
                    thread = result["thread"]["id"]
                        .as_str()
                        .ok_or("missing thread identity")?
                        .into();
                    emit(
                        send,
                        json!({"id":3,"method":"turn/start","params":{"threadId":thread,"model":request.profile.model(),"effort":request.profile.effort(),"input":[{"type":"text","text":request.question,"text_elements":[]}]}}),
                    )?;
                    phase = 3;
                }
                3 => {
                    turn = event["result"]["turn"]["id"]
                        .as_str()
                        .ok_or("missing turn identity")?
                        .into();
                    phase = 4;
                }
                _ => return Err("unexpected app-server response phase".into()),
            }
            continue;
        }
        let method = event["method"]
            .as_str()
            .ok_or("missing app-server method")?;
        let params = &event["params"];
        if let Some(id) = event.get("id") {
            if method != "item/tool/call" {
                // Owner input, approvals, resources, UI and every unknown server request are
                // denied locally. There is no callback to a human or general tool dispatcher.
                let _ = emit(
                    send,
                    json!({"id":id,"error":{"code":-32601,"message":"navigation denies owner interaction and native tools"}}),
                );
                return Err("forbidden app-server owner/native request".into());
            }
            if params["threadId"] != thread
                || params["turnId"] != turn
                || params.get("namespace").is_some_and(|n| !n.is_null())
            {
                return Err("unbound navigation tool call".into());
            }
            let call_id = params["callId"]
                .as_str()
                .ok_or("missing tool call identity")?;
            if !call_ids.insert(call_id.to_owned()) {
                return Err("duplicate navigation tool call".into());
            }
            let name = params["tool"].as_str().unwrap_or("");
            let arguments = &params["arguments"];
            let output = if let Some(operation) = request.tools.semantic_operation(name) {
                let position: SemanticPosition = serde_json::from_value(arguments.clone())
                    .map_err(|_| "invalid semantic navigation arguments")?;
                if !af_contracts::navigation::valid_path(&position.path)
                    || !position.path.ends_with(".rs")
                {
                    return Err("invalid semantic navigation path".into());
                }
                navigation.semantic(operation, position, request.deadline)
            } else {
                navigation.navigate(dispatch(name, arguments)?, request.deadline)
            }
            .map_err(|_| "navigation broker refused")?;
            if output.len() > 65536 || Instant::now() >= request.deadline {
                return Err("navigation broker reply bound/deadline".into());
            }
            emit(
                send,
                json!({"id":id,"result":{"contentItems":[{"type":"inputText","text":output}],"success":true}}),
            )?;
        } else {
            if let Some(item) = params.get("item") {
                match item["type"].as_str() {
                    Some("userMessage" | "reasoning" | "dynamicToolCall") => {}
                    Some("agentMessage") => {
                        if method == "item/completed" {
                            answer = item["text"].as_str().ok_or("missing answer text")?.into();
                        }
                    }
                    _ => return Err("forbidden app-server item".into()),
                }
            }
            if method == "turn/completed" {
                if params["threadId"] != thread
                    || params["turn"]["id"] != turn
                    || params["turn"]["status"] != "completed"
                    || answer.is_empty()
                {
                    return Err("unsuccessful app-server turn".into());
                }
                return Ok(answer);
            }
            if method == "error" {
                return Err("app-server inference failed".into());
            }
        }
    }
}
