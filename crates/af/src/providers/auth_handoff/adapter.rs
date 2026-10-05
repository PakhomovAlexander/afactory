//! Private, bounded transport to the official CLIs' dedicated login operations.
//!
//! No provider-owned bytes become diagnostics. Challenge material exists only in memory and
//! is passed to the private presentation host, never to the invoking command's streams.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

use super::super::{
    BoundDirectoryLock, ProviderKind, ProviderSpec, sanitized_path, set_nonblocking, stop_probe,
    validate_private_auth_directory,
};

const MAX_OUTPUT: usize = 64 * 1024;
const MAX_URL: usize = 12 * 1024;
const MAX_CODE: usize = 4096;

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
    ProviderCliMissing,
    InvalidChallenge,
    Failed,
    Authentication(super::guard::AuthenticationFailure),
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
        Ok(Self {
            child,
            watch,
            stdin: Some(stdin),
            guarded,
            stdout,
            protocol: match kind {
                ProviderKind::Codex => Protocol::Codex(Codex::default()),
                ProviderKind::Claude => Protocol::Claude(Claude::default()),
            },
            input: Vec::new(),
            input_written: 0,
            output: Vec::new(),
            output_bytes: 0,
            exit: None,
            stopped: false,
            terminal: false,
        })
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
        if let Some(status) = self.exit
            && !status.success()
        {
            return Err(if self.guarded {
                match status.code() {
                    Some(super::guard::PROVIDER_CLI_MISSING) => Failure::ProviderCliMissing,
                    code => code
                        .and_then(super::guard::AuthenticationFailure::from_exit_code)
                        .map_or(Failure::Failed, Failure::Authentication),
                }
            } else {
                Failure::Failed
            });
        }
        match &mut self.protocol {
            Protocol::Codex(protocol) => protocol.consume(&mut self.output, self.exit),
            Protocol::Claude(protocol) => protocol.consume(&mut self.output, self.exit),
        }
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

    fn stop(&mut self) {
        if !self.stopped {
            // The child has not been reaped. A guard must finish native cleanup itself;
            // direct synthetic fixtures use the existing reserved-group kill-before-wait path.
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

// Codex 0.159.2 login/src/device_code_auth.rs::device_code_prompt prints this exact
// stdout frame, including unconditional SGR and the extra newline from println!. Match only
// those SGR positions: never remove arbitrary terminal controls or search prose for a URL.
// cli/src/login.rs::run_login_with_device_code reports completion on stderr and by exit status;
// stderr stays inside the lifetime guard and yields only closed failure categories. No success
// prose, app-server notification, or status alone completes login.
const CODEX_DEVICE_URL: &str = "https://auth.openai.com/codex/device";
const CODEX_DEVICE_PREFIX: &str = concat!(
    "\nWelcome to Codex [v\x1b[90m0.159.2\x1b[0m]\n",
    "\x1b[90mOpenAI's command-line coding agent\x1b[0m\n",
    "\nFollow these steps to sign in with ChatGPT using device code authorization:\n",
    "\n1. Open this link in your browser and sign in to your account\n",
    "   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m\n",
    "\n2. Enter this one-time code \x1b[90m(expires in 15 minutes)\x1b[0m\n   \x1b[94m",
);
const CODEX_DEVICE_SUFFIX: &str = concat!(
    "\x1b[0m\n\n\x1b[90mContinue only if you started this login in Codex. ",
    "If a website or another person gave you this code, cancel.\x1b[0m\n\n",
);
const MAX_DEVICE_CODE: usize = 128;

#[derive(Default)]
struct Codex {
    challenge: bool,
}

impl Codex {
    fn consume(
        &mut self,
        output: &mut Vec<u8>,
        exit: Option<ExitStatus>,
    ) -> Result<Option<Event>, Failure> {
        if self.challenge {
            if !output.is_empty() {
                return Err(Failure::Unsupported);
            }
            return match exit {
                Some(status) if status.success() => Ok(Some(Event::Completed)),
                Some(_) => Err(Failure::Failed),
                None => Ok(None),
            };
        }
        let prefix = CODEX_DEVICE_PREFIX.as_bytes();
        let common = output.len().min(prefix.len());
        if output[..common] != prefix[..common] {
            return Err(Failure::Unsupported);
        }
        if output.len() < prefix.len() {
            return if exit.is_some() {
                Err(Failure::Failed)
            } else {
                Ok(None)
            };
        }
        let field = &output[prefix.len()..];
        // Upstream treats user_code as an opaque server-issued String, not a fixed 4-4
        // regex. Support bounded uppercase/digit/hyphen codes conservatively; any other
        // alphabet requires explicit characterization, never guessing or forwarding tokens.
        let code_end = field.iter().position(|byte| {
            !(byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'-')
        });
        let code_len = code_end.unwrap_or(field.len());
        if code_len > MAX_DEVICE_CODE {
            return Err(Failure::InvalidChallenge);
        }
        let Some(code_len) = code_end else {
            return if exit.is_some() {
                Err(Failure::Failed)
            } else {
                Ok(None)
            };
        };
        if code_len == 0 || !field[..code_len].iter().any(u8::is_ascii_alphanumeric) {
            return Err(Failure::InvalidChallenge);
        }
        let suffix = &field[code_len..];
        let expected = CODEX_DEVICE_SUFFIX.as_bytes();
        if suffix.len() > expected.len() || suffix != &expected[..suffix.len()] {
            return Err(Failure::InvalidChallenge);
        }
        if suffix.len() != expected.len() {
            return if exit.is_some() {
                Err(Failure::Failed)
            } else {
                Ok(None)
            };
        }
        if exit.is_some_and(|status| !status.success()) {
            return Err(Failure::Failed);
        }
        let code =
            String::from_utf8(field[..code_len].to_vec()).map_err(|_| Failure::InvalidChallenge)?;
        output.fill(0);
        output.clear();
        self.challenge = true;
        Ok(Some(Event::Challenge(Challenge {
            mode: Mode::DeviceCode,
            url: CODEX_DEVICE_URL.into(),
            user_code: Some(code),
        })))
    }
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
// Claude 2.1.289's dedicated login command under TERM=dumb/piped stdout writes
// two newline-terminated headings, then exactly this unterminated stdin prompt.
// The source-characterized literal boundary is distinct from generic partial "Enter code"
// text. Both browser callback and pasted-code completion append this exact success line.
const CLAUDE_NATIVE_PREFIX: &str =
    "Opening browser to sign in…\nIf the browser didn't open, visit: ";
const CLAUDE_NATIVE_PROMPT: &[u8] = b"Paste code here if prompted > ";
const CLAUDE_NATIVE_SUCCESS: &[u8] = b"Login successful.\n";

#[derive(Default)]
struct Claude {
    started: bool,
    native: bool,
    native_success: bool,
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
        if !self.started {
            // Wait across every split of the heading, including its UTF-8 ellipsis.
            let heading = b"Opening browser";
            if output.len() < heading.len() && heading.starts_with(output) {
                return if exit.is_some() {
                    Err(Failure::Failed)
                } else {
                    Ok(None)
                };
            }
            self.native = output.starts_with(heading);
            self.started = true;
        }
        if self.native {
            return self.consume_native(output, exit);
        }
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

    fn consume_native(
        &mut self,
        output: &mut Vec<u8>,
        exit: Option<ExitStatus>,
    ) -> Result<Option<Event>, Failure> {
        if exit.is_some_and(|status| !status.success()) {
            return Err(Failure::Failed);
        }
        if self.challenge {
            if !output.is_empty() {
                if self.native_success {
                    return Err(Failure::Unsupported);
                }
                if !native_success_tail(output, exit)? {
                    return Ok(None);
                }
                self.native_success = true;
                output.clear();
            }
            return match exit {
                Some(status) if status.success() && self.native_success => {
                    Ok(Some(Event::Completed))
                }
                Some(_) => Err(Failure::Failed),
                None => Ok(None),
            };
        }
        let prefix = CLAUDE_NATIVE_PREFIX.as_bytes();
        let common = output.len().min(prefix.len());
        if output[..common] != prefix[..common] {
            return Err(Failure::Unsupported);
        }
        if output.len() < prefix.len() {
            return if exit.is_some() {
                Err(Failure::Failed)
            } else {
                Ok(None)
            };
        }
        let rest = &output[prefix.len()..];
        let Some(end) = rest.iter().position(|byte| *byte == b'\n') else {
            return if rest.len() > MAX_URL {
                Err(Failure::InvalidChallenge)
            } else if exit.is_some() {
                Err(Failure::Failed)
            } else {
                Ok(None)
            };
        };
        let url = std::str::from_utf8(&rest[..end]).map_err(|_| Failure::InvalidChallenge)?;
        validate_url(ProviderKind::Claude, url)?;
        let prompt = &rest[end + 1..];
        let common = prompt.len().min(CLAUDE_NATIVE_PROMPT.len());
        if prompt[..common] != CLAUDE_NATIVE_PROMPT[..common] {
            return Err(Failure::Unsupported);
        }
        if prompt.len() < CLAUDE_NATIVE_PROMPT.len() {
            return if exit.is_some() {
                Err(Failure::Failed)
            } else {
                Ok(None)
            };
        }
        let tail = &prompt[CLAUDE_NATIVE_PROMPT.len()..];
        if !tail.is_empty() && !native_success_tail(tail, exit)? {
            return Ok(None);
        }
        self.native_success = !tail.is_empty();
        let url = url.to_owned();
        output.fill(0);
        output.clear();
        self.challenge = true;
        Ok(Some(Event::Challenge(Challenge {
            mode: Mode::CodeOrCallback,
            url,
            user_code: None,
        })))
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

fn native_success_tail(tail: &[u8], exit: Option<ExitStatus>) -> Result<bool, Failure> {
    if !CLAUDE_NATIVE_SUCCESS.starts_with(tail) {
        return Err(Failure::Unsupported);
    }
    if tail.len() == CLAUDE_NATIVE_SUCCESS.len() {
        Ok(true)
    } else if exit.is_some() {
        Err(Failure::Failed)
    } else {
        Ok(false)
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

    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};

    // Exact 0.159.2 source-authored device_code_prompt fixture, with a synthetic code.
    // https://raw.githubusercontent.com/openai/codex/rust-v0.159.2/codex-rs/login/src/device_code_auth.rs
    const DEVICE: &str = "\nWelcome to Codex [v\x1b[90m0.159.2\x1b[0m]\n\x1b[90mOpenAI's command-line coding agent\x1b[0m\n\nFollow these steps to sign in with ChatGPT using device code authorization:\n\n1. Open this link in your browser and sign in to your account\n   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m\n\n2. Enter this one-time code \x1b[90m(expires in 15 minutes)\x1b[0m\n   \x1b[94mTEST-CODE\x1b[0m\n\n\x1b[90mContinue only if you started this login in Codex. If a website or another person gave you this code, cancel.\x1b[0m\n\n";

    #[test]
    fn codex_0159_2_exact_native_device_prompt_and_successful_exit() {
        let mut protocol = Codex::default();
        let mut output = DEVICE.as_bytes().to_vec();
        let Some(Event::Challenge(challenge)) = protocol.consume(&mut output, None).unwrap() else {
            panic!("expected a challenge");
        };
        assert!(challenge.mode == Mode::DeviceCode);
        assert_eq!(challenge.url, "https://auth.openai.com/codex/device");
        assert_eq!(challenge.user_code.as_deref(), Some("TEST-CODE"));
        assert!(output.is_empty());
        assert!(matches!(protocol.consume(&mut output, None), Ok(None)));
        assert!(matches!(
            protocol.consume(&mut output, Some(ExitStatus::from_raw(0))),
            Ok(Some(Event::Completed))
        ));
    }

    #[test]
    fn codex_0159_2_waits_for_the_complete_frame_at_every_byte_boundary() {
        for boundary in 0..DEVICE.len() {
            let mut protocol = Codex::default();
            let mut output = DEVICE.as_bytes()[..boundary].to_vec();
            assert!(matches!(protocol.consume(&mut output, None), Ok(None)));
            output.extend_from_slice(&DEVICE.as_bytes()[boundary..]);
            assert!(matches!(
                protocol.consume(&mut output, None),
                Ok(Some(Event::Challenge(_)))
            ));
        }
        let mut protocol = Codex::default();
        let mut output = Vec::new();
        for (index, byte) in DEVICE.bytes().enumerate() {
            output.push(byte);
            let result = protocol.consume(&mut output, None);
            if index + 1 == DEVICE.len() {
                assert!(matches!(result, Ok(Some(Event::Challenge(_)))));
            } else {
                assert!(matches!(result, Ok(None)));
            }
        }
    }

    #[test]
    fn codex_0159_2_refuses_nonzero_exit_or_success_without_a_complete_challenge() {
        for output in ["", "Successfully logged in\n", &DEVICE[..DEVICE.len() - 1]] {
            assert!(
                Codex::default()
                    .consume(
                        &mut output.as_bytes().to_vec(),
                        Some(ExitStatus::from_raw(0))
                    )
                    .is_err()
            );
        }
        let mut protocol = Codex::default();
        let mut output = DEVICE.as_bytes().to_vec();
        assert!(matches!(
            protocol.consume(&mut output, Some(ExitStatus::from_raw(256))),
            Err(Failure::Failed)
        ));
        let mut protocol = Codex::default();
        protocol.consume(&mut output, None).unwrap();
        assert!(matches!(
            protocol.consume(&mut output, Some(ExitStatus::from_raw(256))),
            Err(Failure::Failed)
        ));
    }

    #[test]
    fn codex_0159_2_rejects_unknown_framing_extra_urls_tokens_controls_and_versions() {
        for output in [
            DEVICE.replace("\nWelcome", "\n Welcome"),
            DEVICE.replace("\nFollow", "\n Follow"),
            DEVICE.replace("\n1.", "\n 1."),
            DEVICE.replace("\n2.", "\n 2."),
            DEVICE.replace("\n   \x1b[94m", "\n \x1b[94m"),
            DEVICE.replace("\n   \x1b[94m", "\n  \x1b[94m"),
            DEVICE.replace("\n   \x1b[94m", "\n    \x1b[94m"),
            DEVICE.replace("0.159.2", "0.159.3"),
            DEVICE.replace("0.159.2", "0.159.2; token=secret"),
            DEVICE.replace("auth.openai.com", "auth.openai.com.evil.invalid"),
            DEVICE.replace("codex/device", "codex/device?token=secret"),
            DEVICE.replace("TEST-CODE", ""),
            DEVICE.replace("TEST-CODE", "---"),
            DEVICE.replace("TEST-CODE", "TEST CODE"),
            DEVICE.replace("TEST-CODE", "TEST\nCODE"),
            DEVICE.replace("TEST-CODE", "sk-synthetic-token"),
            DEVICE.replace("TEST-CODE", "eyJhbGciOiJub25lIn0.payload.signature"),
            DEVICE.replace("TEST-CODE", "https://auth.openai.com/codex/device"),
            DEVICE.replace("TEST-CODE", &"A".repeat(MAX_DEVICE_CODE + 1)),
            DEVICE.replace("TEST-CODE", "TEST\x1b[0m\x1b[94mCODE"),
            DEVICE.replace("TEST-CODE", "TEST\rCODE"),
            DEVICE.replace("\x1b[94m", "\x1b[2J"),
            DEVICE.replace("\x1b[90m", "\x1b]8;;https://evil.invalid\x1b\\"),
            DEVICE.replace("\x1b[0m", ""),
            DEVICE.replace("\n", "\r\n"),
            format!("{DEVICE}{DEVICE}"),
            format!("{DEVICE}https://auth.openai.com/codex/device\n"),
            format!("{DEVICE}access_token=synthetic-secret\n"),
            format!("{DEVICE}Error logging in with device code\n"),
            "not a supported prompt".into(),
            r#"{"id":2,"result":{"type":"chatgptDeviceCode"}}"#.into(),
        ] {
            assert!(
                Codex::default()
                    .consume(&mut output.into_bytes(), None)
                    .is_err()
            );
        }
    }

    #[test]
    fn codex_0159_2_does_not_invent_fixed_device_code_group_lengths() {
        // Upstream deserializes user_code as String; ABCD-EFGH is only its test example.
        for code in ["ABCD-EFGH", "ABCD-12345", "123456", "A-BC-DEFG"] {
            let mut output = DEVICE.replace("TEST-CODE", code).into_bytes();
            let Some(Event::Challenge(challenge)) =
                Codex::default().consume(&mut output, None).unwrap()
            else {
                panic!("expected a bounded code");
            };
            assert_eq!(challenge.user_code.as_deref(), Some(code));
        }
    }

    #[test]
    fn codex_0159_2_rejects_late_output_and_a_replayed_prompt() {
        for unexpected in [DEVICE, "\n", "Error logging in\n", "access_token=secret"] {
            let mut protocol = Codex::default();
            let mut output = DEVICE.as_bytes().to_vec();
            protocol.consume(&mut output, None).unwrap();
            output.extend_from_slice(unexpected.as_bytes());
            assert!(
                protocol
                    .consume(&mut output, Some(ExitStatus::from_raw(0)))
                    .is_err()
            );
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

    const CLAUDE_21289: &str = "Opening browser to sign in…\nIf the browser didn't open, visit: https://claude.ai/oauth/authorize?state=synthetic\nPaste code here if prompted > ";

    #[test]
    fn claude_21289_exact_unterminated_prompt_at_every_pipe_boundary() {
        for split in 0..CLAUDE_21289.len() {
            let mut protocol = Claude::default();
            let mut output = CLAUDE_21289.as_bytes()[..split].to_vec();
            assert!(
                matches!(protocol.consume(&mut output, None), Ok(None)),
                "split {split}"
            );
            output.extend_from_slice(&CLAUDE_21289.as_bytes()[split..]);
            let Some(Event::Challenge(challenge)) = protocol.consume(&mut output, None).unwrap()
            else {
                panic!("expected the exact native prompt boundary");
            };
            assert!(challenge.mode == Mode::CodeOrCallback);
            assert_eq!(
                challenge.url,
                "https://claude.ai/oauth/authorize?state=synthetic"
            );
            assert!(challenge.user_code.is_none());
            assert!(protocol.can_submit());
            assert!(output.is_empty());
        }
        let mut protocol = Claude::default();
        let mut output = Vec::new();
        for (index, byte) in CLAUDE_21289.bytes().enumerate() {
            output.push(byte);
            let result = protocol.consume(&mut output, None);
            assert!(if index + 1 == CLAUDE_21289.len() {
                matches!(result, Ok(Some(Event::Challenge(_))))
            } else {
                matches!(result, Ok(None))
            });
        }
    }

    #[test]
    fn claude_21289_callback_and_code_completion_require_native_success_and_exit() {
        for submitted in [false, true] {
            for split in 0..=CLAUDE_NATIVE_SUCCESS.len() {
                let mut protocol = Claude::default();
                let mut output = CLAUDE_21289.as_bytes().to_vec();
                protocol.consume(&mut output, None).unwrap();
                protocol.submitted = submitted;
                output.extend_from_slice(&CLAUDE_NATIVE_SUCCESS[..split]);
                assert!(matches!(protocol.consume(&mut output, None), Ok(None)));
                output.extend_from_slice(&CLAUDE_NATIVE_SUCCESS[split..]);
                assert!(matches!(
                    protocol.consume(&mut output, Some(ExitStatus::from_raw(0))),
                    Ok(Some(Event::Completed))
                ));
            }
        }
        let mut protocol = Claude::default();
        let mut output = format!("{CLAUDE_21289}Login successful.\n").into_bytes();
        assert!(matches!(
            protocol.consume(&mut output, Some(ExitStatus::from_raw(0))),
            Ok(Some(Event::Challenge(_)))
        ));
        assert!(matches!(
            protocol.consume(&mut output, Some(ExitStatus::from_raw(0))),
            Ok(Some(Event::Completed))
        ));
    }

    #[test]
    fn claude_21289_partial_prompt_or_unproven_exit_is_never_completion() {
        for split in 0..CLAUDE_21289.len() {
            let mut output = CLAUDE_21289.as_bytes()[..split].to_vec();
            assert!(
                Claude::default()
                    .consume(&mut output, Some(ExitStatus::from_raw(0)))
                    .is_err()
            );
        }
        for tail in ["", "Login successful.", "\n"] {
            let mut protocol = Claude::default();
            let mut output = CLAUDE_21289.as_bytes().to_vec();
            protocol.consume(&mut output, None).unwrap();
            output.extend_from_slice(tail.as_bytes());
            assert!(
                protocol
                    .consume(&mut output, Some(ExitStatus::from_raw(0)))
                    .is_err()
            );
        }
        let mut protocol = Claude::default();
        let mut output = CLAUDE_21289.as_bytes().to_vec();
        protocol.consume(&mut output, None).unwrap();
        output.extend_from_slice(CLAUDE_NATIVE_SUCCESS);
        assert!(matches!(
            protocol.consume(&mut output, Some(ExitStatus::from_raw(256))),
            Err(Failure::Failed)
        ));
    }

    #[test]
    fn claude_21289_rejects_changed_prompts_urls_and_secret_suffixes() {
        for text in [
            CLAUDE_21289.replace("…", "..."),
            CLAUDE_21289.replace("If the browser didn't open, visit:", "Visit this URL:"),
            CLAUDE_21289.replace("claude.ai", "claude.ai.evil.invalid"),
            CLAUDE_21289.replace(
                "Paste code here if prompted > ",
                "Paste code here if prompted > password: ",
            ),
            CLAUDE_21289.replace("Paste code here if prompted > ", "Enter code: "),
            CLAUDE_21289.replace(
                "Paste code here if prompted > ",
                "Paste code here if prompted >\n",
            ),
            CLAUDE_21289.replace("state=synthetic", "state=synthetic\nwrapped=secret"),
            CLAUDE_21289.replace(
                "state=synthetic",
                "state=synthetic https://claude.ai/oauth/authorize",
            ),
            CLAUDE_21289.replace("state=synthetic", "state=synthetic\x1b[0m"),
        ] {
            let mut output = text.into_bytes();
            assert!(Claude::default().consume(&mut output, None).is_err());
        }
        for suffix in [
            "password: ",
            "access_token=secret",
            "https://claude.ai/oauth/authorize",
            "Login successful.\nsecret",
            "\x1b[2J",
        ] {
            let text = format!("{CLAUDE_21289}{suffix}");
            for split in 0..=text.len() {
                let mut protocol = Claude::default();
                let mut output = text.as_bytes()[..split].to_vec();
                match protocol.consume(&mut output, None) {
                    Err(_) => continue,
                    Ok(Some(Event::Completed)) => panic!("unexpected completion"),
                    Ok(_) => {}
                }
                output.extend_from_slice(&text.as_bytes()[split..]);
                assert!(
                    protocol
                        .consume(&mut output, Some(ExitStatus::from_raw(0)))
                        .is_err(),
                    "suffix split {split}"
                );
            }
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

    fn device_script(tail: &str) -> String {
        format!("printf '%s' '{}'; {tail}", DEVICE.replace('\'', "'\"'\"'"))
    }

    #[test]
    fn synthetic_codex_device_cli_is_reaped_on_successful_exit() {
        let mut login = fixture(ProviderKind::Codex, &device_script("exit 0"));
        let pid = login.child.id();
        assert!(matches!(next_event(&mut login), Ok(Event::Challenge(_))));
        assert_eq!(
            login.submit_code("not-a-device-operation"),
            Err(Failure::InvalidChallenge)
        );
        assert!(matches!(next_event(&mut login), Ok(Event::Completed)));
        assert!(login.stopped);
        assert!(matches!(login.poll(), Err(Failure::Failed)));
        drop(login);
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err());
    }

    #[test]
    fn dropping_device_login_reaps_the_owned_native_cli_without_json_requests() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("unexpected-native-input");
        let script = device_script("read -r unexpected; printf unexpected > \"$1\"; exit 1");
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .args(["-c", &script, "fixture"])
            .arg(&marker);
        let mut login = NativeLogin::spawn(&mut command, ProviderKind::Codex).unwrap();
        let pid = login.child.id();
        assert!(matches!(next_event(&mut login), Ok(Event::Challenge(_))));
        drop(login);
        assert!(!marker.exists());
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err());
    }

    #[test]
    fn codex_native_failure_cannot_complete_or_publish_a_stale_challenge() {
        let mut login = fixture(ProviderKind::Codex, &device_script("exit 1"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while login.watch.exited() != Some(true) {
            assert!(Instant::now() < deadline, "synthetic exit was not observed");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(login.poll(), Err(Failure::Failed)));
        assert!(login.stopped);
    }

    #[test]
    fn private_guard_codes_are_decoded_only_for_owned_guard_children() {
        use super::super::guard::AuthenticationFailure;
        for (code, expected) in [
            (70, AuthenticationFailure::InvalidTokenResponse),
            (71, AuthenticationFailure::ProxyConfiguration),
            (72, AuthenticationFailure::TlsConfiguration),
            (73, AuthenticationFailure::Rejected),
            (74, AuthenticationFailure::Transport),
        ] {
            for guarded in [false, true] {
                let mut command = Command::new("/bin/sh");
                command.env_clear().args(["-c", &format!("exit {code}")]);
                let mut login =
                    NativeLogin::spawn_owned(&mut command, ProviderKind::Codex, guarded).unwrap();
                let result = next_event(&mut login);
                assert!(matches!(result, Err(failure) if failure == if guarded {
                    Failure::Authentication(expected)
                } else {
                    Failure::Failed
                }));
                assert!(login.stopped);
            }
        }
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
            ProviderKind::Claude,
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
