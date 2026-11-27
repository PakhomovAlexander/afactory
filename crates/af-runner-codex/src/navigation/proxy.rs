use super::*;
use af_process::{ExitPolicy, run_supervised_captured_bounded};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(super) enum Upstream {
    Official,
    Fixture(SocketAddr),
}

pub(super) struct State {
    pub calls: Vec<RawCallRecord>,
    pub failure: Option<String>,
    pub inclusive: u64,
    pub response_bytes: u64,
    pub helpers_cleanup_confirmed: bool,
}

pub(super) struct Gateway<'a> {
    listener: TcpListener,
    pub address: SocketAddr,
    // Deliberately neither Debug nor Serialize. Never copied into receipts or diagnostics.
    pub secret: String,
    pub state: Mutex<State>,
    pub stop: AtomicBool,
    pub request: &'a Request,
    hooks: &'a dyn NativeHooks,
    upstream: Upstream,
    origin: Instant,
    nonce: String,
}

fn random() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "gateway randomness unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl<'a> Gateway<'a> {
    pub fn new(
        request: &'a Request,
        hooks: &'a dyn NativeHooks,
        upstream: Upstream,
        origin: Instant,
    ) -> Result<Self, String> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|_| "cannot bind private gateway")?;
        let address = listener
            .local_addr()
            .map_err(|_| "cannot inspect gateway")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "cannot configure gateway")?;
        Ok(Self {
            listener,
            address,
            secret: random()?,
            nonce: random()?,
            state: Mutex::new(State {
                calls: vec![],
                failure: None,
                inclusive: 0,
                response_bytes: 0,
                helpers_cleanup_confirmed: true,
            }),
            stop: AtomicBool::new(false),
            request,
            hooks,
            upstream,
            origin,
        })
    }

    pub fn serve(&self) {
        while !self.stop.load(Ordering::Acquire) && Instant::now() < self.request.deadline {
            match self.listener.accept() {
                Ok((mut stream, peer)) => {
                    let result = if peer.ip().is_loopback() {
                        self.handle(&mut stream)
                    } else {
                        Err("non-loopback peer".into())
                    };
                    if let Err(reason) = result {
                        if reason == "empty gateway connection" {
                            continue;
                        }
                        self.fail(&reason);
                        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => {
                    self.fail("gateway accept failed");
                    break;
                }
            }
        }
    }

    pub fn fail(&self, reason: &str) {
        let mut state = self.state.lock().unwrap();
        if state.failure.is_none() {
            state.failure = Some(reason.into());
        }
    }

    fn handle(&self, stream: &mut TcpStream) -> Result<(), String> {
        // On macOS accept inherits the listener's O_NONBLOCK flag. Restore blocking
        // reads before applying absolute socket deadlines; partial native HTTP writes
        // must not be mistaken for a malformed request.
        stream
            .set_nonblocking(false)
            .map_err(|_| "gateway socket mode failed")?;
        let incoming = incoming(stream, self.address, &self.secret, self.request.deadline)?;
        let route = match (incoming.method.as_str(), incoming.path.as_str()) {
            ("GET", "/models") if incoming.body.is_empty() => "/models",
            ("POST", "/responses" | "/v1/responses") => "/responses",
            _ => return Err("gateway route refused".into()),
        };
        if route == "/responses" {
            validate_worker_request(&incoming.body, self.request.tools, self.request.profile)?;
        }
        if matches!(self.upstream, Upstream::Official)
            && !admitted_personal_account(
                incoming.account.as_deref(),
                &self.request.bindings.account,
            )
        {
            return Err("Native Codex Personal account differs from the admitted binding".into());
        }
        let intent = {
            let state = self.state.lock().unwrap();
            if state.failure.is_some()
                || state.response_bytes >= (4 * MAX_BYTES) as u64
                || state.calls.len() >= self.request.max_sends as usize
                || state.inclusive >= self.request.max_inclusive_tokens
            {
                return Err("remaining native scope exhausted or unknown".into());
            }
            CallIntent {
                id: format!(
                    "{}:{}:{}",
                    self.request.bindings.task_attempt_invocation,
                    self.nonce,
                    state.calls.len() + 1
                ),
                ordinal: (state.calls.len() + 1) as u32,
                route: route.into(),
                request_sha256: sha256(&incoming.body),
                bindings: self.request.bindings.clone(),
                started_mono_ns: self.origin.elapsed().as_nanos(),
            }
        };
        if Instant::now() >= self.request.deadline {
            return Err("native send deadline expired".into());
        }
        let ack = self
            .hooks
            .before_send(&intent, self.request.deadline)
            .map_err(|_| "native call guard refused")?;
        if ack.intent_id != intent.id || Instant::now() >= self.request.deadline {
            return Err("native call intent acknowledgement refused or late".into());
        }
        // No retry at this boundary. A new request (retry or compaction included) necessarily
        // passes the same guard and preceding-usage barrier, with a fresh globally unique ID.
        let response = forward(&self.upstream, route, &incoming, self.request.deadline);
        self.state.lock().unwrap().helpers_cleanup_confirmed &= response.cleanup_confirmed;
        let mut record = RawCallRecord {
            schema: CallRecordSchema::V1,
            intent,
            http_status: response.status,
            response_sha256: sha256(&response.body),
            response_bytes: response.body.len() as u64,
            usage: (route == "/responses").then(|| usage::capture(&response.body)),
            drained_mono_ns: self.origin.elapsed().as_nanos(),
            failure: response.failure,
        };
        if let Some(usage) = &record.usage {
            record.failure = record.failure.or_else(|| usage.failure.clone());
        }
        if record.http_status != Some(200) {
            record.failure.get_or_insert("upstream HTTP failure".into());
        }
        if Instant::now() >= self.request.deadline {
            record
                .failure
                .get_or_insert("raw usage arrived after deadline".into());
        }
        if route == "/responses" {
            if let Err(reason) = validate_response(&response.body, self.request.tools) {
                record.failure.get_or_insert(reason);
            }
        }
        {
            let mut state = self.state.lock().unwrap();
            state.response_bytes = state.response_bytes.saturating_add(record.response_bytes);
            if state.response_bytes > (4 * MAX_BYTES) as u64 {
                record
                    .failure
                    .get_or_insert("invocation response byte budget exceeded".into());
            }
            if let Some(usage) = record.usage.as_ref().and_then(|u| u.reconciled.as_ref()) {
                match state.inclusive.checked_add(usage.inclusive) {
                    Some(total) => {
                        state.inclusive = total;
                        if total > self.request.max_inclusive_tokens {
                            record
                                .failure
                                .get_or_insert("native output budget overshoot".into());
                        }
                    }
                    None => {
                        record.failure.get_or_insert("native usage overflow".into());
                    }
                }
            }
        }
        let collected = self
            .hooks
            .collect(&record)
            .is_ok_and(|ack| ack.intent_id == record.intent.id);
        let failure = record.failure.clone();
        {
            let mut state = self.state.lock().unwrap();
            state.calls.push(record);
        }
        if !collected {
            return Err("native raw collector acknowledgement refused".into());
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        if Instant::now() >= self.request.deadline {
            return Err("collector acknowledgement after deadline".into());
        }
        stream
            .set_write_timeout(Some(remaining(self.request.deadline)?))
            .map_err(|_| "gateway write timeout")?;
        let content_type = if route == "/responses" {
            "text/event-stream"
        } else {
            "application/json"
        };
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.body.len()).map_err(|_| "gateway reply failed")?;
        stream
            .write_all(&response.body)
            .map_err(|_| "gateway reply failed".into())
    }
}

struct Incoming {
    method: String,
    path: String,
    authorization: String,
    account: Option<String>,
    body: Vec<u8>,
}

fn admitted_personal_account(account: Option<&str>, binding: &str) -> bool {
    account.is_some_and(|account| !account.is_empty() && sha256(account.as_bytes()) == binding)
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| "gateway deadline expired".into())
}

fn read_exact(stream: &mut TcpStream, bytes: &mut [u8], deadline: Instant) -> Result<(), String> {
    let mut offset = 0;
    while offset < bytes.len() {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|_| "gateway read timeout")?;
        let n = stream
            .read(&mut bytes[offset..])
            .map_err(|e| format!("gateway read failed ({:?})", e.kind()))?;
        if n == 0 {
            return Err("truncated gateway request".into());
        }
        offset += n;
    }
    Ok(())
}

fn incoming(
    stream: &mut TcpStream,
    address: SocketAddr,
    secret: &str,
    deadline: Instant,
) -> Result<Incoming, String> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 16384 {
            return Err("gateway header bound".into());
        }
        let mut b = [0];
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|_| "gateway read timeout")?;
        match stream.read(&mut b) {
            Ok(0) if header.is_empty() => return Err("empty gateway connection".into()),
            Err(e)
                if header.is_empty()
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                    ) =>
            {
                return Err("empty gateway connection".into());
            }
            Ok(1) => header.push(b[0]),
            _ => return Err("truncated gateway headers".into()),
        }
    }
    let text = std::str::from_utf8(&header).map_err(|_| "invalid gateway headers")?;
    let mut lines = text.split("\r\n");
    let first: Vec<_> = lines.next().unwrap_or("").split(' ').collect();
    if first.len() != 3 || first[2] != "HTTP/1.1" {
        return Err("invalid gateway request line".into());
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("invalid gateway header")?;
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
        {
            return Err("invalid gateway header".into());
        }
        if headers
            .insert(name.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err("duplicate gateway header".into());
        }
    }
    // No forwarding of user destinations, redirects, hop-by-hop headers or proxy credentials.
    if headers.get("host") != Some(&address.to_string())
        || headers.get("x-af-gateway") != Some(&secret.to_owned())
        || headers.contains_key("upgrade")
        || headers.contains_key("transfer-encoding")
        || headers.contains_key("proxy-authorization")
        || headers
            .get("connection")
            .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"))
    {
        return Err("gateway authentication or framing refused".into());
    }
    let authorization = headers
        .remove("authorization")
        .filter(|v| v.starts_with("Bearer ") && v.len() > 7)
        .ok_or("native authorization absent")?;
    let length = headers
        .get("content-length")
        .map(|v| v.parse::<usize>())
        .transpose()
        .map_err(|_| "invalid gateway length")?
        .unwrap_or(0);
    if length > MAX_BYTES {
        return Err("gateway request body bound".into());
    }
    let mut body = vec![0; length];
    read_exact(stream, &mut body, deadline)?;
    Ok(Incoming {
        method: first[0].into(),
        path: first[1].into(),
        authorization,
        account: headers.remove("chatgpt-account-id"),
        body,
    })
}

pub(super) fn wire_parameters(schema: &Value) -> Value {
    // The pinned native JsonSchema serializer omits numeric/string bounds and patterns.
    // Compare its exact observed projection; shared typed admission enforces every bound.
    let mut schema = schema.clone();
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for property in properties.values_mut() {
            if let Some(object) = property.as_object_mut() {
                object.remove("minimum");
                object.remove("maximum");
                object.remove("minLength");
                object.remove("maxLength");
                object.remove("pattern");
            }
        }
    }
    schema
}

pub(super) fn request_tools(request: &Value) -> Result<&Vec<Value>, String> {
    // Responses Lite registers tools in an additional_tools input item. Ordinary
    // Responses uses a top-level catalog. Never search arbitrary prompt text.
    let embedded: Vec<_> = request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "additional_tools")
        .collect();
    let tools = match (request.get("tools"), embedded.as_slice()) {
        (Some(tools), []) => tools.as_array().ok_or("invalid tool catalog"),
        (None, [item]) => item["tools"]
            .as_array()
            .ok_or("invalid embedded tool catalog"),
        _ => Err("missing or ambiguous tool catalog"),
    }?;
    if tools.len() == 1 && tools[0]["type"] == "namespace" && tools[0]["name"] == "functions" {
        return tools[0]["tools"]
            .as_array()
            .ok_or_else(|| "invalid functions namespace".into());
    }
    Ok(tools)
}

#[cfg(test)]
fn validate_request(body: &[u8], selected: ToolSet) -> Result<(), String> {
    validate_worker_request(body, selected, WorkerProfile::Subject)
}

fn validate_worker_request(
    body: &[u8],
    selected: ToolSet,
    profile: WorkerProfile,
) -> Result<(), String> {
    let request: Value = serde_json::from_slice(body).map_err(|_| "invalid Responses request")?;
    if request["model"] != profile.model()
        || request["stream"] != true
        || request["reasoning"]["effort"] != profile.effort()
    {
        return Err("unbound model or nonstreaming inference".into());
    }
    let tools = request_tools(&request)?;
    let declared = selected.schemas();
    let declared = declared.as_array().unwrap();
    if tools.len() != declared.len() + 1
        || tools
            .iter()
            .filter(|t| t["name"] == "request_user_input")
            .count()
            != 1
        || tools.iter().any(|t| t["type"] != "function")
    {
        return Err("unexpected advertised tool catalog".into());
    }
    for declared in declared {
        let registered = tools
            .iter()
            .find(|t| t["name"] == declared["name"])
            .ok_or("missing navigation registration")?;
        if registered["parameters"] != wire_parameters(&declared["inputSchema"]) {
            return Err("navigation tool schema mismatch".into());
        }
    }
    Ok(())
}

fn validate_response(body: &[u8], selected: ToolSet) -> Result<(), String> {
    let text = std::str::from_utf8(body).map_err(|_| "invalid response UTF8")?;
    let text = text.replace("\r\n", "\n");
    for data in usage::data_frames(&text) {
        if data == "[DONE]" {
            continue;
        }
        let event: Value =
            serde_json::from_str(&data).map_err(|_| "malformed upstream SSE event")?;
        let mut items = Vec::new();
        if let Some(item) = event.get("item") {
            items.push(item);
        }
        if let Some(output) = event.pointer("/response/output").and_then(Value::as_array) {
            items.extend(output);
        }
        for item in items {
            match item["type"].as_str() {
                Some("function_call") => {
                    if !item["name"]
                        .as_str()
                        .is_some_and(|name| selected.admits(name))
                    {
                        return Err("forbidden upstream tool call".into());
                    }
                }
                Some("message" | "reasoning") => {}
                _ => return Err("forbidden upstream native/custom tool".into()),
            }
        }
    }
    Ok(())
}

struct Response {
    status: Option<u16>,
    body: Vec<u8>,
    failure: Option<String>,
    cleanup_confirmed: bool,
}

// Curl owns TLS verification. It receives native auth only on stdin, never argv, disk,
// stderr logging, receipts or environment. -q disables ambient curlrc; no proxy, redirects,
// retries, WebSocket or CONNECT. URLs are constants or loopback fixtures only.
fn forward(upstream: &Upstream, route: &str, incoming: &Incoming, deadline: Instant) -> Response {
    let url = match upstream {
        Upstream::Official => format!("https://chatgpt.com/backend-api/codex{route}"),
        Upstream::Fixture(address) => format!("http://{address}{route}"),
    };
    let quote = |s: &str| {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        )
    };
    let mut config = format!(
        "header = {}\n",
        quote(&format!("Authorization: {}", incoming.authorization))
    );
    if let Some(account) = &incoming.account {
        config.push_str(&format!(
            "header = {}\n",
            quote(&format!("ChatGPT-Account-ID: {account}"))
        ));
    }
    config.push_str("header = \"Content-Type: application/json\"\n");
    if incoming.method == "POST" {
        let Ok(body) = std::str::from_utf8(&incoming.body) else {
            return Response {
                status: None,
                body: vec![],
                failure: Some("invalid upstream request UTF8".into()),
                cleanup_confirmed: true,
            };
        };
        config.push_str(&format!("data-binary = {}\n", quote(body)));
    }
    let Ok(timeout) = remaining(deadline) else {
        return Response {
            status: None,
            body: vec![],
            failure: Some("upstream deadline expired".into()),
            cleanup_confirmed: true,
        };
    };
    let protocol = match upstream {
        Upstream::Official => "=https",
        Upstream::Fixture(_) => "=http",
    };
    let mut command =
        match server::transport_command(std::path::Path::new("/usr/bin/curl"), deadline) {
            Ok(command) => command,
            Err(_) => {
                return Response {
                    status: None,
                    body: vec![],
                    failure: Some("upstream lifecycle enforcement unavailable".into()),
                    cleanup_confirmed: true,
                };
            }
        };
    command.env_clear().args([
        "-q",
        "--silent",
        "--proxy",
        "",
        "--retry",
        "0",
        "--max-redirs",
        "0",
        "--proto",
        protocol,
        "--max-time",
        &timeout.as_secs_f64().to_string(),
        "--max-filesize",
        &MAX_BYTES.to_string(),
        "--write-out",
        "\n%{http_code}",
        "--config",
        "-",
        "--url",
        &url,
    ]);
    let Ok(timeout) = remaining(deadline) else {
        return Response {
            status: None,
            body: vec![],
            failure: Some("upstream deadline expired".into()),
            cleanup_confirmed: true,
        };
    };
    let result = run_supervised_captured_bounded(
        &mut command,
        Some(config.into_bytes()),
        timeout,
        ExitPolicy::KillDescendants,
        None,
        MAX_BYTES + 4,
    );
    let cleanup_confirmed = server::lifecycle_confirmed(
        result.status.as_ref().is_ok_and(|s| s.success())
            && !result.stderr_held
            && !result.truncation.is_truncated(),
    );
    let failure = (!cleanup_confirmed).then(|| "upstream send/drain or cleanup failed".into());
    let mut body = result.stdout;
    let status = (body.len() >= 4 && body[body.len() - 4] == b'\n')
        .then(|| {
            std::str::from_utf8(&body[body.len() - 3..])
                .ok()?
                .parse::<u16>()
                .ok()
        })
        .flatten();
    if status.is_some() {
        body.truncate(body.len() - 4);
    }
    Response {
        // Curl's 000 means that no HTTP status was received, not a provider status.
        status: status.filter(|code| (100..=599).contains(code)),
        body,
        failure,
        cleanup_confirmed,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{hooks, request, terminal, usage_value};
    use super::*;

    fn fixture_accept(listener: &TcpListener) -> (TcpStream, SocketAddr) {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((stream, address)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    return (stream, address);
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("fixture accept failed or expired: {error}"),
            }
        }
    }

    fn exchange(
        gateway: &Gateway<'_>,
        method: &str,
        path: &str,
        secret: &str,
        extra: &str,
        body: &[u8],
    ) -> Result<(), String> {
        std::thread::scope(|scope| {
            let serving = scope.spawn(|| {
                let until = Instant::now() + Duration::from_secs(10);
                let (mut peer, _) = loop {
                    match gateway.listener.accept() {
                        Ok(value) => break value,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < until =>
                        {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => return Err("fixture accept failed".into()),
                    }
                };
                let result = gateway.handle(&mut peer);
                if let Err(reason) = &result {
                    gateway.fail(reason);
                }
                result
            });
            let mut stream = TcpStream::connect(gateway.address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let sent = write!(stream,"{method} {path} HTTP/1.1\r\nHost: {}\r\nX-AF-Gateway: {secret}\r\nAuthorization: Bearer dummy\r\nContent-Length: {}\r\n{extra}\r\n",gateway.address,body.len())
                .and_then(|()| stream.write_all(body));
            let mut reply = vec![];
            let _ = stream.read_to_end(&mut reply);
            let handled = serving.join().unwrap();
            // Expired admission may close the socket before the client finishes writing.
            // Still join the handler and assert its refusal; never mask a successful
            // handler's unexpected client-side failure.
            if handled.is_ok() {
                sent.map_err(|_| "fixture send failed".to_string())?;
            }
            handled
        })
    }
    fn body() -> Vec<u8> {
        request_body(ToolSet::Text)
    }

    fn request_body(selected: ToolSet) -> Vec<u8> {
        let mut tools = selected.schemas();
        for tool in tools.as_array_mut().unwrap() {
            tool["parameters"] = wire_parameters(&tool["inputSchema"]);
            tool.as_object_mut().unwrap().remove("inputSchema");
        }
        tools
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"function","name":"request_user_input","parameters":{}}));
        serde_json::to_vec(
            &json!({"model":MODEL,"stream":true,"reasoning":{"effort":"medium"},"tools":tools}),
        )
        .unwrap()
    }

    #[test]
    fn unknown_registration_schema_changes_and_ambiguous_catalogs_are_refused() {
        let good: Value = serde_json::from_slice(&body()).unwrap();
        assert!(validate_request(&serde_json::to_vec(&good).unwrap(), ToolSet::Text).is_ok());
        for name in [
            "exec_command",
            "apply_patch",
            "web_search",
            "spawn_agent",
            "read_mcp_resource",
            "view_image",
            "computer",
        ] {
            let mut request = good.clone();
            request["tools"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"function", "name":name,"parameters":{}}));
            assert!(
                validate_request(&serde_json::to_vec(&request).unwrap(), ToolSet::Text).is_err()
            );
        }
        let mut altered = good.clone();
        altered["tools"][0]["parameters"]["additionalProperties"] = json!(true);
        assert!(validate_request(&serde_json::to_vec(&altered).unwrap(), ToolSet::Text).is_err());
        let mut ambiguous = good.clone();
        ambiguous["input"] = json!([{"type":"additional_tools","tools":good["tools"]}]);
        assert!(validate_request(&serde_json::to_vec(&ambiguous).unwrap(), ToolSet::Text).is_err());
        let mut fallback = good.clone();
        fallback["model"] = json!("other");
        assert!(validate_request(&serde_json::to_vec(&fallback).unwrap(), ToolSet::Text).is_err());
        let mut effort = good;
        effort["reasoning"]["effort"] = json!("high");
        assert!(validate_request(&serde_json::to_vec(&effort).unwrap(), ToolSet::Text).is_err());
    }

    #[test]
    fn grader_registration_requires_the_terra_high_binding() {
        let mut request: Value = serde_json::from_slice(&body()).unwrap();
        let profile = WorkerProfile::Grader;
        let accepts = |request: &Value| {
            validate_worker_request(
                &serde_json::to_vec(request).unwrap(),
                ToolSet::Text,
                profile,
            )
            .is_ok()
        };
        assert!(!accepts(&request));
        request["model"] = json!("gpt-6.1-terra");
        assert!(!accepts(&request));
        request["reasoning"]["effort"] = json!("high");
        assert!(accepts(&request));
        assert_ne!(profile.config_identity(), config_identity());
        assert!(validate_worker_catalog(&profile.fixture_catalog(), profile).is_ok());
        assert!(validate_catalog(&profile.fixture_catalog()).is_err());
    }

    #[test]
    fn semantic_registration_and_sse_calls_require_the_treatment_tool_set() {
        let baseline = request_body(ToolSet::Text);
        let treatment = request_body(ToolSet::Semantic);
        assert!(validate_request(&baseline, ToolSet::Semantic).is_err());
        assert!(validate_request(&treatment, ToolSet::Text).is_err());
        assert!(validate_request(&treatment, ToolSet::Semantic).is_ok());
        for (operation, _) in SEMANTIC_OPERATIONS {
            let stream = multiline_tool_stream(operation.name());
            assert!(validate_response(&stream, ToolSet::Text).is_err());
            assert!(validate_response(&stream, ToolSet::Semantic).is_ok());
        }
    }

    #[test]
    fn gateway_rejects_routes_auth_smuggling_and_upgrade_before_guard() {
        for (method, path, extra, bad_secret) in [
            ("CONNECT", "chatgpt.com:443", "", false),
            ("GET", "http://evil.invalid/models", "", false),
            ("POST", "/responses/compact", "", false),
            ("POST", "/responses?url=evil", "", false),
            ("POST", "/v1/../responses", "", false),
            ("POST", "/responses", "Upgrade: websocket\r\n", false),
            (
                "POST",
                "/responses",
                "Transfer-Encoding: chunked\r\n",
                false,
            ),
            (
                "POST",
                "/responses",
                "Authorization: Bearer duplicate\r\n",
                false,
            ),
            ("POST", "/responses", "Host: evil.invalid\r\n", false),
            ("POST", "/responses", "", true),
        ] {
            let home = tempfile::tempdir().unwrap();
            let request = request(home.path(), false);
            let hooks = hooks();
            let gateway =
                Gateway::new(&request, &hooks, Upstream::Official, Instant::now()).unwrap();
            let secret = if bad_secret { "wrong" } else { &gateway.secret };
            assert!(
                exchange(&gateway, method, path, secret, extra, &body()).is_err(),
                "{method} {path}"
            );
            assert!(hooks.order.lock().unwrap().is_empty());
            assert!(gateway.state.lock().unwrap().calls.is_empty());
        }
    }

    #[test]
    fn native_guard_and_exact_ack_are_required_before_any_upstream_send() {
        for bad_ack in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let mut request = request(home.path(), false);
            request.bindings.account = sha256(b"fixture-account");
            let mut hooks = hooks();
            hooks.refuse = !bad_ack;
            hooks.bad_ack = bad_ack;
            let gateway =
                Gateway::new(&request, &hooks, Upstream::Official, Instant::now()).unwrap();
            assert!(
                exchange(
                    &gateway,
                    "POST",
                    "/responses",
                    &gateway.secret,
                    "ChatGPT-Account-ID: fixture-account\r\n",
                    &body()
                )
                .is_err()
            );
            assert_eq!(*hooks.order.lock().unwrap(), ["guard:1"]);
            assert!(gateway.state.lock().unwrap().calls.is_empty());
        }
    }

    #[test]
    fn personal_account_requires_the_explicit_native_header_pin() {
        let binding = sha256(b"fixture-account");
        assert!(admitted_personal_account(Some("fixture-account"), &binding));
        assert!(!admitted_personal_account(Some("other-account"), &binding));
        assert!(!admitted_personal_account(None, &binding));
        assert!(!admitted_personal_account(
            Some("fixture-account"),
            FIXTURE_ACCOUNT
        ));
    }

    fn multiline_tool_stream(tool: &str) -> Vec<u8> {
        let event = json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "id": "fc-multiline",
                "call_id": "call-multiline",
                "name": tool,
                "arguments": "{}"
            }
        });
        let data = serde_json::to_string_pretty(&event)
            .unwrap()
            .lines()
            .map(|line| format!("data: {line}\n"))
            .collect::<String>();
        let mut raw = format!(": fixture comment\n{data}\n").into_bytes();
        raw.extend(terminal(Some(usage_value())));
        raw
    }

    #[test]
    fn multiline_sse_tools_use_the_same_framing_as_raw_usage() {
        for newline in ["\n", "\r\n"] {
            for tool in [
                "af_list_paths",
                "af_read_range",
                "af_search",
                "exec_command",
                "apply_patch",
                "spawn_agent",
                "request_user_input",
            ] {
                let raw = String::from_utf8(multiline_tool_stream(tool))
                    .unwrap()
                    .replace('\n', newline);
                assert!(usage::capture(raw.as_bytes()).failure.is_none());
                assert_eq!(
                    validate_response(raw.as_bytes(), ToolSet::Text).is_ok(),
                    tool.starts_with("af_"),
                    "{tool}"
                );
            }
        }
    }

    #[test]
    fn unknown_usage_or_tools_stop_retry_and_preserve_known_usage() {
        for (raw, cap) in [
            (terminal(None), 1000),
            (terminal(Some(usage_value())), 1),
            (multiline_tool_stream("exec_command"), 1000),
        ] {
            let home = tempfile::tempdir().unwrap();
            let mut request = request(home.path(), false);
            request.max_inclusive_tokens = cap;
            let hooks = hooks();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            std::thread::scope(|scope| {
                let upstream = scope.spawn(|| {
                    let (mut stream, _) = fixture_accept(&listener);
                    let _ = super::super::tests::fixture_http(&mut stream);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        raw.len()
                    )
                    .unwrap();
                    stream.write_all(&raw).unwrap();
                });
                let gateway =
                    Gateway::new(&request, &hooks, Upstream::Fixture(address), Instant::now())
                        .unwrap();
                assert!(
                    exchange(&gateway, "POST", "/responses", &gateway.secret, "", &body()).is_err()
                );
                upstream.join().unwrap();
                // An explicit retry is a new HTTP request; unknown or overshot usage
                // prevents the guard and therefore prevents any second upstream send.
                assert!(
                    exchange(&gateway, "POST", "/responses", &gateway.secret, "", &body()).is_err()
                );
                let state = gateway.state.lock().unwrap();
                assert_eq!(state.calls.len(), 1);
                assert_eq!(*hooks.order.lock().unwrap(), ["guard:1", "collect:1"]);
                let usage = state.calls[0].usage.as_ref().unwrap();
                assert!(state.calls[0].failure.is_some());
                if usage.value == RawField::Absent {
                    assert!(usage.reconciled.is_none());
                } else {
                    assert_eq!(usage.reconciled.as_ref().unwrap().chargeable, 90);
                }
            });
        }
    }

    #[test]
    fn redirects_and_collector_refusals_preserve_usage_without_a_second_send() {
        struct RejectCollector<'a>(&'a super::super::tests::Hooks);
        impl NativeHooks for RejectCollector<'_> {
            fn before_send(
                &self,
                intent: &CallIntent,
                deadline: Instant,
            ) -> Result<DurableAck, String> {
                self.0.before_send(intent, deadline)
            }
            fn collect(&self, record: &RawCallRecord) -> Result<DurableAck, String> {
                self.0.collect(record)?;
                Err("collector failed".into())
            }
        }
        for status in [200, 302] {
            let home = tempfile::tempdir().unwrap();
            let request = request(home.path(), false);
            let base = hooks();
            let rejecting = RejectCollector(&base);
            let native: &dyn NativeHooks = if status == 200 { &rejecting } else { &base };
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            std::thread::scope(|scope| {
                let upstream = scope.spawn(|| {
                    let (mut stream, _) = fixture_accept(&listener); let _ = super::super::tests::fixture_http(&mut stream);
                    let raw = terminal(Some(usage_value()));
                    write!(stream, "HTTP/1.1 {status} Fixture\r\nLocation: http://{address}/evil\r\nContent-Length: {}\r\n\r\n", raw.len()).unwrap(); stream.write_all(&raw).unwrap();
                });
                let gateway =
                    Gateway::new(&request, native, Upstream::Fixture(address), Instant::now())
                        .unwrap();
                assert!(
                    exchange(&gateway, "POST", "/responses", &gateway.secret, "", &body()).is_err()
                );
                upstream.join().unwrap();
                assert!(
                    exchange(&gateway, "POST", "/responses", &gateway.secret, "", &body()).is_err()
                );
                let state = gateway.state.lock().unwrap();
                assert_eq!(state.calls.len(), 1);
                assert_eq!(state.calls[0].http_status, Some(status));
                assert_eq!(
                    state.calls[0]
                        .usage
                        .as_ref()
                        .unwrap()
                        .reconciled
                        .as_ref()
                        .unwrap()
                        .chargeable,
                    90
                );
                assert_eq!(*base.order.lock().unwrap(), ["guard:1", "collect:1"]);
            });
        }
    }

    #[test]
    fn accepted_nonblocking_sockets_allow_fragmented_native_requests() {
        let home = tempfile::tempdir().unwrap();
        let mut request = request(home.path(), false);
        request.bindings.account = sha256(b"inert-fixture");
        let mut hooks = hooks();
        hooks.refuse = true;
        let gateway = Gateway::new(&request, &hooks, Upstream::Official, Instant::now()).unwrap();
        std::thread::scope(|scope| {
            let serving = scope.spawn(|| {
                let (mut peer, _) = loop {
                    match gateway.listener.accept() {
                        Ok(peer) => break peer,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1))
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                peer.set_nonblocking(true).unwrap();
                gateway.handle(&mut peer)
            });
            let mut client = TcpStream::connect(gateway.address).unwrap();
            client.write_all(b"POST /responses HTTP/1.1\r\n").unwrap();
            std::thread::sleep(Duration::from_millis(20));
            let body = body();
            write!(client, "Host: {}\r\nX-AF-Gateway: {}\r\nAuthorization: Bearer dummy\r\nChatGPT-Account-ID: inert-fixture\r\nContent-Length: {}\r\n\r\n", gateway.address, gateway.secret, body.len()).unwrap();
            client.write_all(&body).unwrap();
            assert_eq!(
                serving.join().unwrap().unwrap_err(),
                "native call guard refused"
            );
        });
        assert_eq!(*hooks.order.lock().unwrap(), ["guard:1"]);
    }

    #[test]
    fn metadata_is_retained_without_inventing_inference_usage() {
        let home = tempfile::tempdir().unwrap();
        let request = request(home.path(), false);
        let hooks = hooks();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway = Gateway::new(
            &request,
            &hooks,
            Upstream::Fixture(listener.local_addr().unwrap()),
            Instant::now(),
        )
        .unwrap();
        std::thread::scope(|scope| {
            let serving = scope.spawn(|| {
                let (mut stream, _) = fixture_accept(&listener);
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut bytes = vec![];
                while !bytes.ends_with(b"\r\n\r\n") {
                    assert!(bytes.len() < 16384);
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                assert!(bytes.starts_with(b"GET /models HTTP/1.1\r\n"));
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .unwrap();
            });
            exchange(&gateway, "GET", "/models", &gateway.secret, "", b"").unwrap();
            serving.join().unwrap();
        });
        let state = gateway.state.lock().unwrap();
        assert_eq!(state.calls.len(), 1);
        assert_eq!(state.calls[0].intent.route, "/models");
        assert!(state.calls[0].usage.is_none());
        assert!(state.calls[0].failure.is_none());
        assert_eq!(state.inclusive, 0); // no inference, rather than a measured zero-token call
        assert!(state.helpers_cleanup_confirmed);
        assert_eq!(*hooks.order.lock().unwrap(), ["guard:1", "collect:1"]);
    }

    #[test]
    fn expired_deadline_cannot_reach_guard() {
        let home = tempfile::tempdir().unwrap();
        let mut request = request(home.path(), false);
        request.deadline = Instant::now();
        let hooks = hooks();
        let gateway = Gateway::new(&request, &hooks, Upstream::Official, Instant::now()).unwrap();
        assert!(exchange(&gateway, "POST", "/responses", &gateway.secret, "", &body()).is_err());
        assert!(hooks.order.lock().unwrap().is_empty());
        assert!(gateway.state.lock().unwrap().calls.is_empty());
    }
}
