//! Private, bounded transport to the official CLIs' dedicated login operations.
//!
//! No provider-owned bytes become diagnostics. Challenge material exists only in memory and
//! is passed to the private presentation host, never to the invoking command's streams.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

use super::super::{
    BoundDirectoryLock, ProviderKind, ProviderSpec, sanitized_path, set_nonblocking, stop_probe,
    validate_private_auth_directory,
};

const MAX_OUTPUT: usize = 64 * 1024;
const MAX_URL: usize = 12 * 1024;
const MAX_CODE: usize = 4096;
const CANCEL_GRACE: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    DeviceCode,
    CodeOrCallback,
}

pub(super) struct Challenge {
    pub mode: Mode,
    pub url: String,
    pub user_code: Option<String>,
}

pub(super) enum Event {
    Challenge(Challenge),
    Completed,
}

/// A closed vocabulary deliberately unable to carry Provider output or process errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Unsupported,
    InvalidChallenge,
    Failed,
    OutputLimit,
}

pub(super) struct NativeLogin {
    child: Child,
    watch: review_process::ExitWatch,
    stdin: Option<ChildStdin>,
    guarded: bool,
    stdout: ChildStdout,
    protocol: Protocol,
    input: Vec<u8>,
    input_written: usize,
    output: Vec<u8>,
    output_bytes: usize,
    exit: Option<ExitStatus>,
    stopped: bool,
    terminal: bool,
}

enum Protocol {
    Codex(Codex),
    Claude(Claude),
}

impl NativeLogin {
    pub(super) fn start(spec: &ProviderSpec, lock: &BoundDirectoryLock) -> Result<Self, Failure> {
        let auth_dir = spec.auth_dir.as_deref().ok_or(Failure::Unsupported)?;
        if !spec.explicit_selector || !auth_dir.is_absolute() {
            return Err(Failure::Unsupported);
        }
        validate_private_auth_directory(auth_dir).map_err(|_| Failure::Failed)?;
        lock.ensure_directory_current(auth_dir, "auth directory")
            .map_err(|_| Failure::Failed)?;
        let mut command = Command::new(std::env::current_exe().map_err(|_| Failure::Failed)?);
        super::configure_private_login_environment(&mut command, spec, &sanitized_path());
        command
            .args([
                "provider",
                "auth",
                "supervise",
                "--kind",
                spec.kind.name(),
                "--auth-dir",
            ])
            .arg(auth_dir)
            // Stdio transfers the already-locked OFD through the safe spawn path. No
            // ambient descriptor is ever made inheritable, even temporarily.
            .stderr(Stdio::from(
                lock._lock.try_clone().map_err(|_| Failure::Failed)?,
            ));
        Self::spawn_owned(&mut command, spec.kind, true)
    }

    #[cfg(test)]
    fn spawn(command: &mut Command, kind: ProviderKind) -> Result<Self, Failure> {
        Self::spawn_owned(command, kind, false)
    }

    fn spawn_owned(
        command: &mut Command,
        kind: ProviderKind,
        guarded: bool,
    ) -> Result<Self, Failure> {
        if !guarded {
            command.stderr(Stdio::null());
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .process_group(0);
        let mut child = review_process::spawn(command).map_err(|_| Failure::Failed)?;
        let watch = review_process::ExitWatch::new(child.id());
        let stdin = child.stdin.take().expect("login stdin was piped");
        let stdout = child.stdout.take().expect("login stdout was piped");
        if set_nonblocking(&stdin)
            .and_then(|()| set_nonblocking(&stdout))
            .is_err()
        {
            if guarded {
                drop(stdin);
                let _ = child.wait();
            } else {
                stop_probe(&mut child);
            }
            return Err(Failure::Failed);
        }
        let mut login = Self {
            child,
            watch,
            stdin: Some(stdin),
            guarded,
            stdout,
            protocol: match kind {
                ProviderKind::Codex => Protocol::Codex(Codex::Initializing),
                ProviderKind::Claude => Protocol::Claude(Claude::default()),
            },
            input: Vec::new(),
            input_written: 0,
            output: Vec::new(),
            output_bytes: 0,
            exit: None,
            stopped: false,
            terminal: false,
        };
        if kind == ProviderKind::Codex {
            login.queue_json(&serde_json::json!({
                "id": 1,
                "method": "initialize",
                "params": { "clientInfo": {
                    "name": "afactory", "version": env!("CARGO_PKG_VERSION")
                }}
            }));
        }
        Ok(login)
    }

    pub(super) fn poll(&mut self) -> Result<Option<Event>, Failure> {
        if self.terminal {
            return Err(Failure::Failed);
        }
        let result = self.poll_inner();
        if result.is_err() || matches!(result, Ok(Some(Event::Completed))) {
            self.terminal = true;
            self.stop();
        }
        result
    }

    fn poll_inner(&mut self) -> Result<Option<Event>, Failure> {
        self.flush_input()?;
        self.read_output()?;
        if self.exit.is_none() {
            self.exit = review_process::try_reap_killing_group(&mut self.child, &mut self.watch)
                .map_err(|_| Failure::Failed)?;
            if self.exit.is_some() {
                self.stopped = true;
                // Collect bytes written immediately before exit, after descendants were ended.
                self.read_output()?;
            }
        }
        if self.exit.is_some_and(|status| !status.success()) {
            return Err(Failure::Failed);
        }
        if let Protocol::Claude(protocol) = &mut self.protocol {
            return protocol.consume(&mut self.output, self.exit);
        }
        while let Some(end) = self.output.iter().position(|byte| *byte == b'\n') {
            let line: Vec<_> = self.output.drain(..=end).collect();
            let line = line.strip_suffix(b"\n").unwrap_or(&line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let parsed = match &mut self.protocol {
                Protocol::Codex(protocol) => protocol.line(line)?,
                Protocol::Claude(_) => unreachable!("Claude consumes its dedicated text stream"),
            };
            match parsed {
                Parsed::None => {}
                Parsed::StartDevice => {
                    self.queue_json(&serde_json::json!({"method":"initialized","params":{}}));
                    self.queue_json(&serde_json::json!({
                        "id":2, "method":"account/login/start", "params":{"type":"chatgptDeviceCode"}
                    }));
                    self.flush_input()?;
                }
                Parsed::Event(event) => return Ok(Some(event)),
            }
        }
        if self.exit.is_some() {
            return Err(Failure::Failed);
        }
        Ok(None)
    }

    pub(super) fn submit_code(&mut self, code: &str) -> Result<(), Failure> {
        if self.terminal
            || code.is_empty()
            || code.len() > MAX_CODE
            || code.chars().any(char::is_control)
        {
            return Err(Failure::InvalidChallenge);
        }
        match &mut self.protocol {
            Protocol::Claude(protocol) if protocol.can_submit() => {
                protocol.submitted = true;
            }
            _ => return Err(Failure::InvalidChallenge),
        }
        self.input.extend_from_slice(code.as_bytes());
        self.input.push(b'\n');
        if let Err(error) = self.flush_input() {
            self.terminal = true;
            self.stop();
            return Err(error);
        }
        Ok(())
    }

    fn queue_json(&mut self, value: &Value) {
        // These values only contain bounded native IDs and af-authored protocol requests.
        serde_json::to_writer(&mut self.input, value)
            .expect("writing JSON into memory cannot fail");
        self.input.push(b'\n');
    }

    fn flush_input(&mut self) -> Result<(), Failure> {
        while self.input_written < self.input.len() {
            match self
                .stdin
                .as_mut()
                .ok_or(Failure::Failed)?
                .write(&self.input[self.input_written..])
            {
                Ok(0) => return Err(Failure::Failed),
                Ok(count) => self.input_written += count,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(Failure::Failed),
            }
        }
        self.input.fill(0);
        self.input.clear();
        self.input_written = 0;
        Ok(())
    }

    fn read_output(&mut self) -> Result<(), Failure> {
        let mut chunk = [0_u8; 4096];
        loop {
            match self.stdout.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(count) => {
                    self.output_bytes = self.output_bytes.saturating_add(count);
                    if self.output_bytes > MAX_OUTPUT {
                        return Err(Failure::OutputLimit);
                    }
                    self.output.extend_from_slice(&chunk[..count]);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(Failure::Failed),
            }
        }
    }

    fn wait_for_cancel(&mut self) {
        let deadline = Instant::now() + CANCEL_GRACE;
        while Instant::now() < deadline {
            if self.flush_input().is_err() || self.read_output().is_err() {
                return;
            }
            if let Some(response) = super::super::response_for_id(&self.output, 3)
                && matches!(
                    response
                        .get("result")
                        .and_then(|result| result.get("status"))
                        .and_then(Value::as_str),
                    Some("canceled" | "notFound")
                )
            {
                return;
            }
            match review_process::try_reap_killing_group(&mut self.child, &mut self.watch) {
                Ok(Some(status)) => {
                    self.exit = Some(status);
                    self.stopped = true;
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => return,
            }
        }
    }

    fn stop(&mut self) {
        if !self.stopped {
            if let Protocol::Codex(Codex::Waiting { login_id }) = &self.protocol {
                let cancel = serde_json::json!({
                    "id":3, "method":"account/login/cancel", "params":{"loginId":login_id}
                });
                self.queue_json(&cancel);
                let _ = self.flush_input();
                self.wait_for_cancel();
            }
            // The child has not been reaped. A guard must finish native cleanup itself;
            // direct synthetic fixtures use the existing reserved-group kill-before-wait path.
            if !self.stopped {
                if self.guarded {
                    // EOF reaches the independent guard even after this owner's hard death.
                    // The guard keeps the context lock until native cleanup has completed.
                    self.stdin.take();
                    let _ = self.child.wait();
                } else {
                    stop_probe(&mut self.child);
                }
                self.stopped = true;
            }
        }
        self.input.fill(0);
        self.input.clear();
        self.output.fill(0);
        self.output.clear();
    }
}

impl Drop for NativeLogin {
    fn drop(&mut self) {
        self.stop();
    }
}

enum Parsed {
    None,
    StartDevice,
    Event(Event),
}

enum Codex {
    Initializing,
    Starting,
    Waiting { login_id: String },
    Completed,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceResponse {
    #[serde(rename = "type")]
    kind: String,
    login_id: String,
    verification_url: String,
    user_code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoginCompleted {
    login_id: Option<String>,
    success: bool,
    error: Option<String>,
}

impl Codex {
    fn line(&mut self, line: &[u8]) -> Result<Parsed, Failure> {
        let message: Value = serde_json::from_slice(line).map_err(|_| Failure::Unsupported)?;
        let object = message.as_object().ok_or(Failure::Unsupported)?;
        if let Some(id) = object.get("id").and_then(Value::as_u64) {
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "id" | "result" | "error"))
            {
                return Err(Failure::Unsupported);
            }
            if let Some(error) = object.get("error") {
                return Err(
                    if matches!(
                        error.get("code").and_then(Value::as_i64),
                        Some(-32601 | -32602)
                    ) {
                        Failure::Unsupported
                    } else {
                        Failure::Failed
                    },
                );
            }
            let result = object.get("result").ok_or(Failure::Unsupported)?;
            return match (id, &self) {
                (1, Self::Initializing) if result.is_object() => {
                    *self = Self::Starting;
                    Ok(Parsed::StartDevice)
                }
                (2, Self::Starting) => {
                    let response: DeviceResponse = serde_json::from_value(result.clone())
                        .map_err(|_| Failure::InvalidChallenge)?;
                    if response.kind != "chatgptDeviceCode"
                        || !bounded_graphic(&response.login_id, 512)
                        || !bounded_graphic(&response.user_code, 128)
                    {
                        return Err(Failure::InvalidChallenge);
                    }
                    validate_url(ProviderKind::Codex, &response.verification_url)?;
                    *self = Self::Waiting {
                        login_id: response.login_id,
                    };
                    Ok(Parsed::Event(Event::Challenge(Challenge {
                        mode: Mode::DeviceCode,
                        url: response.verification_url,
                        user_code: Some(response.user_code),
                    })))
                }
                _ => Err(Failure::Unsupported),
            };
        }
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "method" | "params"))
        {
            return Err(Failure::Unsupported);
        }
        match object.get("method").and_then(Value::as_str) {
            Some("account/login/completed") => {
                let params = object.get("params").ok_or(Failure::Unsupported)?;
                if !params
                    .get("loginId")
                    .is_some_and(|value| value.is_null() || value.is_string())
                    || !params
                        .get("error")
                        .is_some_and(|value| value.is_null() || value.is_string())
                {
                    return Err(Failure::Unsupported);
                }
                let completed: LoginCompleted =
                    serde_json::from_value(params.clone()).map_err(|_| Failure::Unsupported)?;
                let Self::Waiting { login_id } = self else {
                    return Err(Failure::Failed);
                };
                if completed.login_id.as_deref() != Some(login_id.as_str())
                    || !completed.success
                    || completed.error.is_some()
                {
                    return Err(Failure::Failed);
                }
                *self = Self::Completed;
                Ok(Parsed::Event(Event::Completed))
            }
            // The official app-server announces the account change next to login completion.
            // The host re-probes identity separately; no identity bytes leave this adapter.
            Some("account/updated") if matches!(self, Self::Waiting { .. } | Self::Completed) => {
                if !object.get("params").is_some_and(Value::is_object) {
                    return Err(Failure::Unsupported);
                }
                Ok(Parsed::None)
            }
            _ => Err(Failure::Unsupported),
        }
    }
}

fn bounded_graphic(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Accept an exact native HTTPS authority; never repair, shorten, decode, or join URL output.
fn validate_url(kind: ProviderKind, url: &str) -> Result<(), Failure> {
    if url.is_empty() || url.len() > MAX_URL || !url.is_ascii() {
        return Err(Failure::InvalidChallenge);
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(Failure::InvalidChallenge);
    };
    let Some((host, path)) = rest.split_once('/') else {
        return Err(Failure::InvalidChallenge);
    };
    let allowed = match kind {
        ProviderKind::Codex => host == "auth.openai.com" && path == "codex/device",
        ProviderKind::Claude => {
            matches!(host, "claude.ai" | "claude.com" | "platform.claude.com") && !path.is_empty()
        }
    };
    if !allowed {
        return Err(Failure::InvalidChallenge);
    }
    let bytes = path.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let byte = bytes[offset];
        if byte == b'%' {
            if offset + 2 >= bytes.len()
                || !bytes[offset + 1].is_ascii_hexdigit()
                || !bytes[offset + 2].is_ascii_hexdigit()
            {
                return Err(Failure::InvalidChallenge);
            }
            offset += 3;
        } else if byte.is_ascii_alphanumeric() || b"-._~!$&()*+,;=:@/?".contains(&byte) {
            offset += 1;
        } else {
            return Err(Failure::InvalidChallenge);
        }
    }
    Ok(())
}

/// Claude documents URL output and code stdin for the dedicated `auth login` command, not a
/// machine-readable prompt protocol. Prose is discarded. Only one complete native HTTPS link
/// is actionable; terminal controls, extra links and requests for other inputs fail closed.
#[derive(Default)]
struct Claude {
    challenge: bool,
    submitted: bool,
    pending_url: Option<String>,
    pending_ready: bool,
}

impl Claude {
    fn consume(
        &mut self,
        output: &mut Vec<u8>,
        exit: Option<ExitStatus>,
    ) -> Result<Option<Event>, Failure> {
        while let Some((consumed, record, hyperlink)) = claude_record(output, exit.is_some())? {
            output.drain(..consumed);
            self.record(&record, hyperlink)?;
        }
        // Plain human prompts have no documented delimiter other than a complete record.
        // Never guess from a partial "Enter code" prefix: a delayed continuation may request
        // another secret. A fully closed OSC-8 target is already a complete link boundary.
        if !output.is_empty() {
            if let Ok(text) = std::str::from_utf8(output)
                && !text.contains("://")
                && other_input(text)
            {
                return Err(Failure::Unsupported);
            }
            if !self.pending_ready {
                return Ok(None);
            }
        }
        if self.pending_ready
            && let Some(url) = self.pending_url.take()
        {
            self.challenge = true;
            return Ok(Some(Event::Challenge(Challenge {
                mode: Mode::CodeOrCallback,
                url,
                user_code: None,
            })));
        }
        if let Some(status) = exit {
            return if status.success() && self.challenge {
                Ok(Some(Event::Completed))
            } else {
                Err(Failure::Failed)
            };
        }
        Ok(None)
    }

    fn record(&mut self, record: &str, hyperlink: bool) -> Result<(), Failure> {
        let record = record.trim();
        if record.is_empty() {
            return Ok(());
        }
        let links: Vec<_> = record
            .split_whitespace()
            .filter(|word| word.contains("://"))
            .collect();
        let prose = record
            .split_whitespace()
            .filter(|word| !word.contains("://"))
            .collect::<Vec<_>>()
            .join(" ");
        if other_input(&prose) {
            return Err(Failure::Unsupported);
        }
        if links.len() > 1 {
            return Err(Failure::InvalidChallenge);
        }
        if let Some(url) = links.first() {
            if self.challenge || self.pending_url.is_some() {
                return Err(Failure::InvalidChallenge);
            }
            validate_url(ProviderKind::Claude, url)?;
            self.pending_url = Some((*url).to_string());
            self.pending_ready = hyperlink;
        } else if code_prompt(record) {
            self.pending_ready = self.pending_url.is_some();
        } else if self.pending_url.is_some() && !self.pending_ready {
            return Err(Failure::InvalidChallenge);
        } else if looks_like_prompt(record) && !link_heading(record) {
            return Err(Failure::Unsupported);
        } else if record.starts_with(['?', '&'])
            || (!record.contains(char::is_whitespace) && record.contains('='))
        {
            // A continuation is not a second URL alphabet. Never join a wrapped/truncated URL.
            return Err(Failure::InvalidChallenge);
        }
        Ok(())
    }

    fn can_submit(&self) -> bool {
        self.challenge && !self.submitted
    }
}

fn other_input(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "password",
        "api key",
        "access token",
        "refresh token",
        "two-factor",
        "2fa",
        "mfa",
        "authenticator",
        "passkey",
        "recovery code",
        "sms",
        "email code",
        "[y/n]",
        "[y/n",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

fn looks_like_prompt(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.ends_with(['?', '>', ':'])
        || other_input(text)
        || ["enter ", "paste ", "input ", "select ", "choose "]
            .iter()
            .any(|word| lower.contains(word))
}

fn code_prompt(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("code")
        && ["paste", "enter", "input"]
            .iter()
            .any(|word| lower.contains(word))
        && !other_input(text)
        && !lower.contains("token")
}

fn link_heading(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.ends_with(':')
        && ["url", "link", "browser"]
            .iter()
            .any(|word| lower.contains(word))
        && !other_input(text)
}

/// Decode one complete stdout record. SGR formatting can be discarded; an OSC-8 hyperlink
/// supplies its complete target even when its visible label wraps across lines. Other control
/// operations are not a login protocol, and are never replayed into a terminal or private UI.
fn claude_record(input: &[u8], eof: bool) -> Result<Option<(usize, String, bool)>, Failure> {
    let mut text = Vec::new();
    let mut hyperlink = false;
    let mut offset = 0;
    while offset < input.len() {
        match input[offset] {
            b'\n' => {
                return String::from_utf8(text)
                    .map(|text| Some((offset + 1, text, hyperlink)))
                    .map_err(|_| Failure::Unsupported);
            }
            b'\r' if input.get(offset + 1) == Some(&b'\n') => offset += 1,
            b'\r' if offset + 1 == input.len() && !eof => return Ok(None),
            0x1b => {
                let tail = &input[offset..];
                if tail.starts_with(b"\x1b[") {
                    let Some(end) = tail[2..]
                        .iter()
                        .position(|byte| !byte.is_ascii_digit() && *byte != b';')
                    else {
                        return if eof {
                            Err(Failure::Unsupported)
                        } else {
                            Ok(None)
                        };
                    };
                    if tail[end + 2] != b'm' {
                        return Err(Failure::Unsupported);
                    }
                    offset += end + 3;
                } else if tail.starts_with(b"\x1b]8;;") {
                    let Some((target_end, terminator_length)) = osc_end(&tail[5..]) else {
                        return if eof {
                            Err(Failure::InvalidChallenge)
                        } else {
                            Ok(None)
                        };
                    };
                    let target = &tail[5..5 + target_end];
                    let target_text =
                        std::str::from_utf8(target).map_err(|_| Failure::InvalidChallenge)?;
                    validate_url(ProviderKind::Claude, target_text)?;
                    let label_start = 5 + target_end + terminator_length;
                    let Some(close) = tail[label_start..]
                        .windows(5)
                        .position(|bytes| bytes == b"\x1b]8;;")
                    else {
                        return if eof {
                            Err(Failure::InvalidChallenge)
                        } else {
                            Ok(None)
                        };
                    };
                    if !safe_hyperlink_label(&tail[label_start..label_start + close]) {
                        return Err(Failure::Unsupported);
                    }
                    let close = label_start + close + 5;
                    let Some((close_end, close_length)) = osc_end(&tail[close..]) else {
                        return if eof {
                            Err(Failure::InvalidChallenge)
                        } else {
                            Ok(None)
                        };
                    };
                    if close_end != 0 {
                        return Err(Failure::InvalidChallenge);
                    }
                    hyperlink = true;
                    text.extend_from_slice(target);
                    offset += close + close_length;
                } else if tail.len() < 5 && !eof {
                    return Ok(None);
                } else {
                    return Err(Failure::Unsupported);
                }
            }
            byte if byte.is_ascii_control() && byte != b'\t' => return Err(Failure::Unsupported),
            byte => {
                text.push(byte);
                offset += 1;
            }
        }
    }
    if eof && !input.is_empty() {
        String::from_utf8(text)
            .map(|text| Some((input.len(), text, hyperlink)))
            .map_err(|_| Failure::Unsupported)
    } else {
        Ok(None)
    }
}

fn safe_hyperlink_label(label: &[u8]) -> bool {
    if std::str::from_utf8(label).is_err() {
        return false;
    }
    let mut offset = 0;
    while offset < label.len() {
        match label[offset] {
            0x1b if label[offset..].starts_with(b"\x1b[") => {
                let tail = &label[offset + 2..];
                let Some(end) = tail
                    .iter()
                    .position(|byte| !byte.is_ascii_digit() && *byte != b';')
                else {
                    return false;
                };
                if tail[end] != b'm' {
                    return false;
                }
                offset += end + 3;
            }
            byte if byte.is_ascii_control() && !b"\r\n\t".contains(&byte) => return false,
            _ => offset += 1,
        }
    }
    true
}

fn osc_end(input: &[u8]) -> Option<(usize, usize)> {
    input
        .iter()
        .enumerate()
        .find_map(|(index, byte)| match byte {
            0x07 => Some((index, 1)),
            0x1b if input.get(index + 1) == Some(&b'\\') => Some((index, 2)),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = r#"{"id":2,"result":{"type":"chatgptDeviceCode","loginId":"synthetic-login","verificationUrl":"https://auth.openai.com/codex/device","userCode":"TEST-CODE"}}"#;

    fn initialized_codex() -> Codex {
        let mut protocol = Codex::Initializing;
        assert!(matches!(
            protocol.line(br#"{"id":1,"result":{"userAgent":"synthetic"}}"#),
            Ok(Parsed::StartDevice)
        ));
        protocol
    }

    #[test]
    fn codex_uses_native_device_challenge_and_matching_completion() {
        let mut protocol = initialized_codex();
        let Parsed::Event(Event::Challenge(challenge)) = protocol.line(DEVICE.as_bytes()).unwrap()
        else {
            panic!("expected a challenge");
        };
        assert!(challenge.mode == Mode::DeviceCode);
        assert_eq!(challenge.url, "https://auth.openai.com/codex/device");
        assert_eq!(challenge.user_code.as_deref(), Some("TEST-CODE"));
        assert!(matches!(
            protocol.line(br#"{"method":"account/updated","params":{"authMode":"chatgpt","planType":"plus"}}"#),
            Ok(Parsed::None)
        ));
        assert!(matches!(
            protocol.line(br#"{"method":"account/login/completed","params":{"loginId":"synthetic-login","success":true,"error":null}}"#),
            Ok(Parsed::Event(Event::Completed))
        ));
    }

    #[test]
    fn codex_refuses_wrong_replayed_or_failed_completion() {
        for completion in [
            r#"{"loginId":"another-login","success":true,"error":null}"#,
            r#"{"loginId":null,"success":true,"error":null}"#,
            r#"{"loginId":"synthetic-login","success":false,"error":"synthetic secret"}"#,
            r#"{"loginId":"synthetic-login","success":true,"error":"synthetic secret"}"#,
        ] {
            let mut protocol = initialized_codex();
            protocol.line(DEVICE.as_bytes()).unwrap();
            let message =
                format!(r#"{{"method":"account/login/completed","params":{completion}}}"#);
            assert!(matches!(
                protocol.line(message.as_bytes()),
                Err(Failure::Failed)
            ));
        }
        assert!(matches!(
            initialized_codex().line(br#"{"method":"account/login/completed","params":{"loginId":"synthetic-login","success":true,"error":null}}"#),
            Err(Failure::Failed)
        ));
    }

    #[test]
    fn codex_unsupported_native_method_is_a_redacted_category() {
        assert!(matches!(
            initialized_codex()
                .line(br#"{"id":2,"error":{"code":-32602,"message":"synthetic-secret-error"}}"#),
            Err(Failure::Unsupported)
        ));
        assert!(matches!(
            initialized_codex()
                .line(br#"{"id":2,"error":{"code":-32000,"message":"synthetic-secret-error"}}"#),
            Err(Failure::Failed)
        ));
    }

    #[test]
    fn codex_rejects_unknown_or_broadened_challenge_shapes() {
        for output in [
            DEVICE.replace("chatgptDeviceCode", "chatgpt"),
            DEVICE.replace("TEST-CODE", "TEST\\nCODE"),
            DEVICE.replace("auth.openai.com", "auth.openai.com.evil.invalid"),
            DEVICE.replace(
                "\"userCode\":\"TEST-CODE\"",
                "\"token\":\"synthetic-secret\"",
            ),
            DEVICE.replace("\"id\":2", "\"id\":3"),
            "not a protocol response".into(),
        ] {
            assert!(initialized_codex().line(output.as_bytes()).is_err());
        }
    }

    #[test]
    fn urls_require_exact_https_authority_and_complete_ascii_syntax() {
        for url in [
            "https://auth.openai.com/codex/device",
            "https://claude.ai/oauth/authorize?state=synthetic&redirect_uri=http%3A%2F%2Flocalhost",
            "https://claude.com/oauth/authorize?state=synthetic",
            "https://platform.claude.com/oauth/authorize?state=synthetic",
        ] {
            let kind = if url.contains("openai") {
                ProviderKind::Codex
            } else {
                ProviderKind::Claude
            };
            assert!(validate_url(kind, url).is_ok());
        }
        for url in [
            "http://claude.ai/oauth/authorize",
            "https://claude.ai.evil.invalid/oauth/authorize",
            "https://claude.ai@evil.invalid/oauth/authorize",
            "https://evil.invalid@claude.ai/oauth/authorize",
            "https://claude.ai:443/oauth/authorize",
            "https://claude.ai./oauth/authorize",
            "https://CLAUDE.AI/oauth/authorize",
            "https://claude.ai\\evil.invalid/oauth/authorize",
            "https://claude.ai/oauth/authorize\n?state=wrapped",
            "https://claude.ai/oauth/authorize?state=truncated%2",
            "https://claude.ai/oauth/authorize?state=bad%xx",
            "https://claude.ai/oauth/authorize?state=nonascii-☃",
            "https://claude.ai/oauth/authorize#fragment",
            "https://claude.ai/oauth/authorize?state=space here",
            "https://claude.ai/oauth/authorize?state=<script>",
            "https://claude.ai",
        ] {
            assert!(validate_url(ProviderKind::Claude, url).is_err());
        }
        assert!(validate_url(ProviderKind::Codex, "https://auth.openai.com/another/path").is_err());
        assert!(validate_url(ProviderKind::Claude, &"x".repeat(MAX_URL + 1)).is_err());
    }

    fn fixture(kind: ProviderKind, script: &str) -> NativeLogin {
        let mut command = Command::new("/bin/sh");
        command.env_clear().args(["-c", script]);
        NativeLogin::spawn(&mut command, kind).unwrap()
    }

    fn next_event(login: &mut NativeLogin) -> Result<Event, Failure> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = login.poll()? {
                return Ok(event);
            }
            assert!(Instant::now() < deadline, "synthetic login did not answer");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn claude_accepts_one_complete_url_without_inventing_a_json_protocol() {
        let mut protocol = Claude::default();
        let mut output = b"Open this URL in your browser:\nhttps://claude.ai/oauth/authorize?state=synthetic&challenge=sms\nPaste the browser code here > \n".to_vec();
        let Some(Event::Challenge(challenge)) = protocol.consume(&mut output, None).unwrap() else {
            panic!("expected a native Claude link");
        };
        assert!(challenge.mode == Mode::CodeOrCallback);
        assert!(challenge.user_code.is_none());
        assert_eq!(
            challenge.url,
            "https://claude.ai/oauth/authorize?state=synthetic&challenge=sms"
        );
        assert!(output.is_empty());
        assert!(protocol.can_submit());
    }

    #[test]
    fn claude_uses_complete_osc8_target_when_display_label_wraps() {
        for terminator in ["\x07", "\x1b\\"] {
            let text = format!(
                "\x1b[1mOpen browser:\x1b[0m\n\x1b]8;;https://claude.com/oauth/authorize?state=synthetic{terminator}https://claude.com/oauth/\n  authorize?state=synthetic\x1b]8;;{terminator}\n"
            );
            let mut protocol = Claude::default();
            let mut output = text.into_bytes();
            let Some(Event::Challenge(challenge)) = protocol.consume(&mut output, None).unwrap()
            else {
                panic!("expected a complete hyperlink");
            };
            assert_eq!(
                challenge.url,
                "https://claude.com/oauth/authorize?state=synthetic"
            );
        }
    }

    #[test]
    fn claude_url_and_hyperlink_parsing_is_independent_of_pipe_chunk_boundaries() {
        for text in [
            "Open browser:\r\nhttps://claude.ai/oauth/authorize?state=synthetic\r\nPaste browser code >\n",
            "Open browser:\n\x1b]8;;https://claude.com/oauth/authorize?state=synthetic\x1b\\wrapped\nlabel\x1b]8;;\x1b\\\n",
            "✓ Open browser:\nhttps://claude.ai/oauth/authorize?state=synthetic\nPaste browser code >\n",
        ] {
            for split in 0..text.len() {
                let mut protocol = Claude::default();
                let mut output = text.as_bytes()[..split].to_vec();
                assert!(
                    matches!(protocol.consume(&mut output, None), Ok(None)),
                    "partial record was actionable at byte {split}"
                );
                output.extend_from_slice(&text.as_bytes()[split..]);
                assert!(matches!(
                    protocol.consume(&mut output, None),
                    Ok(Some(Event::Challenge(_)))
                ));
                assert!(output.is_empty());
            }
        }
    }

    #[test]
    fn claude_refuses_unsafe_controls_extra_links_and_other_input_prompts() {
        for text in [
            "https://claude.ai/oauth/authorize?state=one\nhttps://claude.ai/oauth/authorize?state=two\n",
            "https://evil.invalid/oauth/authorize\n",
            "https://claude.ai/oauth/authorize?state=\ncontinued=value\n",
            "https://claude.ai/oauth/authorize\nPassword: ",
            "https://claude.ai/oauth/authorize\nEnter your API key: ",
            "https://claude.ai/oauth/authorize\nEnter the code from your authenticator: ",
            "https://claude.ai/oauth/authorize\nChoose a different account? ",
            "https://claude.ai/oauth/authorize\n\x1b[2J",
            "\x1b]8;;https://claude.ai/oauth/authorize\x07bad\x1b[2Jlabel\x1b]8;;\x07\n",
        ] {
            let mut framed = text.as_bytes().to_vec();
            framed.push(b'\n');
            assert!(Claude::default().consume(&mut framed, None).is_err());
        }
    }

    #[test]
    fn malformed_plaintext_urls_never_escape_at_any_pipe_split() {
        for text in [
            "https://claude.ai/oauth/authorize?state=\ncontinued=value\nPaste browser code >\n",
            "https://claude.ai/oauth/authorize?state=abc\ndefgh\nPaste browser code >\n",
            "https://claude.ai/oauth/authorize?state=abc\nEnter the code from your authenticator:\n",
            "https://claude.ai/oauth/authorize?state=abc\nEnter the code: from your authenticator:\n",
            "https://claude.ai/oauth/authorize?state=one\nhttps://claude.ai/oauth/authorize?state=two\nPaste browser code >\n",
            "https://claude.ai/oauth/authorize\nEnter your password: ",
        ] {
            for split in 0..=text.len() {
                let mut protocol = Claude::default();
                let mut output = text.as_bytes()[..split].to_vec();
                match protocol.consume(&mut output, None) {
                    Err(_) => continue,
                    Ok(Some(_)) => panic!("invalid challenge escaped at byte {split}"),
                    Ok(None) => {}
                }
                output.extend_from_slice(&text.as_bytes()[split..]);
                assert!(protocol.consume(&mut output, None).is_err());
            }
        }
        let mut protocol = Claude::default();
        let mut bare = b"https://claude.ai/oauth/authorize?state=synthetic\n".to_vec();
        assert!(matches!(protocol.consume(&mut bare, None), Ok(None)));
        assert!(protocol.pending_url.is_some());
        assert!(!protocol.can_submit());
    }

    #[test]
    fn claude_keeps_partial_links_until_the_complete_native_record() {
        let mut protocol = Claude::default();
        let mut output = b"https://claude.ai/oauth/authorize?state=partial".to_vec();
        assert!(matches!(protocol.consume(&mut output, None), Ok(None)));
        output.extend_from_slice(b"-complete\nPaste browser code >\n");
        let Some(Event::Challenge(challenge)) = protocol.consume(&mut output, None).unwrap() else {
            panic!("expected a complete URL");
        };
        assert_eq!(
            challenge.url,
            "https://claude.ai/oauth/authorize?state=partial-complete"
        );
    }

    #[test]
    fn synthetic_claude_process_accepts_exactly_one_code_and_reaps() {
        let mut login = fixture(
            ProviderKind::Claude,
            "printf '%s\\n' 'https://claude.ai/oauth/authorize?state=synthetic'; printf 'Paste the browser code here > \\n'; read -r code; test \"$code\" = 'synthetic-code#state'",
        );
        assert!(matches!(next_event(&mut login), Ok(Event::Challenge(_))));
        for code in ["", "line\nbreak", "line\rbreak", "embedded\0nul", "\x1b[2J"] {
            assert_eq!(login.submit_code(code), Err(Failure::InvalidChallenge));
        }
        login.submit_code("synthetic-code#state").unwrap();
        assert_eq!(
            login.submit_code("second-code"),
            Err(Failure::InvalidChallenge)
        );
        assert!(matches!(next_event(&mut login), Ok(Event::Completed)));
        assert!(login.stopped);
    }

    #[test]
    fn synthetic_claude_callback_completes_without_returned_code() {
        let mut login = fixture(
            ProviderKind::Claude,
            "printf '%s\\n' 'https://claude.ai/oauth/authorize?state=synthetic'; printf 'Paste browser code >'; exit 0",
        );
        assert!(matches!(next_event(&mut login), Ok(Event::Challenge(_))));
        assert!(matches!(next_event(&mut login), Ok(Event::Completed)));
        assert!(login.stopped);
    }

    #[test]
    fn native_nonzero_exit_never_presents_a_stale_challenge() {
        let mut login = fixture(
            ProviderKind::Claude,
            "printf '%s\\n' 'https://claude.ai/oauth/authorize?state=synthetic'; exit 1",
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while login.watch.exited() != Some(true) {
            assert!(Instant::now() < deadline, "synthetic exit was not observed");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(login.poll(), Err(Failure::Failed)));
    }

    #[test]
    fn synthetic_codex_process_owns_protocol_and_is_reaped_on_completion() {
        let mut login = fixture(
            ProviderKind::Codex,
            &format!(
                "read -r initialize\nprintf '%s\\n' '{{\"id\":1,\"result\":{{}}}}'\nread -r initialized\nread -r start\nprintf '%s\\n' '{DEVICE}'\nprintf '%s\\n' '{{\"method\":\"account/login/completed\",\"params\":{{\"loginId\":\"synthetic-login\",\"success\":true,\"error\":null}}}}'\nread -r cancel"
            ),
        );
        let pid = login.child.id();
        assert!(matches!(next_event(&mut login), Ok(Event::Challenge(_))));
        assert_eq!(
            login.submit_code("not-a-device-operation"),
            Err(Failure::InvalidChallenge)
        );
        assert!(matches!(next_event(&mut login), Ok(Event::Completed)));
        assert!(login.stopped);
        drop(login);
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err());
    }

    #[test]
    fn dropping_device_login_requests_native_cancel_before_reaping() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("native-canceled");
        let script = format!(
            "read -r initialize\nprintf '%s\\n' '{{\"id\":1,\"result\":{{}}}}'\nread -r initialized\nread -r start\nprintf '%s\\n' '{DEVICE}'\nread -r cancel\ncase \"$cancel\" in *'account/login/cancel'*) ;; *) exit 1;; esac\ncase \"$cancel\" in *'synthetic-login'*) ;; *) exit 1;; esac\nprintf canceled > \"$1\"\nprintf '%s\\n' '{{\"id\":3,\"result\":{{\"status\":\"canceled\"}}}}'\nread -r hold"
        );
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args(["-c", &script, "fixture"])
            .arg(&marker);
        let mut login = NativeLogin::spawn(&mut command, ProviderKind::Codex).unwrap();
        let pid = login.child.id();
        assert!(matches!(next_event(&mut login), Ok(Event::Challenge(_))));
        drop(login);
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "canceled");
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err());
    }

    #[test]
    fn native_stderr_never_enters_the_private_challenge_buffer() {
        let mut login = fixture(
            ProviderKind::Claude,
            "printf '%s\\n' 'synthetic-stderr-secret' >&2; printf '%s\\n' 'https://claude.ai/oauth/authorize?state=synthetic'; printf 'Paste browser code >'; exit 0",
        );
        assert!(matches!(next_event(&mut login), Ok(Event::Challenge(_))));
        assert!(matches!(next_event(&mut login), Ok(Event::Completed)));
    }

    #[test]
    fn dropping_pending_native_login_reaps_the_owned_process() {
        let login = fixture(ProviderKind::Claude, "read -r code");
        let pid = login.child.id();
        drop(login);
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err());
    }

    #[test]
    fn native_output_limit_is_cumulative_and_never_a_diagnostic() {
        let mut login = fixture(
            ProviderKind::Codex,
            "while :; do printf '%s' 'synthetic-secret-output'; done",
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match login.poll() {
                Err(Failure::OutputLimit) => break,
                Ok(None) => {}
                _ => panic!("expected bounded-output refusal"),
            }
            assert!(
                Instant::now() < deadline,
                "output fixture did not fill its bound"
            );
        }
        assert!(login.stopped);
        assert!(login.output.is_empty());
        assert_eq!(format!("{:?}", Failure::OutputLimit), "OutputLimit");
    }
}
