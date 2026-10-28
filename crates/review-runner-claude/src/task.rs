//! Claude framing for generic typed Task Workers. No review-result parser or retry loop.
use super::*;
use crate::hint;
use review_runner::cli::{
    WorkerStderr, classify_without_result, ended_without_result, startup_failure,
    strongest_native_failure, transient_ending, transport_outcome,
};
use review_runner::native_failure::{
    NativeFailureKind, classify_native_failure, says_safeguards_flagged,
};
use review_runner::task::reply_schema::ReplyContract;
use review_runner::task::{ModelWorkerReturn, Unstarted, WorkerAccess, WorkerModelAdapter};

mod model_usage;
mod structured;

/// The adapter-owned tool grant for one Attempt, derived from its access alone. The same list
/// fills `--tools` and `--allowedTools`; a package cannot add Bash, MCP or permission flags.
pub fn task_tools(access: WorkerAccess) -> &'static str {
    match access {
        WorkerAccess::ReadOnly => "Read,Glob,Grep",
        // Build and drive the candidate from a shell. The sandbox is an ephemeral clone whose
        // declared source the kernel requires unchanged at seal; there are no edit tools.
        WorkerAccess::ExecuteChecks => "Read,Glob,Grep,Bash",
        // Native file edits only; the kernel captures the sealed tree as the candidate.
        WorkerAccess::WriteSource => "Read,Glob,Grep,Edit,Write",
        // Edits plus a shell to format, lint and test them; still no MCP or permission flags.
        WorkerAccess::WriteSourceWithShell => "Read,Glob,Grep,Edit,Write,Bash",
    }
}

/// The whole command one Task Attempt runs: [`claude_command`] with the access's tool grant in
/// place of the read-only one, then `--json-schema` when the Worker declares a structured reply
/// or is a review-Campaign reviewer (ADR-0103, ADR-0246).
/// The same restricted root and customization isolation as review; only the tool list widens,
/// and only as far as the kernel-derived access.
fn task_command(
    program: &str,
    model_flags: &[String],
    access: WorkerAccess,
    output_schema: Option<&str>,
    shape: OutputShape,
) -> Command {
    let mut command = claude_command(program, model_flags, shape);
    if let Some(schema) = output_schema {
        command
            .args
            .extend([Arg::literal("--json-schema"), Arg::literal(schema)]);
    }
    let tools = task_tools(access);
    for arg in &mut command.args {
        if arg.value == "Read,Glob,Grep" {
            *arg = Arg::literal(tools);
        }
    }
    command
}

pub struct ClaudeTaskAdapter {
    program: String,
    model_flags: Vec<String>,
    /// The explicit `--model` restriction every reported model's usage is checked against.
    model: String,
    grants: Vec<(String, String)>,
    /// Keep the harness's project directory of each Attempt instead of removing it.
    keep_transcripts: bool,
    /// The registry ID of the Provider this adapter runs for, which its hints name.
    provider: Option<String>,
}

impl ClaudeTaskAdapter {
    /// Refuses a command without an explicit `--model`: usage accounting needs the selected
    /// model to tell its own spend from unexpected model activity. Refuses a `--model` or
    /// `--effort` value outside grammar G ([`hint::printable`]) before any process starts, so a
    /// path or an account identifier never reaches Claude Code or a hint (ADR-0147).
    pub fn new(command: &Command) -> Result<Self, String> {
        let model_flags = claude_model_flags(command)?;
        let model = model_flags
            .chunks_exact(2)
            .find(|pair| pair[0] == "--model")
            .map(|pair| pair[1].clone())
            .ok_or("Claude Task Worker requires an explicit --model")?;
        for pair in model_flags.chunks_exact(2) {
            hint::check_binding(&pair[0], &pair[1])?;
        }
        Ok(Self {
            program: command.program.clone(),
            model_flags,
            model,
            grants: vec![],
            keep_transcripts: false,
            provider: None,
        })
    }

    /// Name the registry ID of the Provider this adapter runs for in its hints (ADR-0148).
    pub fn for_provider(mut self, id: impl Into<String>) -> Self {
        self.provider = Some(id.into());
        self
    }
    pub fn with_auth(mut self, config_dir: Option<String>, user: String, home: String) -> Self {
        self.grants = vec![("USER".into(), user), ("HOME".into(), home)];
        if let Some(config) = config_dir {
            self.grants.push(("CLAUDE_CONFIG_DIR".into(), config));
        }
        self
    }

    /// Keep each Attempt's `projects/<slug>` directory in the Claude config directory
    /// (`[storage] keep_worker_transcripts`); by default it is removed when the process exits.
    pub fn keeping_transcripts(mut self, keep: bool) -> Self {
        self.keep_transcripts = keep;
        self
    }

    /// The harness directory the auth grants point the CLI at, when they name one.
    fn session_store(&self) -> Option<crate::ClaudeSessionStore> {
        let grant = |name: &str| {
            self.grants
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        let home = grant("HOME")?;
        Some(crate::ClaudeSessionStore::from_grants(
            grant("CLAUDE_CONFIG_DIR"),
            home,
        ))
    }

    /// After the Claude process of an Attempt in `workdir` exits, whatever the outcome, remove
    /// the history the CLI kept for that working directory (ADR-0144). Only a directory af
    /// created for this Attempt alone qualifies; a failure is reported, never the Attempt's.
    fn effort(&self) -> Option<&str> {
        self.model_flags
            .chunks_exact(2)
            .find(|pair| pair[0] == "--effort")
            .map(|pair| pair[1].as_str())
    }

    /// The hint for an Attempt the bound model did not answer: another model answered, the
    /// safety classifier refused, or Claude Code does not know the model (ADR-0147).
    fn hint(
        &self,
        envelope: Option<&serde_json::Value>,
        accounting: &model_usage::Accounting,
        failure: Option<NativeFailureKind>,
        stream_category: Option<&str>,
    ) -> Option<String> {
        if accounting.switched {
            return accounting.error.clone();
        }
        let bound = hint::BoundModel {
            model: &self.model,
            effort: self.effort(),
            provider: Some(crate::models::ProviderRef {
                id: self.provider.as_deref(),
                kind: crate::models::Kind::Claude,
            }),
        };
        let outcome = match failure? {
            NativeFailureKind::ModelRefused => {
                let value = envelope?;
                hint::ModelOutcome::Refused {
                    safety_stops: value
                        .get("safety_stops")
                        .and_then(serde_json::Value::as_u64),
                    stop_reason_refusal: value
                        .get("stop_reason")
                        .and_then(serde_json::Value::as_str)
                        == Some("refusal"),
                    category: refusal_category(value).or(stream_category),
                }
            }
            NativeFailureKind::ModelUnavailable => hint::ModelOutcome::Unavailable,
            _ => return None,
        };
        Some(hint::compose(bound, &outcome))
    }

    fn remove_transcripts(&self, workdir: &Path) {
        if self.keep_transcripts {
            return;
        }
        let Some(store) = self.session_store() else {
            return;
        };
        if let Err(detail) = store.remove_attempt_project(workdir) {
            eprintln!("af: the Claude Worker's transcript was not removed: {detail}");
        }
    }
}

impl WorkerModelAdapter for ClaudeTaskAdapter {
    fn credential_mode(&self) -> review_core::CredentialModeV1 {
        review_core::CredentialModeV1::TrustedUnsafe
    }

    fn provider_kind(&self) -> &'static str {
        "claude"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        let effort = self
            .model_flags
            .chunks_exact(2)
            .find(|args| args[0] == "--effort")
            .map(|args| args[1].clone())?;
        Some((self.model.clone(), effort))
    }
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        self.invoke_started(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
        )
        .unwrap_or_else(|unstarted| *unstarted.0)
    }
    fn invoke_started(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
    ) -> Result<ModelWorkerReturn, Unstarted> {
        self.invoke_constrained(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
            ReplyContract::Request,
        )
    }
    fn invoke_constrained(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
        contract: ReplyContract,
    ) -> Result<ModelWorkerReturn, Unstarted> {
        if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Ok(ModelWorkerReturn {
                notes: Default::default(),
                usage_observation: None,
                message: Err("Worker invocation was cancelled before starting".into()),
                usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
                raw_artifact_ids: vec![],
                native_failure: None,
            });
        }
        let output_schema = match structured::output_schema(contract, &input) {
            Ok(schema) => schema,
            Err(error) => {
                return Ok(ModelWorkerReturn {
                    notes: Default::default(),
                    usage_observation: None,
                    message: Err(error),
                    usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
                    raw_artifact_ids: vec![],
                    native_failure: None,
                });
            }
        };
        let shape = OutputShape::now();
        let command = task_command(
            &self.program,
            &self.model_flags,
            access,
            output_schema.as_deref(),
            shape,
        );
        // Native 2.1.272 uses the first two guards to latch automatic title generation before
        // its auxiliary inference. They preserve OAuth/auth grants, unlike --bare. The third
        // turns off Claude Code's automatic model fallback: when the bound model's safety
        // classifier flags a request, the client would re-run it on another model and keep the
        // session there (2.1.300 gates that whole feature on this variable). With it set, a
        // flagged request ends as a refusal on the bound model instead, which af classifies
        // `model_refused`. CLAUDE_CODE_NO_MODEL_FALLBACK is deliberately not set: it disables
        // every model substitution, compaction's included. All three are a bounded client
        // mitigation, not proof that every internal model request is disabled or that no other
        // model answers; the identity check in `model_usage::account` stays the authority
        // (ADR-0102, ADR-0147). The working directory is the sandbox root the kernel
        // materialized.
        let mut runner = ModelRunner::new(workdir, timeout)
            .with_harness("anthropic", self.model_settings().map(|(model, _)| model))
            .with_env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .with_env("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1")
            .with_env("CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK", "1");
        if access.has_shell() {
            // A shell child must not outlive the Attempt that started it.
            runner = runner.killing_process_group_on_exit();
        }
        for (name, value) in &self.grants {
            // The config directory is redacted from everything stored, in every spelling, as
            // `CODEX_HOME` is; `HOME` and `USER` are ordinary context a finding may name.
            runner = if name == "CLAUDE_CONFIG_DIR" {
                runner.with_grant(name, value)
            } else {
                runner.with_env(name, value)
            };
        }
        // Sandbox-local, non-secret context resolved by the kernel for this exact Attempt.
        for (name, value) in environment {
            runner = runner.with_env(name, value);
        }
        let mut parsed = None;
        let mut enveloped = false;
        let mut refused = false;
        let mut failure: Option<NativeFailureKind> = None;
        let mut said = WorkerStderr::default();
        // Read from the complete capture before any filter: whether stdout held anything, the
        // native text a `Retry-After` may be named in, and the refusal category a stream event's
        // `stop_details` names (absent from the `json` result).
        let mut stdout_blank = false;
        let mut ending_text = String::new();
        let mut stream_category: Option<String> = None;
        let capture = runner.capture_observed(
            cas,
            &command,
            input,
            cancellation,
            &review_runner::stream::HarnessStream::claude(task_tools(access)),
            |failed, stdout, stderr| {
                stdout_blank = stdout.iter().all(u8::is_ascii_whitespace);
                stream_category = stream_refusal_category(stdout);
                // The raw-output rule: of stdout the Store keeps the result line alone (or the
                // `json` document); Telemetry has the rest. Taken from the complete capture,
                // before the adapter reads it.
                let kept = result_output(stdout, shape);
                // What the adapter reads of the complete in-memory capture: the result line,
                // else the native text around the messages, and whether the invocation's shape
                // lets it be the envelope.
                let native = native_output(stdout, shape);
                let framed_result = native.envelope;
                let mut read = match native.bytes {
                    std::borrow::Cow::Owned(read) => read,
                    std::borrow::Cow::Borrowed(_) => std::mem::take(stdout),
                };
                'classify: {
                    // Keep the original in-memory usage envelope, but never capture an auth
                    // challenge or token-bearing error as ordinary Task evidence. Under
                    // `stream-json` only a `result` line is an envelope: native text, a typeless
                    // JSON object among it, never is.
                    parsed = framed_result
                        .then(|| serde_json::from_slice::<serde_json::Value>(&read).ok())
                        .flatten();
                    enveloped = parsed
                        .as_ref()
                        .is_some_and(|value| is_envelope(value, output_schema.is_some()));
                    let envelope = parsed.as_ref().filter(|_| enveloped);
                    let native_failed = envelope.is_some_and(|value| {
                        value.get("is_error").and_then(serde_json::Value::as_bool) == Some(true)
                    });
                    // A classifier refusal is never a success, whatever `is_error` says.
                    refused = envelope.is_some_and(refusal_reported);
                    // The Worker's own last words, bounded and filtered, before the privacy
                    // filter below may replace the stream.
                    said = WorkerStderr::read(stderr, &read);
                    ending_text = format!(
                        "{}\n{}",
                        String::from_utf8_lossy(stderr),
                        String::from_utf8_lossy(&read)
                    );
                    let stderr_kind = classify_native_failure(&String::from_utf8_lossy(stderr));
                    let (kind, auth_seen) = match envelope {
                        // No parsable result, whatever the exit status or transport outcome: the
                        // one place its classification is decided. Stdout is native output here.
                        None => {
                            let native_stdout = String::from_utf8_lossy(&read);
                            let candidates = [classify_native_failure(&native_stdout), stderr_kind];
                            (
                                classify_without_result(stderr, &native_stdout, &candidates),
                                strongest_native_failure(&candidates).is_auth(),
                            )
                        }
                        // A parsed envelope speaks only through its native fields, so model text
                        // cannot choose the category. A reported refusal is one whatever else
                        // the streams say.
                        Some(_) if refused => (NativeFailureKind::ModelRefused, false),
                        Some(value) if failed || native_failed => {
                            let kind = strongest_native_failure(&[
                                native_failure(value, native_failed),
                                stderr_kind,
                            ]);
                            (kind, kind.is_auth())
                        }
                        Some(_) => break 'classify,
                    };
                    failure = (kind != NativeFailureKind::Unknown).then_some(kind);
                    kind.redact_capture(auth_seen, &mut read, stderr);
                }
                // The adapter's whole-capture replacement is what the Store keeps when it took
                // one; otherwise only the result line, never native text or another message.
                *stdout = if NativeFailureKind::replaced_capture(&read, stderr) {
                    read
                } else {
                    kept
                };
            },
        );
        // The process has exited, whatever its outcome: what it kept in the operator's Claude
        // history for this Attempt's working directory goes now.
        self.remove_transcripts(workdir);
        let accounting = model_usage::account(parsed.as_ref(), &self.model, self.effort());
        let envelope = parsed.as_ref().filter(|_| enveloped);
        let success = accounting.error.is_none()
            && !refused
            && accounting.observation.is_none()
            && capture.status.as_ref().is_ok_and(|status| status.success())
            && envelope.is_some_and(|v| {
                v.get("is_error").and_then(serde_json::Value::as_bool) == Some(false)
            });
        let message = if success {
            structured::message(
                envelope.expect("successful envelope"),
                output_schema.is_some(),
            )
        } else if let Some(hint) =
            self.hint(envelope, &accounting, failure, stream_category.as_deref())
        {
            // The bound model did not answer: one af-authored hint leads the diagnostic, so the
            // calling site can print it again (ADR-0147).
            let ended = match envelope {
                Some(_) => format!("transport: {}", transport_outcome(&capture.status)),
                None => format!("it {}", ended_without_result(&capture.status, &said)),
            };
            let mut error = format!("{}{hint}; {ended}", hint::DIAGNOSTIC_PREFIX);
            if !accounting.switched
                && let Some(accounting_error) = &accounting.error
            {
                error.push_str("; ");
                error.push_str(accounting_error);
            }
            Err(error)
        } else if envelope.is_none() {
            // No envelope, whether stdout was empty, prose or JSON of another shape (`{}`), and
            // whatever the exit: the process's exit and its own last words are all there is,
            // under the one classification decided above.
            Err(startup_failure(
                "Claude Worker",
                &CLAUDE_INVOCATION,
                &capture.status,
                &said,
                failure.unwrap_or(NativeFailureKind::Unknown),
            ))
        } else if let Some(kind) = failure.filter(|kind| *kind != NativeFailureKind::Unknown) {
            let mut error = format!(
                "Claude Worker failed: {}; transport: {}",
                kind.diagnostic(),
                transport_outcome(&capture.status)
            );
            if let Some(accounting_error) = &accounting.error {
                error.push_str("; ");
                error.push_str(accounting_error);
            } else if accounting.observation.is_some() {
                error.push_str("; Claude native usage is absent or malformed");
            }
            Err(error)
        } else if let Some(error) = accounting.error {
            Err(error)
        } else {
            Err(format!(
                "Claude Worker failed; transport: {}",
                transport_outcome(&capture.status)
            ))
        };
        let native_failure = (!success).then_some(failure).flatten();
        // Only a call with no reply may be retried; a reply the model gave is never discarded.
        let transient = (message.is_err() && envelope.is_none_or(|_| native_failure.is_some()))
            .then(|| {
                transient_ending(
                    &capture.status,
                    native_failure.unwrap_or(NativeFailureKind::Unknown),
                    stdout_blank,
                    &ending_text,
                )
            })
            .flatten();
        let refusal_category = (native_failure == Some(NativeFailureKind::ModelRefused))
            .then(|| {
                envelope
                    .and_then(refusal_category)
                    .map(str::to_owned)
                    .or(stream_category.clone())
            })
            .flatten();
        let returned = ModelWorkerReturn {
            usage_observation: accounting.observation,
            native_failure,
            message,
            usage: accounting.usage,
            raw_artifact_ids: capture.raw_artifact_ids,
            notes: review_runner::task::CallNotes {
                transient,
                events: Vec::new(),
                refusal_category,
                environment: native_failure == Some(NativeFailureKind::Environment),
            },
        };
        if capture.started {
            Ok(returned)
        } else {
            Err(Unstarted(Box::new(returned)))
        }
    }
}

/// The raw-output rule for Claude (design §7.3): of an Attempt's complete stdout under `shape`,
/// what the Store keeps. Under `--output-format stream-json` every line is one message: the last
/// `result` line, the envelope `--output-format json` prints, is kept alone, ending in its
/// newline, and without one nothing is. Under `json` a stdout that frames no message is kept
/// too when it is one JSON object (an envelope without a `type`), the document accounting
/// reads. Nothing else is kept: no other message, and no native text around or instead of the
/// messages, which the adapter classifies from the in-memory capture ([`native_output`]). So the
/// same final result gives the same bytes under both shapes and at every level.
pub fn result_output(stdout: &[u8], shape: OutputShape) -> Vec<u8> {
    let (framed, result) = result_line(stdout);
    if let Some(kept) = result {
        return kept;
    }
    if shape == OutputShape::Json
        && !framed
        && serde_json::from_slice::<serde_json::Value>(stdout)
            .is_ok_and(|document| document.is_object())
    {
        return stdout.to_vec();
    }
    Vec::new()
}

/// Whether `stdout` frames any message, and its last `result` line ending in a newline.
fn result_line(stdout: &[u8]) -> (bool, Option<Vec<u8>>) {
    use review_runner::stream::{lines, message_type};
    let mut framed = false;
    let mut result = None;
    for line in lines(stdout) {
        if let Some(kind) = message_type(line) {
            framed = true;
            if kind == "result" {
                result = Some(line);
            }
        }
    }
    let result = result.map(|line| {
        let mut line = line.to_vec();
        if !line.ends_with(b"\n") {
            line.push(b'\n');
        }
        line
    });
    (framed, result)
}

/// What the adapter reads of an Attempt's complete in-memory stdout ([`native_output`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeOutput<'a> {
    /// The bytes its envelope parsing, accounting, refusal classification and model-identity
    /// check read.
    pub bytes: std::borrow::Cow<'a, [u8]>,
    /// Whether `bytes` may be parsed as the result envelope: the last `result` line under either
    /// shape, and under `json` whatever stdout holds. Under `stream-json` with no `result` line
    /// it is native text only, whatever it parses as.
    pub envelope: bool,
}

/// What the adapter's envelope parsing, accounting, refusal classification and model-identity
/// check read of an Attempt's complete in-memory stdout under `shape`: the last `result` line
/// when the stdout frames messages and has one; with no `result` line, the lines that are no
/// message (native text a failing CLI wrote), as they were; a stdout that frames no message at
/// all — a `json` envelope without a `type`, plain text, a document over several lines —
/// whole. Only the `json` invocation's stdout is an envelope without a `result` line. The Store
/// keeps less ([`result_output`]).
pub fn native_output(stdout: &[u8], shape: OutputShape) -> NativeOutput<'_> {
    use review_runner::stream::{lines, message_type};
    let (framed, result) = result_line(stdout);
    let envelope = shape == OutputShape::Json || result.is_some();
    let bytes = if !framed {
        std::borrow::Cow::Borrowed(stdout)
    } else if let Some(line) = result {
        std::borrow::Cow::Owned(line)
    } else {
        std::borrow::Cow::Owned(
            lines(stdout)
                .filter(|line| message_type(line).is_none())
                .flatten()
                .copied()
                .collect(),
        )
    };
    NativeOutput { bytes, envelope }
}

/// Whether a result reports that the bound model's safety classifier flagged the request: a
/// positive `safety_stops`, `stop_reason` `refusal`, or, in an error result, Claude Code's own
/// "safeguards flagged this message" text, in its `result` whatever `errors` or `error` hold, or
/// in the error text [`native_failure`] reads. `is_error` does not decide the first two.
fn refusal_reported(value: &serde_json::Value) -> bool {
    let is_error = value.get("is_error").and_then(serde_json::Value::as_bool) == Some(true);
    value
        .get("safety_stops")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|stops| stops >= 1)
        || value.get("stop_reason").and_then(serde_json::Value::as_str) == Some("refusal")
        || is_error
            && value
                .get("result")
                .and_then(serde_json::Value::as_str)
                .is_some_and(says_safeguards_flagged)
        || is_error && native_failure(value, true) == NativeFailureKind::ModelRefused
}

/// The policy category a refusal result names, when it names one: `stop_details.category` as
/// the API reports it, or the client's `api_refusal_category`.
fn refusal_category(value: &serde_json::Value) -> Option<&str> {
    value
        .get("stop_details")
        .and_then(|details| details.get("category"))
        .or_else(|| value.get("api_refusal_category"))
        .and_then(serde_json::Value::as_str)
}

/// The policy category the stream named for a refusal: `stop_details.category` of any message,
/// at its top level or in its `message`. Under `--output-format stream-json` the category lives
/// in the assistant message's `stop_details`, which the `json` result leaves out.
fn stream_refusal_category(stdout: &[u8]) -> Option<String> {
    use review_runner::stream::lines;
    let mut category = None;
    for line in lines(stdout) {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let named = [
            value.get("stop_details"),
            value.pointer("/message/stop_details"),
        ]
        .into_iter()
        .flatten()
        .find_map(|details| details.get("category")?.as_str());
        if let Some(named) = named {
            category = Some(named.to_owned());
        }
    }
    category
}

/// Whether stdout is a result envelope: a JSON object carrying the fields the adapter reads, a
/// boolean `is_error` and the `result`, or for a schema-constrained Attempt its
/// `structured_output`. Any other JSON, `{}` among it, is no parsable result.
fn is_envelope(value: &serde_json::Value, typed: bool) -> bool {
    value
        .get("is_error")
        .is_some_and(serde_json::Value::is_boolean)
        && (value.get("result").is_some() || typed && value.get("structured_output").is_some())
}

/// In an `is_error` envelope, `result` is the CLI's own error text whenever `errors` and `error`
/// yield no string (absent, `null`, `[]`, an array without strings, an object without a string
/// `message`); anywhere else it is the model's reply and is never classified.
fn native_failure(value: &serde_json::Value, is_error: bool) -> NativeFailureKind {
    let strings = |key: &str| -> Vec<&str> {
        let Some(value) = value.get(key) else {
            return Vec::new();
        };
        if let Some(message) = value.as_str() {
            vec![message]
        } else if let Some(values) = value.as_array() {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        } else {
            value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .into_iter()
                .collect()
        }
    };
    let mut messages = strings("errors");
    messages.extend(strings("error"));
    if is_error && messages.is_empty() {
        messages.extend(strings("result"));
    }
    messages.extend(strings("message"));
    let kinds: Vec<_> = messages.into_iter().map(classify_native_failure).collect();
    strongest_native_failure(&kinds)
}

fn parse_usage(
    value: Option<&serde_json::Value>,
) -> (
    Option<review_core::task::usage::TaskTokenUsageV3>,
    Option<review_core::task::usage::TaskUsageObservationV1>,
) {
    use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
    use review_runner::task::usage::NativeCounter;
    let Some(value) = value else {
        return (None, None);
    };
    let Some(usage) = value.get("usage").filter(|value| value.is_object()) else {
        return (
            None,
            Some(TaskUsageObservationV1 {
                reported_usage: None,
                charge_complete: false,
            }),
        );
    };
    let input = NativeCounter::read(usage, "input_tokens").value();
    let output = NativeCounter::read(usage, "output_tokens").value();
    let write = NativeCounter::read(usage, "cache_creation_input_tokens");
    let read = NativeCounter::read(usage, "cache_read_input_tokens");
    let complete = input.is_some() && output.is_some() && write.optional_zero().is_some();
    let malformed = !complete || read == NativeCounter::Invalid;
    let reported =
        (input.is_some() || output.is_some() || write.value().is_some() || read.value().is_some())
            .then(|| TaskTokenUsageV3 {
                input_tokens: input.map(|n| u128::from(n).into()),
                output_tokens: output.map(|n| u128::from(n).into()),
                cache_read_tokens: read.value().map(|n| u128::from(n).into()),
                cache_write_tokens: write.value().map(|n| u128::from(n).into()),
                reasoning_tokens: None,
                // Claude input excludes cache reads; cache creation is an additional billed
                // component. Keep each independently valid contribution when another is malformed.
                chargeable_tokens: (u128::from(input.unwrap_or(0))
                    + u128::from(output.unwrap_or(0))
                    + u128::from(write.value().unwrap_or(0)))
                .into(),
            });
    let observation = malformed.then(|| TaskUsageObservationV1 {
        reported_usage: reported.clone(),
        charge_complete: complete,
    });
    (reported, observation)
}

#[cfg(test)]
mod usage_tests;

#[cfg(test)]
mod invocation_tests {
    use super::*;
    use std::collections::BTreeSet;

    /// [`CLAUDE_INVOCATION`] is the single source of truth for what `claude --help` must declare:
    /// every option any Attempt's command can emit is in it, and nothing else is.
    #[test]
    fn the_required_flag_list_is_exactly_what_the_command_builder_emits() {
        let model_flags: Vec<String> = ["--model", "claude-fixture-1", "--effort", "high"]
            .map(String::from)
            .into();
        let mut emitted = BTreeSet::new();
        for access in [
            WorkerAccess::ReadOnly,
            WorkerAccess::ExecuteChecks,
            WorkerAccess::WriteSource,
            WorkerAccess::WriteSourceWithShell,
        ] {
            for (schema, shape) in
                [None, Some("{\"type\":\"object\"}")]
                    .into_iter()
                    .flat_map(|schema| {
                        [OutputShape::Json, OutputShape::StreamJson].map(|shape| (schema, shape))
                    })
            {
                let command = task_command("claude", &model_flags, access, schema, shape);
                for value in command.resolve().unwrap() {
                    if value.starts_with('-') {
                        assert!(
                            CLAUDE_INVOCATION.declares(&value),
                            "`{value}` is emitted but not required of the installed CLI"
                        );
                        emitted.insert(value);
                    }
                }
            }
        }
        let required: BTreeSet<String> = CLAUDE_INVOCATION
            .flags
            .iter()
            .map(|flag| flag.spelling.to_string())
            .collect();
        assert_eq!(emitted, required, "a required flag is never emitted");
        // The isolation flags are among them, so a CLI without them is refused, not weakened;
        // and so are both options of the stream shape, which `-p` accepts only together.
        for isolation in ["--safe-mode", "--restricted", "--tools", "--allowedTools"] {
            assert!(CLAUDE_INVOCATION.declares(isolation), "{isolation}");
        }
        for stream in ["--output-format", "--verbose"] {
            assert!(CLAUDE_INVOCATION.declares(stream), "{stream}");
        }
    }

    /// The two shapes differ only in what Claude is asked to print.
    #[test]
    fn the_stream_shape_asks_for_stream_json_with_verbose_and_changes_nothing_else() {
        let model_flags: Vec<String> = ["--model", "claude-fixture-1"].map(String::from).into();
        let json = task_command(
            "claude",
            &model_flags,
            WorkerAccess::ReadOnly,
            None,
            OutputShape::Json,
        )
        .resolve()
        .unwrap();
        let stream = task_command(
            "claude",
            &model_flags,
            WorkerAccess::ReadOnly,
            None,
            OutputShape::StreamJson,
        )
        .resolve()
        .unwrap();
        assert_eq!(&json[..3], ["-p", "--output-format", "json"]);
        assert_eq!(
            &stream[..4],
            ["-p", "--output-format", "stream-json", "--verbose"]
        );
        assert_eq!(json[3..], stream[4..]);
    }
}

#[cfg(test)]
mod refusal_category_tests {
    use super::*;

    /// ADR-0208: under `stream-json` the refusal category lives in the assistant message's
    /// `stop_details`, which the result line leaves out; the adapter reads it from any message,
    /// the last one naming it winning, and a stream that names none has none.
    #[test]
    fn the_refusal_category_is_read_from_the_stream_messages() {
        let stream = concat!(
            r#"{"type":"system","subtype":"init"}"#,
            "\n",
            r#"{"type":"assistant","message":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}}"#,
            "\n",
            r#"{"type":"result","subtype":"success","is_error":true,"result":"","safety_stops":1}"#,
            "\n"
        );
        assert_eq!(
            stream_refusal_category(stream.as_bytes()).as_deref(),
            Some("cyber")
        );
        let top = r#"{"type":"result","stop_details":{"category":"bio"}}"#;
        assert_eq!(
            stream_refusal_category(top.as_bytes()).as_deref(),
            Some("bio")
        );
        assert_eq!(
            stream_refusal_category(br#"{"type":"result","is_error":false}"#),
            None
        );
        assert_eq!(stream_refusal_category(b"not json"), None);
    }
}
