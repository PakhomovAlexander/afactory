//! The performance laboratory's native supervisor (ADR-0166).
//!
//! One authority-built binary measures both sides of a comparison: the laboratory builds it once
//! from the authority source, hashes it, and launches the same bytes for every baseline and
//! candidate sample. One process supervises one sample. It reads one bounded
//! `af.perf-supervisor-request/4` from stdin and writes, to stdout, one
//! `af.perf-supervisor-result/5` line followed by exactly the retained bytes of the child's
//! stdout and then its stderr, whose lengths the line declares. The timing, draining and
//! lifecycle semantics are `review_process::sample`'s; this file only parses, refuses and
//! reports.
//!
//! A request may name a fixture `setup` (ADR-0168): one more child, run first under the same
//! containment and in the same scratch, untimed and reported apart. Only a setup that exits 0
//! with a verified lifecycle lets the measured child launch; otherwise the result is
//! `setup_failed` and nothing is measured. The measured interval never includes it. A request
//! may also name up to eight `stdout_patterns`, whose occurrences in the child's whole stdout,
//! retained or not, the result counts, so an oracle can check a count in output larger than any
//! retained log, and up to eight `stdout_json` selectors of declared fields, which the result
//! observes over the whole stdout as one JSON document (`review_process::json_stream`,
//! ADR-0172): whether it is complete, and what each selector's path reaches, so an oracle can
//! judge the selected semantics of such output without freezing its formatting.
//!
//! A SIGTERM, SIGINT or SIGHUP cancels the sample through the same supervision: a running child
//! is signalled, killed after its cleanup grace and verified gone, a child not yet launched is
//! never launched, and the reply says how the sample ended. The host cancels a supervisor this
//! way at its backstop or the run's total deadline, never by killing it (ADR-0171).
//!
//! The request must be canonical JSON (sorted keys, compact, no duplicate keys) and is refused
//! with exit 2 and no stdout when it is not, or when any field is unknown or out of bounds. In
//! `pid_namespace` containment the supervisor must be PID 1 of its Linux PID namespace, which
//! `review_process::sample` itself enforces; it then also owns the sample scratch: the scratch
//! must be empty before the launch, and the supervisor clears it from inside the namespace only
//! after verifying that no other process remains, so no candidate process can race the removal
//! and the host removes no candidate-controlled path.

use review_process::json_stream::{self, Kind, Observer, Selected, Selector};
use review_process::sample::{
    Containment, Control, ExitObservation, LifecycleStatus, NOT_NAMESPACE_INIT, SampleOutcome,
    SampleSpec, Termination, forward_termination_signals, linux_observations, namespace_init,
    run_sample,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const REQUEST_SCHEMA: &str = "af.perf-supervisor-request/4";
const RESULT_SCHEMA: &str = "af.perf-supervisor-result/5";
const RECORD_BYTES_MAX: usize = 64 * 1024;
const LOG_BYTES_MAX: u64 = 1024 * 1024;
const PATTERNS_MAX: usize = 8;
const PATTERN_BYTES_MAX: usize = 128;
const TIMEOUT_MS_MAX: u64 = 120_000;
const GRACE_MS_MAX: u64 = 30_000;
const DETAIL_MAX: usize = 1024;
/// What a fixture setup's output may retain: only its diagnostics are reported, never judged.
const SETUP_STREAM_CAP: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: String,
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
    cwd: String,
    stdin: String,
    timeout_ms: u64,
    stdout_cap_bytes: u64,
    log_cap_bytes: u64,
    cleanup_grace_ms: u64,
    containment: String,
    scratch: Option<String>,
    setup: Option<Setup>,
    stdout_patterns: Vec<String>,
    stdout_json: Vec<JsonSelector>,
}

/// One declared field of the child's stdout as a JSON document (ADR-0172).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonSelector {
    path: String,
    kind: String,
    equals: Vec<String>,
}

/// The untimed fixture step before the measured child (ADR-0168).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup {
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
    cwd: String,
    timeout_ms: u64,
}

/// A parsed request: the measured child and, when declared, the setup that precedes it.
struct Parsed {
    spec: SampleSpec,
    setup: Option<SampleSpec>,
}

fn main() -> ExitCode {
    let entered = Instant::now();
    let control = Control::new();
    if let Err(error) = forward_termination_signals(std::sync::Arc::clone(&control)) {
        eprintln!("perf-supervisor: cannot forward termination signals: {error}");
        return ExitCode::from(1);
    }
    let mut input = Vec::new();
    if let Err(error) = std::io::stdin()
        .take(RECORD_BYTES_MAX as u64 + 1)
        .read_to_end(&mut input)
    {
        eprintln!("perf-supervisor: cannot read the request: {error}");
        return ExitCode::from(2);
    }
    let (request, parsed) = match parse(&input) {
        Ok(parsed) => parsed,
        Err(reason) => {
            eprintln!("perf-supervisor: refused request: {reason}");
            return ExitCode::from(2);
        }
    };
    #[cfg(target_os = "linux")]
    let _ = nix::sys::prctl::set_dumpable(false);

    let before = observations();
    let supervised = supervise(&request, &parsed, &control);
    let after = observations();
    let line = result(&parsed.spec, &supervised, entered, before, after);
    let outcome = &supervised.outcome;
    let mut stdout = std::io::stdout().lock();
    let written = stdout
        .write_all(&line)
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.write_all(&outcome.stdout.retained))
        .and_then(|()| stdout.write_all(&outcome.stderr.retained))
        .and_then(|()| stdout.flush());
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

fn parse(input: &[u8]) -> Result<(Request, Parsed), String> {
    if input.len() > RECORD_BYTES_MAX {
        return Err("the request exceeds 64 KiB".into());
    }
    let request: Request =
        serde_json::from_slice(input).map_err(|error| format!("malformed: {error}"))?;
    let canonical = serde_json::to_vec(
        &serde_json::from_slice::<Value>(input).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if canonical != input {
        return Err("the request is not canonical JSON".into());
    }
    if request.schema != REQUEST_SCHEMA {
        return Err(format!(
            "schema {:?} is not {REQUEST_SCHEMA}",
            request.schema
        ));
    }
    check_command(&request.argv, &request.environment, &request.cwd)?;
    if let Some(setup) = &request.setup {
        check_command(&setup.argv, &setup.environment, &setup.cwd)
            .map_err(|reason| format!("setup: {reason}"))?;
        if !(1..=TIMEOUT_MS_MAX).contains(&setup.timeout_ms) {
            return Err("setup: timeout_ms is 1..120000".into());
        }
    }
    if !(1..=TIMEOUT_MS_MAX).contains(&request.timeout_ms)
        || !(1..=GRACE_MS_MAX).contains(&request.cleanup_grace_ms)
    {
        return Err("timeout_ms is 1..120000 and cleanup_grace_ms 1..30000".into());
    }
    if !(1..=LOG_BYTES_MAX).contains(&request.stdout_cap_bytes)
        || !(request.stdout_cap_bytes..=LOG_BYTES_MAX).contains(&request.log_cap_bytes)
    {
        return Err(
            "stdout_cap_bytes is 1..1 MiB and log_cap_bytes stdout_cap_bytes..1 MiB".into(),
        );
    }
    if request.stdout_patterns.len() > PATTERNS_MAX
        || request
            .stdout_patterns
            .iter()
            .any(|pattern| pattern.is_empty() || pattern.len() > PATTERN_BYTES_MAX)
    {
        return Err("stdout_patterns holds at most 8 patterns of 1 to 128 bytes".into());
    }
    if request.stdout_json.len() > json_stream::SELECTORS_MAX {
        return Err("stdout_json holds at most 8 selectors".into());
    }
    let selectors = request
        .stdout_json
        .iter()
        .map(|selector| {
            let kind = match selector.kind.as_str() {
                "integer" => Kind::Integer,
                "string" => Kind::String,
                other => return Err(format!("stdout_json kind {other:?} is not declared")),
            };
            let equals = selector
                .equals
                .iter()
                .map(|value| value.as_bytes().to_vec())
                .collect();
            Selector::parse(&selector.path, kind, equals)
                .map_err(|reason| format!("stdout_json: {reason}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let containment = match request.containment.as_str() {
        "process_group" => Containment::ProcessGroup,
        "pid_namespace" => Containment::PidNamespace,
        other => return Err(format!("containment {other:?} is not declared")),
    };
    match (&request.scratch, containment) {
        (None, _) => {}
        (Some(path), Containment::PidNamespace) if path.starts_with('/') && path.len() > 1 => {}
        _ => return Err("only pid_namespace containment owns an absolute scratch".into()),
    }
    let spec = SampleSpec {
        program: request.argv[0].clone().into(),
        args: request.argv[1..].iter().map(Into::into).collect(),
        env: request
            .environment
            .iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        stdin: request.stdin.clone().into_bytes(),
        timeout: Duration::from_millis(request.timeout_ms),
        stdout_cap: request.stdout_cap_bytes as usize,
        stderr_cap: (request.log_cap_bytes - request.stdout_cap_bytes) as usize,
        containment,
        cleanup_grace: Duration::from_millis(request.cleanup_grace_ms),
        stdout_patterns: request
            .stdout_patterns
            .iter()
            .map(|pattern| pattern.as_bytes().to_vec())
            .collect(),
        stdout_json: selectors,
    };
    let setup = request.setup.as_ref().map(|setup| SampleSpec {
        program: setup.argv[0].clone().into(),
        args: setup.argv[1..].iter().map(Into::into).collect(),
        env: setup
            .environment
            .iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        stdin: Vec::new(),
        timeout: Duration::from_millis(setup.timeout_ms),
        stdout_cap: SETUP_STREAM_CAP,
        stderr_cap: SETUP_STREAM_CAP,
        containment,
        cleanup_grace: Duration::from_millis(request.cleanup_grace_ms),
        stdout_patterns: Vec::new(),
        stdout_json: Vec::new(),
    });
    Ok((request, Parsed { spec, setup }))
}

/// An absolute program of 1 to 64 arguments, an absolute directory and a bounded environment
/// of declared names, none of it holding NUL.
fn check_command(
    argv: &[String],
    environment: &BTreeMap<String, String>,
    cwd: &str,
) -> Result<(), String> {
    if argv.is_empty() || argv.len() > 64 {
        return Err("argv holds 1 to 64 arguments".into());
    }
    if !argv[0].starts_with('/') || !cwd.starts_with('/') {
        return Err("the program and cwd are absolute paths".into());
    }
    let text = argv
        .iter()
        .chain(environment.values())
        .map(String::as_str)
        .chain(std::iter::once(cwd));
    for value in text {
        if value.len() > 4096 || value.contains('\0') {
            return Err("an argument, value or path is longer than 4096 bytes or holds NUL".into());
        }
    }
    if environment.len() > 64
        || environment.keys().any(|key| {
            key.is_empty()
                || key.len() > 64
                || key.starts_with(|first: char| first.is_ascii_digit())
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        })
    {
        return Err("environment keys are at most 64 names matching [A-Z_][A-Z0-9_]*".into());
    }
    Ok(())
}

/// What one supervisor run produced: the measured child's outcome (or its refusal), the
/// scratch report, and the fixture setup's outcome when one was declared and launched.
struct Supervised {
    outcome: SampleOutcome,
    scratch: Value,
    setup: Option<SampleOutcome>,
    /// The setup did not end as required, so the measured child was never launched.
    setup_failed: Option<String>,
}

/// Run the sample, refusing it first when this process cannot provide its containment or its
/// scratch is not empty, and clear the scratch only after a verified-empty namespace. A
/// declared setup runs first, in its own directory, and the measured child launches only after
/// it exited 0 with a verified lifecycle.
fn supervise(request: &Request, parsed: &Parsed, control: &Control) -> Supervised {
    let spec = &parsed.spec;
    let plain = |outcome: SampleOutcome, scratch: Value| Supervised {
        outcome,
        scratch,
        setup: None,
        setup_failed: None,
    };
    // The library refuses it too; refusing here first leaves the scratch untouched.
    if spec.containment == Containment::PidNamespace && !namespace_init() {
        return plain(refused(spec, NOT_NAMESPACE_INIT), Value::Null);
    }
    let scratch = request.scratch.as_deref().map(std::path::Path::new);
    if let Some(scratch) = scratch
        && let Err(reason) = empty_directory(scratch)
    {
        return plain(
            refused(spec, &format!("the sample scratch is unusable: {reason}")),
            json!({"status": "refused", "detail": bounded(&reason)}),
        );
    }
    let mut setup_outcome = None;
    let mut setup_failed = None;
    if let (Some(setup_spec), Some(setup)) = (&parsed.setup, &request.setup) {
        let outcome = match std::env::set_current_dir(&setup.cwd) {
            Ok(()) => run_sample(setup_spec, control),
            Err(error) => refused(setup_spec, &format!("cannot enter {}: {error}", setup.cwd)),
        };
        setup_failed = setup_refusal(&outcome);
        setup_outcome = Some(outcome);
    }
    // The child starts in this process's directory: one supervisor runs one sample.
    let outcome = if let Some(reason) = &setup_failed {
        refused(spec, &format!("the fixture setup failed: {reason}"))
    } else {
        match std::env::set_current_dir(&request.cwd) {
            Ok(()) => run_sample(spec, control),
            Err(error) => refused(spec, &format!("cannot enter {}: {error}", request.cwd)),
        }
    };
    let Some(scratch) = scratch else {
        return Supervised {
            outcome,
            scratch: Value::Null,
            setup: setup_outcome,
            setup_failed,
        };
    };
    // The namespace the scratch is cleared in must be verified empty by the last child that
    // ran in it: the measured child, or a setup that failed before it.
    let last = match (&setup_failed, &setup_outcome) {
        (Some(_), Some(setup)) => &setup.lifecycle,
        _ => &outcome.lifecycle,
    };
    let verified = matches!(
        last.status,
        LifecycleStatus::Clean | LifecycleStatus::UnexpectedDescendants
    ) && last.remaining_after_cleanup == Some(0);
    let report = if !verified {
        json!({"status": "retained", "detail": "the namespace was not verified empty, so the \
               scratch was not cleared"})
    } else {
        match clear(scratch) {
            Ok(()) => json!({"status": "cleared", "detail": "cleared from inside the verified-\
                             empty PID namespace"}),
            Err(reason) => json!({"status": "retained", "detail": bounded(&reason)}),
        }
    };
    Supervised {
        outcome,
        scratch: report,
        setup: setup_outcome,
        setup_failed,
    }
}

/// Why a setup does not let the measured child launch, or `None` when it exited 0, its exit
/// was observed unreaped, and its lifecycle is the best this containment can establish with
/// nothing left behind.
fn setup_refusal(outcome: &SampleOutcome) -> Option<String> {
    let accepted = match outcome.lifecycle.status {
        LifecycleStatus::Clean => true,
        LifecycleStatus::NoObservedDescendants => {
            outcome.lifecycle.observation == review_process::sample::Observation::PipesOnly
        }
        _ => false,
    };
    match &outcome.termination {
        Termination::Exited { code: 0 }
            if accepted && outcome.exit_observation == ExitObservation::Unreaped =>
        {
            None
        }
        Termination::Exited { code: 0 } => Some(format!(
            "it exited 0 with lifecycle {} ({})",
            outcome.lifecycle.status.name(),
            bounded(&outcome.lifecycle.detail)
        )),
        Termination::Exited { code } => Some(format!("it exited {code}")),
        Termination::Signalled { signal } => Some(format!("it was killed by signal {signal}")),
        Termination::TimedOut => Some("it exceeded its deadline".into()),
        Termination::Cancelled { signal } => Some(format!("it was cancelled by signal {signal}")),
        Termination::LaunchFailed { detail } => Some(format!("it did not launch: {detail}")),
        Termination::StatusUnknown { detail } => Some(format!("its status is unknown: {detail}")),
    }
}

fn refused(spec: &SampleSpec, reason: &str) -> SampleOutcome {
    SampleOutcome::refused(spec.containment, reason.to_string())
}

fn empty_directory(path: &std::path::Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let mut entries = std::fs::read_dir(path).map_err(|error| error.to_string())?;
    if entries.next().is_some() {
        return Err(format!("{} is not empty before the launch", path.display()));
    }
    Ok(())
}

/// Remove every entry below the scratch mount; the mount point itself stays. Nothing else
/// runs in the namespace, and `remove_dir_all` never follows a symlink.
fn clear(path: &std::path::Path) -> Result<(), String> {
    let entries = std::fs::read_dir(path).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        let removed = if kind.is_dir() {
            std::fs::remove_dir_all(entry.path())
        } else {
            std::fs::remove_file(entry.path())
        };
        removed.map_err(|error| format!("{}: {error}", entry.path().display()))?;
    }
    empty_directory(path)
}

/// Linux cgroup and affinity observations, or why there are none.
fn observations() -> Result<Value, String> {
    linux_observations().map(|list| {
        Value::Object(
            list.into_iter()
                .map(|(name, value)| {
                    let entry = match value {
                        Ok(text) => json!({"value": text}),
                        Err(reason) => json!({"unavailable": bounded(&reason)}),
                    };
                    (name, entry)
                })
                .collect(),
        )
    })
}

fn bounded(text: &str) -> String {
    if text.len() <= DETAIL_MAX {
        return text.to_string();
    }
    let mut end = DETAIL_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn termination(termination: &Termination) -> Value {
    let (kind, code, signal, detail) = match termination {
        Termination::Exited { code } => ("exited", Some(*code), None, String::new()),
        Termination::Signalled { signal } => ("signalled", None, Some(*signal), String::new()),
        Termination::TimedOut => ("timeout", None, None, String::new()),
        Termination::Cancelled { signal } => ("cancelled", None, Some(*signal), String::new()),
        Termination::LaunchFailed { detail } => ("launch_failed", None, None, bounded(detail)),
        Termination::StatusUnknown { detail } => ("status_unknown", None, None, bounded(detail)),
    };
    json!({"kind": kind, "code": code, "signal": signal, "detail": detail})
}

fn lifecycle(lifecycle: &review_process::sample::Lifecycle) -> Value {
    json!({
        "observation": lifecycle.observation.name(),
        "descendants_at_exit": lifecycle.descendants_at_exit,
        "remaining_after_cleanup": lifecycle.remaining_after_cleanup,
        "pipes_held": lifecycle.pipes_held,
        "status": lifecycle.status.name(),
        "detail": bounded(&lifecycle.detail),
    })
}

fn stream(stream: &review_process::sample::Stream) -> Value {
    json!({"retained": stream.retained.len(), "total": stream.total,
           "truncated": stream.truncated()})
}

/// What the declared selectors observed of the whole stdout as one JSON document, or null
/// when none was declared. A child that never launched wrote nothing, which is no document.
fn json_observation(spec: &SampleSpec, stream: &review_process::sample::Stream) -> Value {
    if spec.stdout_json.is_empty() {
        return Value::Null;
    }
    let observed = stream.json.clone().unwrap_or_else(|| {
        let mut observer = Observer::new(spec.stdout_json.clone());
        observer.finish();
        observer.observation()
    });
    let document = observed.document;
    json!({
        "document": {"status": document.name(), "offset": document.offset(),
                     "detail": document.detail()},
        "selectors": observed.selectors.iter().map(selected).collect::<Vec<_>>(),
    })
}

fn selected(observed: &Selected) -> Value {
    json!({
        "holders": observed.holders,
        "count": observed.count,
        "duplicates": observed.duplicates,
        "invalid": observed.invalid,
        "first_invalid": observed.first_invalid,
        "first": observed.first,
        "last": observed.last,
        "breaks": observed.breaks,
        "first_break": observed.first_break.map(|at| json!({
            "index": at.index, "after": at.after, "value": at.value,
        })),
        "matches": observed.matches,
    })
}

/// The setup's own record: how it ended, its untimed duration, its output counts and a bounded
/// tail of its stderr for diagnosis; its output is never part of the reply's payload.
fn setup_record(outcome: &SampleOutcome) -> Value {
    let tail_start = outcome.stderr.retained.len().saturating_sub(DETAIL_MAX);
    json!({
        "termination": termination(&outcome.termination),
        "wall_ns": outcome.child_wall_ns.map(|nanos| nanos.to_string()),
        "exit_observation": outcome.exit_observation.name(),
        "stdout": stream(&outcome.stdout),
        "stderr": stream(&outcome.stderr),
        "stderr_tail": bounded(&String::from_utf8_lossy(&outcome.stderr.retained[tail_start..])),
        "lifecycle": lifecycle(&outcome.lifecycle),
    })
}

fn result(
    spec: &SampleSpec,
    supervised: &Supervised,
    entered: Instant,
    before: Result<Value, String>,
    after: Result<Value, String>,
) -> Vec<u8> {
    let outcome = &supervised.outcome;
    // A failed setup launched no measured child: the result says so, and its lifecycle is the
    // setup's, the last thing that ran in the containment.
    let (termination, lifecycle) = match (&supervised.setup_failed, &supervised.setup) {
        (Some(reason), Some(setup)) => (
            json!({"kind": "setup_failed", "code": null, "signal": null,
                   "detail": bounded(reason)}),
            lifecycle(&setup.lifecycle),
        ),
        _ => (
            termination(&outcome.termination),
            lifecycle(&outcome.lifecycle),
        ),
    };
    let rusage = outcome.rusage.as_ref().ok().map(|usage| {
        json!({
            "scope": "reaped_child_and_its_waited_descendants",
            "user_cpu_ns": usage.user_cpu_ns.to_string(),
            "system_cpu_ns": usage.system_cpu_ns.to_string(),
            "maxrss_bytes": usage.maxrss_bytes,
            "minor_faults": usage.minor_faults,
            "major_faults": usage.major_faults,
            "voluntary_switches": usage.voluntary_switches,
            "involuntary_switches": usage.involuntary_switches,
        })
    });
    let doc = json!({
        "schema": RESULT_SCHEMA,
        "supervisor": {
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "namespace_init": namespace_init(),
            "containment": spec.containment.name(),
        },
        "termination": termination,
        "timing": {
            "child_wall_ns": outcome.child_wall_ns.map(|nanos| nanos.to_string()),
            "exit_observation": outcome.exit_observation.name(),
            "before_launch_ns": outcome.launched.saturating_duration_since(entered)
                .as_nanos().to_string(),
            "cleanup_ns": outcome.cleanup_ns.to_string(),
            "after_exit_ns": outcome.ended.elapsed().as_nanos().to_string(),
        },
        "stdout": stream(&outcome.stdout),
        "stderr": stream(&outcome.stderr),
        // Each requested pattern's occurrences in the whole stdout, past its cap too; a child
        // that never launched wrote nothing.
        "stdout_pattern_counts": if outcome.stdout.pattern_counts.is_empty() {
            vec![0; spec.stdout_patterns.len()]
        } else {
            outcome.stdout.pattern_counts.clone()
        },
        // The declared selectors over the whole stdout as one document (ADR-0172).
        "stdout_json": json_observation(spec, &outcome.stdout),
        "lifecycle": lifecycle,
        "setup": supervised.setup.as_ref().map(setup_record),
        "scratch": supervised.scratch,
        "rusage": rusage,
        "rusage_unavailable": outcome.rusage.as_ref().err().map(|reason| bounded(reason)),
        "observations": match (before, after) {
            (Ok(before), Ok(after)) => json!({"before": before, "after": after,
                                              "unavailable": null}),
            (Err(reason), _) | (_, Err(reason)) => json!({"before": {}, "after": {},
                                                         "unavailable": bounded(&reason)}),
        },
    });
    serde_json::to_vec(&doc).unwrap_or_default()
}
