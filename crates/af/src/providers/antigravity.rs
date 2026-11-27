//! Antigravity (`agy`) local ACP stdio probes (token-free capability, status, and model catalog).
//!
//! `agy acp --stdio` speaks newline-delimited JSON-RPC 2.0 over stdin/stdout:
//! - `initialize` proves the CLI serves the ACP protocol without touching credentials or tokens.
//! - `session/new` checks authentication (`-32000` / `"Authentication required"` when logged out)
//!   and, when authenticated, returns the available model catalog, effort levels (`thought_level`),
//!   and optional subscription tier metadata (`_meta.antigravity.tier.name`), all without running
//!   any model turn.

use super::compatibility::{CliRefusal, DeniedProbe, protocol_incompatibility};
use super::*;
use af_runner::models::{self as rules, Catalog, CatalogModel, diagnostic as hint};

/// The protocol capability Antigravity probes need of the installed `agy` CLI.
pub(super) const ACP_PROTOCOL: &str =
    "`agy acp --stdio` answering JSON-RPC `initialize` over stdio";

/// The most models an Antigravity catalog keeps; the rest are counted as skipped.
const MAX_CATALOG_MODELS: usize = 64;

/// What one `agy acp --stdio` session probe observed.
pub(super) struct AcpSessionResult {
    pub(super) authenticated: bool,
    pub(super) version: Option<String>,
    pub(super) tier: Option<String>,
    pub(super) models_result: Option<serde_json::Value>,
}

/// Token-free capability check for `agy acp --stdio`: starts the ACP server in a private scratch
/// probe directory (never mutating the operator's auth directory), sends `initialize`, verifies
/// that `result` is a JSON object, and terminates the child.
pub(super) fn check_acp_server(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
) -> Result<(), CliRefusal> {
    let mut observed = AppServerProbe::default();
    match probe_acp_observed(
        program,
        spec,
        probe_path,
        cancelled,
        false,
        attempt_deadline,
        None,
        &mut observed,
    ) {
        Ok(_) => Ok(()),
        Err(error)
            if !observed.answered
                && super::compatibility::permission_denied(&error)
                && (error.starts_with("cannot start Antigravity ACP probe:")
                    || (error.starts_with("agy acp exited")
                        && error.contains("before answering initialize"))) =>
        {
            Err(CliRefusal::Denied(DeniedProbe::new(spec, error)))
        }
        Err(error) if observed.lacks_protocol => Err(protocol_incompatibility(
            program,
            spec,
            probe_path,
            cancelled,
            attempt_deadline,
            &error,
        )),
        Err(error) => Err(CliRefusal::Unchecked(format!(
            "provider {}: af could not check that the {} CLI `{}` serves {ACP_PROTOCOL}: {error}",
            spec.id,
            spec.kind.name(),
            program.display()
        ))),
    }
}

/// Probe authentication, subscription tier, and model catalog via `agy acp --stdio` (`initialize`
/// followed by `session/new`). Token-free and bounded by `PROBE_TIMEOUT`, `cap`, and
/// `attempt_deadline`.
pub(super) fn probe_acp_session(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    attempt_deadline: Option<Instant>,
    cap: Option<Duration>,
    observed: &mut AppServerProbe,
) -> Result<AcpSessionResult, String> {
    let (init_response, session_response) = probe_acp_observed(
        program,
        spec,
        probe_path,
        cancelled,
        true,
        attempt_deadline,
        cap,
        observed,
    )?;
    let version = init_response
        .get("result")
        .and_then(|r| r.get("agentInfo"))
        .and_then(|a| a.get("version"))
        .and_then(serde_json::Value::as_str)
        .filter(|v| hint::printable(v))
        .map(str::to_owned);
    let session = session_response.ok_or_else(|| {
        "agy acp exited after initialize without answering `session/new`".to_string()
    })?;
    if let Some(err) = session.get("error") {
        let code = err.get("code").and_then(serde_json::Value::as_i64);
        let msg = err
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let lower = msg.to_ascii_lowercase();
        if code == Some(-32000)
            || lower.contains("authentication required")
            || lower.contains("not authenticated")
            || lower.contains("unauthenticated")
            || lower.contains("login required")
        {
            return Ok(AcpSessionResult {
                authenticated: false,
                version,
                tier: None,
                models_result: None,
            });
        }
        let line = af_runner::cli::bounded_cli_line(msg);
        return Err(format!("agy acp rejected `session/new`: {line}"));
    }
    let result = session
        .get("result")
        .filter(|v| v.is_object())
        .cloned()
        .ok_or_else(|| {
            observed.lacks_protocol = true;
            "agy acp answered `session/new` without a result object".to_string()
        })?;
    let tier = result
        .get("_meta")
        .and_then(|m| m.get("antigravity"))
        .and_then(|a| a.get("tier"))
        .and_then(|t| t.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(|name| safe_display(name, ""))
        .filter(|name| !name.is_empty());
    Ok(AcpSessionResult {
        authenticated: true,
        version,
        tier,
        models_result: Some(result),
    })
}

/// Convert an `agy acp` `session/new` result object into a bounded [`Catalog`].
pub(crate) fn parse_acp_models(result: &serde_json::Value) -> Result<(Catalog, usize), String> {
    let available = result
        .get("models")
        .and_then(|m| m.get("availableModels").or(Some(m)))
        .and_then(serde_json::Value::as_array)
        .ok_or("agy acp answered `session/new` without an availableModels list")?;

    let (thought_efforts, default_effort) = extract_thought_levels(result);
    let fallback_efforts: Vec<String> = rules::ANTIGRAVITY_EFFORTS
        .iter()
        .map(|level| (*level).to_owned())
        .collect();
    let efforts = if thought_efforts.is_empty() {
        fallback_efforts
    } else {
        thought_efforts
    };

    let mut catalog = Catalog::default();
    let mut skipped = 0;
    for entry in available {
        let Some(id) = entry
            .get("modelId")
            .or_else(|| entry.get("id"))
            .and_then(serde_json::Value::as_str)
            .filter(|id| hint::printable(id))
        else {
            skipped += 1;
            continue;
        };
        if catalog.model(id).is_some() {
            continue;
        }
        if catalog.models.len() == MAX_CATALOG_MODELS {
            skipped += 1;
            continue;
        }
        let display = entry
            .get("name")
            .or_else(|| entry.get("displayName"))
            .and_then(serde_json::Value::as_str)
            .map(|name| safe_display(name, id))
            .unwrap_or_else(|| id.to_owned());
        catalog.models.push(CatalogModel {
            id: id.to_owned(),
            display_name: display,
            other_names: Vec::new(),
            aliases: Vec::new(),
            efforts: efforts.clone(),
            default_effort: default_effort.clone(),
            hidden: false,
        });
    }
    Ok((catalog, skipped))
}

fn extract_thought_levels(result: &serde_json::Value) -> (Vec<String>, Option<String>) {
    let Some(options) = result
        .get("configOptions")
        .and_then(serde_json::Value::as_array)
    else {
        return (Vec::new(), None);
    };
    let Some(thought) = options
        .iter()
        .find(|opt| opt.get("id").and_then(serde_json::Value::as_str) == Some("thought_level"))
    else {
        return (Vec::new(), None);
    };
    let mut levels = Vec::new();
    if let Some(choices) = thought.get("options").and_then(serde_json::Value::as_array) {
        for choice in choices {
            if let Some(level) = choice
                .get("value")
                .and_then(serde_json::Value::as_str)
                .filter(|v| hint::printable(v))
                && !levels.iter().any(|existing| existing == level)
            {
                levels.push(level.to_owned());
            }
        }
    }
    let default = thought
        .get("currentValue")
        .and_then(serde_json::Value::as_str)
        .filter(|v| levels.iter().any(|listed| listed == v))
        .map(str::to_owned);
    (levels, default)
}

#[allow(clippy::too_many_arguments)]
fn probe_acp_observed(
    program: &Path,
    spec: &ProviderSpec,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
    send_session_new: bool,
    attempt_deadline: Option<Instant>,
    cap: Option<Duration>,
    observed: &mut AppServerProbe,
) -> Result<(serde_json::Value, Option<serde_json::Value>), String> {
    availability::check(spec)?;
    check_task_probe_control(attempt_deadline, cancelled)?;
    let directory = ProbeDirectory::new(spec)?;
    let mut command = Command::new(program);
    command.args(["acp", "--stdio", "--disable-telemetry"]);
    // For the initialize-only capability probe, point `--gemini-dir` at the private scratch probe
    // directory so capability checks never create state inside an unprobed auth_dir. For
    // `session/new`, pass the context's actual auth_dir so `agy` inspects that exact login.
    let gemini_dir = if send_session_new {
        spec.auth_dir
            .clone()
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|h| h.is_absolute())
                    .map(|h| h.join(".gemini").join("antigravity-cli"))
            })
            .unwrap_or_else(|| directory.path().to_path_buf())
    } else {
        directory.path().to_path_buf()
    };
    command
        .arg("--gemini-dir")
        .arg(&gemini_dir)
        .args(["--app-data-dir", "."]);
    configure_probe_environment(&mut command, spec, probe_path, directory.path());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);

    let mut child = af_process::spawn(&mut command)
        .map_err(|error| format!("cannot start Antigravity ACP probe: {error}"))?;
    let mut watch = af_process::ExitWatch::new(child.id());
    let mut reaped = false;
    let mut stdin = child.stdin.take().expect("provider stdin was piped");
    let mut stdout = child.stdout.take().expect("provider stdout was piped");
    let mut stderr = child.stderr.take().expect("provider stderr was piped");
    if let Err(error) = set_nonblocking(&stdout).and_then(|()| set_nonblocking(&stderr)) {
        stop_probe(&mut child);
        return Err(error);
    }

    let initialize = writeln!(
        stdin,
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":1,\"clientCapabilities\":{{}}}}}}"
    )
    .and_then(|()| stdin.flush());

    let timeout = probe_timeout(PROBE_TIMEOUT);
    let timeout = cap.map_or(timeout, |cap| timeout.min(cap));
    let deadline = Instant::now() + timeout;
    let deadline = attempt_deadline.map_or(deadline, |limit| deadline.min(limit));
    let mut captured = Vec::with_capacity(MAX_PROBE_OUTPUT.min(4096));
    let mut said = Vec::new();
    let mut exceeded = false;
    let mut request_sent: Option<usize> = None;
    let mut init_response: Option<serde_json::Value> = None;
    let session_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {
            "cwd": directory.path().display().to_string(),
            "mcpServers": []
        }
    });

    let drain_stderr = |stderr: &mut std::process::ChildStderr, said: &mut Vec<u8>| {
        let mut chunk = [0_u8; 8192];
        while let Ok(count @ 1..) = stderr.read(&mut chunk) {
            let room = MAX_APP_SERVER_STDERR.saturating_sub(said.len());
            said.extend_from_slice(&chunk[..count.min(room)]);
        }
    };

    let result = 'probe: loop {
        loop {
            match drain_available(&mut stdout, &mut captured, &mut exceeded) {
                Ok(true) if !exceeded => {}
                Ok(_) => break,
                Err(error) => break 'probe Err(error),
            }
        }
        drain_stderr(&mut stderr, &mut said);
        if exceeded {
            break Err(format!(
                "Antigravity ACP probe output exceeds {MAX_PROBE_OUTPUT} bytes"
            ));
        }
        if request_sent.is_none()
            && let Some(answer) = initialize_answer(&captured)
        {
            let response = match answer {
                InitializeAnswer::Result(response) => response,
                InitializeAnswer::Refused { how, line } => {
                    observed.lacks_protocol = true;
                    let exit = outcome_after_refusal(
                        &mut child,
                        &mut watch,
                        &mut reaped,
                        Instant::now() + REFUSED_ANSWER_EXIT_GRACE.min(timeout),
                        deadline,
                    );
                    break Err(format!(
                        "agy acp {exit} after answering initialize {how}: {line}"
                    ));
                }
            };
            observed.answered = true;
            if !send_session_new {
                break Ok((response, None));
            }
            init_response = Some(response);
            if let Err(error) = writeln!(stdin, "{session_request}").and_then(|()| stdin.flush()) {
                break Err(format!(
                    "cannot send `session/new` to the Antigravity ACP server: {error}"
                ));
            }
            request_sent = Some(captured.len());
        }
        if let Some(sent) = request_sent
            && let Some(answer) = awaited_response(&captured, 2, sent)
        {
            match answer {
                Awaited::Response(response, _) => {
                    break Ok((
                        init_response.take().unwrap_or(serde_json::Value::Null),
                        Some(response),
                    ));
                }
                Awaited::Malformed(line) => {
                    observed.lacks_protocol = true;
                    let exit = outcome_after_refusal(
                        &mut child,
                        &mut watch,
                        &mut reaped,
                        Instant::now() + REFUSED_ANSWER_EXIT_GRACE.min(timeout),
                        deadline,
                    );
                    break Err(format!(
                        "agy acp {exit} after answering initialize, before answering `session/new`, with a line that is not a JSON-RPC message: {}",
                        observed_line(line)
                    ));
                }
            }
        }
        match af_process::try_reap_killing_group(&mut child, &mut watch) {
            Ok(Some(status)) => {
                reaped = true;
                while let Ok(true) = drain_available(&mut stdout, &mut captured, &mut exceeded) {}
                drain_stderr(&mut stderr, &mut said);
                let exit = af_runner::cli::exit_description(&status);
                if let Some(sent) = request_sent
                    && let Some(answer) = awaited_response(&captured, 2, sent)
                {
                    match answer {
                        Awaited::Response(response, _) => {
                            break Ok((
                                init_response.take().unwrap_or(serde_json::Value::Null),
                                Some(response),
                            ));
                        }
                        Awaited::Malformed(line) => {
                            observed.lacks_protocol = true;
                            break Err(format!(
                                "agy acp {exit} after answering initialize, before answering `session/new`, with a line that is not a JSON-RPC message: {}",
                                observed_line(line)
                            ));
                        }
                    }
                }
                let answer = if request_sent.is_some() {
                    None
                } else {
                    initialize_answer(&captured)
                };
                match answer {
                    Some(InitializeAnswer::Result(response)) if !send_session_new => {
                        observed.answered = true;
                        break Ok((response, None));
                    }
                    Some(InitializeAnswer::Result(_)) => observed.answered = true,
                    Some(InitializeAnswer::Refused { how, line }) => {
                        observed.lacks_protocol = true;
                        break Err(format!(
                            "agy acp {exit} after answering initialize {how}: {line}"
                        ));
                    }
                    None => {}
                }
                if observed.answered {
                    let (line, _) = af_runner::cli::error_and_fix(&said, b"");
                    let line = line.map(|line| format!(": {line}")).unwrap_or_default();
                    break Err(format!(
                        "agy acp {exit} after initialize without answering `session/new`{line}"
                    ));
                }
                let (line, _) = af_runner::cli::error_and_fix(&said, &captured);
                let line = line.map(|line| format!(": {line}")).unwrap_or_default();
                observed.lacks_protocol = true;
                break Err(format!("agy acp {exit} before answering initialize{line}"));
            }
            Ok(None) => {}
            Err(error) => {
                break Err(format!("Antigravity ACP probe failed: {error}"));
            }
        }
        if let Err(error) = &initialize
            && request_sent.is_none()
            && Instant::now() >= deadline
        {
            break Err(format!("cannot initialize Antigravity ACP probe: {error}"));
        }
        if cancelled.load(Ordering::Acquire) {
            break Err("provider status refresh cancelled".to_string());
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "Antigravity ACP probe timed out after {} seconds",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };

    if !reaped {
        stop_probe(&mut child);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_acp_models_reads_available_models_and_thought_levels() {
        let payload = serde_json::json!({
            "sessionId": "sess-1",
            "models": {
                "availableModels": [
                    { "modelId": "gemini-4-neon-high", "name": "Gemini 4 Neon (High)" },
                    { "modelId": "claude-sonnet-4-6", "name": "Claude Sonnet 4.6" },
                    { "modelId": "bad id with space", "name": "Ignored" }
                ]
            },
            "configOptions": [
                {
                    "id": "thought_level",
                    "currentValue": "high",
                    "options": [
                        { "value": "low" },
                        { "value": "medium" },
                        { "value": "high" },
                        { "value": "xhigh" }
                    ]
                }
            ]
        });
        let (catalog, skipped) = parse_acp_models(&payload).unwrap();
        assert_eq!(skipped, 1);
        assert_eq!(catalog.models.len(), 2);
        let neon = catalog.model("gemini-4-neon-high").unwrap();
        assert_eq!(neon.display_name, "Gemini 4 Neon (High)");
        assert_eq!(neon.efforts, ["low", "medium", "high", "xhigh"]);
        assert_eq!(neon.default_effort.as_deref(), Some("high"));
    }
}
