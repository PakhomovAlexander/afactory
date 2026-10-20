//! `af telemetry runs`, `show`, `tail` and `gc` (design §10): each reads, prints human text on
//! a terminal and one versioned JSON document with `--json`; progress and warnings go to
//! stderr. `gc --apply` is the one that removes anything, and only under ADR-0144's rules.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use review_telemetry::view::{self, Resolved, Run, Runs, Show, ShowSelection, TailSelection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::{self, human_bytes};

/// `af/telemetry-gc@1`.
pub(crate) const GC_SCHEMA: &str = "af/telemetry-gc@1";

/// A `[telemetry]` refusal exits 2; otherwise the directory to read.
fn root_of(
    command: &str,
    json: bool,
    dir: Option<PathBuf>,
) -> Result<Result<PathBuf, i32>, String> {
    let settings = match super::settings() {
        Ok(settings) => settings,
        Err(refusal) => return Ok(Err(super::refused(command, json, &refusal))),
    };
    Ok(Ok(super::chosen(dir, &settings)?))
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

/// Unix nanoseconds (decimal text) as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub(crate) fn iso_ns(text: &str) -> String {
    let nanos: u128 = text.parse().unwrap_or(0);
    let millis = u64::try_from(nanos / 1_000_000).unwrap_or(u64::MAX);
    let seconds = review_core::task::collection::collected_time(millis);
    format!("{}.{:03}Z", seconds.trim_end_matches('Z'), millis % 1_000)
}

/// Microseconds for a human: `850 µs`, `12.345 ms`, `3.210 s`, `2 h 03 min`.
pub(crate) fn duration(us: u64) -> String {
    match us {
        0..1_000 => format!("{us} µs"),
        1_000..1_000_000 => format!("{}.{:03} ms", us / 1_000, us % 1_000),
        1_000_000..3_600_000_000 => format!("{}.{:03} s", us / 1_000_000, us % 1_000_000 / 1_000),
        _ => format!(
            "{} h {:02} min",
            us / 3_600_000_000,
            us % 3_600_000_000 / 60_000_000
        ),
    }
}

fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

/// `af telemetry runs`.
pub(crate) fn runs(
    json: bool,
    dir: Option<PathBuf>,
    since: Option<String>,
    kind: Option<String>,
) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry runs";
    let root = match root_of(COMMAND, json, dir)? {
        Ok(root) => root,
        Err(code) => return Ok(code),
    };
    let since_unix_ns = match &since {
        Some(text) => {
            let window = view::parse_since(text)
                .ok_or_else(|| format!("--since {text}: a duration such as 30m, 12h, 14d or 2w"))?;
            let now = review_telemetry::layout::unix_ns_now();
            Some(i64::try_from(now.saturating_sub(window.as_nanos())).unwrap_or(i64::MAX))
        }
        None => None,
    };
    let document = if root.is_dir() {
        let (index, read, unreadable) = view::open(&root)?;
        if let Some(note) = &read.reason {
            eprintln!("{COMMAND}: {note}");
        }
        view::runs(
            &index,
            &root,
            read,
            unreadable,
            since_unix_ns,
            kind.as_deref(),
        )?
    } else {
        Runs {
            schema: view::RUNS_SCHEMA.into(),
            directory: root.display().to_string(),
            index: view::IndexRead {
                state: "absent".into(),
                reason: Some("nothing has been recorded here".into()),
            },
            since_unix_ns: since_unix_ns.map(|ns| ns.to_string()),
            kind,
            runs: Vec::new(),
            unreadable: Vec::new(),
        }
    };
    if json {
        print_json(&document)?;
    } else {
        print!("{}", render_runs(&document));
    }
    Ok(0)
}

pub(crate) fn render_runs(document: &Runs) -> String {
    let mut out = String::new();
    if document.runs.is_empty() {
        let _ = writeln!(
            out,
            "no runs in {}",
            super::home_relative(&document.directory)
        );
    }
    for run in &document.runs {
        render_run(&mut out, run);
    }
    for unreadable in &document.unreadable {
        let _ = writeln!(
            out,
            "unreadable {}: {}",
            super::home_relative(&unreadable.path),
            unreadable.reason
        );
    }
    out
}

fn render_run(out: &mut String, run: &Run) {
    let subject = match (&run.task_id, &run.campaign) {
        (_, Some(campaign)) => format!("Campaign {campaign}"),
        (Some(task), None) => format!("Task {task}"),
        (None, None) => "run".into(),
    };
    let _ = writeln!(
        out,
        "{} {subject}  run {}  store {}",
        run.kind.as_deref().unwrap_or("-"),
        run.run_id,
        short(&run.store_id)
    );
    let _ = writeln!(
        out,
        "  executions {} ({}; history {})  first start {}",
        run.executions,
        run.retained,
        run.history,
        iso_ns(&run.first_start_unix_ns)
    );
    let _ = writeln!(
        out,
        "  active {} · execution {} · elapsed {}{} · outcome {}",
        duration(run.active_us),
        duration(run.execution_us),
        duration(run.elapsed_us),
        if run.open { " (open)" } else { "" },
        run.outcome.as_deref().unwrap_or("-")
    );
    let mut tokens = Vec::new();
    for (name, value) in [
        ("input", &run.tokens.input_tokens),
        ("output", &run.tokens.output_tokens),
        ("cache read", &run.tokens.cache_read_tokens),
        ("cache write", &run.tokens.cache_write_tokens),
        ("reasoning", &run.tokens.reasoning_tokens),
    ] {
        if let Some(value) = value {
            tokens.push(format!("{name} {value}"));
        }
    }
    let _ = writeln!(
        out,
        "  charge {} over {} Attempt{}{} · failures {} · coverage {}",
        run.charge.as_deref().unwrap_or("-"),
        run.attempts,
        if run.attempts == 1 { "" } else { "s" },
        if tokens.is_empty() {
            String::new()
        } else {
            format!(" ({})", tokens.join(", "))
        },
        run.failures,
        run.coverage
    );
}

/// A resolved run: its Store, its trace and its kind.
type Found = (String, String, Option<String>);

/// The run `RUN` names in `index`, or the exit code with its message on stderr: 1 for an
/// unknown run, 2 when two Stores share it and `--store` does not say which.
fn resolve(
    command: &str,
    index: &review_telemetry::index::Index,
    run: &str,
    store: Option<&str>,
) -> Result<Result<Found, i32>, String> {
    match view::resolve(index, run, store)? {
        Resolved::Run {
            store_id,
            trace_id,
            kind,
        } => Ok(Ok((store_id, trace_id, kind))),
        Resolved::Unknown => Err(format!("unknown run {run}")),
        Resolved::Ambiguous(stores) => {
            eprintln!(
                "{command}: run {run} is recorded by {} Stores; name one with --store ID:",
                stores.len()
            );
            for store in stores {
                eprintln!("  --store {store}");
            }
            Ok(Err(2))
        }
    }
}

/// `af telemetry show`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    json: bool,
    dir: Option<PathBuf>,
    run: String,
    store: Option<String>,
    attempt: Option<String>,
    span: Option<String>,
    depth: Option<u64>,
) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry show";
    let root = match root_of(COMMAND, json, dir)? {
        Ok(root) => root,
        Err(code) => return Ok(code),
    };
    if !root.is_dir() {
        return Err(format!(
            "unknown run {run}: nothing has been recorded in {}",
            root.display()
        ));
    }
    let (index, read, unreadable) = view::open(&root)?;
    if let Some(note) = &read.reason {
        eprintln!("{COMMAND}: {note}");
    }
    let (store_id, trace_id, kind) = match resolve(COMMAND, &index, &run, store.as_deref())? {
        Ok(found) => found,
        Err(code) => return Ok(code),
    };
    let document = view::show(
        &index,
        &root,
        read,
        unreadable,
        (&store_id, &run, &trace_id, kind),
        &ShowSelection {
            attempt,
            span,
            depth,
        },
    )?;
    if json {
        print_json(&document)?;
    } else {
        print!("{}", render_show(&document));
    }
    Ok(0)
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(crate) fn render_show(document: &Show) -> String {
    let mut out = String::new();
    let run = &document.run;
    let _ = writeln!(
        out,
        "{} run {}  store {}  history {} — {}",
        run.kind.as_deref().unwrap_or("-"),
        run.run_id,
        short(&run.store_id),
        run.history,
        run.retained
    );
    for execution in &run.executions {
        let _ = writeln!(
            out,
            "  execution {} ({}) {} → {}{} · wall {} · {}",
            execution
                .ordinal
                .map_or("?".to_string(), |ordinal| ordinal.to_string()),
            execution.execution_id,
            iso_ns(&execution.start_unix_ns),
            iso_ns(&execution.end_unix_ns),
            if execution.open { " (open)" } else { "" },
            duration(execution.wall_us),
            execution.segment
        );
    }
    let _ = writeln!(
        out,
        "{:<12} {:<12} {:<12} {:<6} span",
        "offset", "total", "self", "status"
    );
    for span in &document.spans {
        let attributes: Vec<String> = span
            .attributes
            .iter()
            .map(|(key, value)| format!("{key}={}", value_text(value)))
            .collect();
        let _ = writeln!(
            out,
            "{:<12} {:<12} {:<12} {:<6} {}{}{}",
            duration(span.start_offset_us),
            span.total_us.map_or("-".into(), duration),
            span.self_us.map_or("-".into(), duration),
            span.status,
            "  ".repeat(usize::try_from(span.depth).unwrap_or(0)),
            span.name,
            if attributes.is_empty() || document.events.is_some() {
                String::new()
            } else {
                format!("  {}", attributes.join(" "))
            }
        );
    }
    if let Some(events) = &document.events {
        if let Some(span) = document.spans.first() {
            let _ = writeln!(out, "span {} ({})", span.span_id, span.name);
            for (key, value) in &span.attributes {
                let _ = writeln!(out, "  {key} = {}", value_text(value));
            }
        }
        let _ = writeln!(out, "events {}", events.len());
        for event in events {
            let attributes: Vec<String> = event
                .attributes
                .iter()
                .map(|(key, value)| format!("{key}={}", value_text(value)))
                .collect();
            let _ = writeln!(
                out,
                "  {:<12} {} {}",
                duration(event.offset_us),
                event.name,
                attributes.join(" ")
            );
        }
    }
    for unreadable in &document.unreadable {
        let _ = writeln!(
            out,
            "unreadable {}: {}",
            super::home_relative(&unreadable.path),
            unreadable.reason
        );
    }
    out
}

/// `af telemetry tail`.
pub(crate) fn tail(
    json: bool,
    dir: Option<PathBuf>,
    run: Option<String>,
    store: Option<String>,
) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry tail";
    let settings = match super::settings() {
        Ok(settings) => settings,
        Err(refusal) => return Ok(super::refused(COMMAND, json, &refusal)),
    };
    let root = super::chosen(dir, &settings)?;
    // A run two Stores share needs `--store`; the index knows the runs ingested so far.
    if let Some(run) = &run
        && store.is_none()
        && root.is_dir()
    {
        let (index, _, _) = view::open(&root)?;
        let stores = view::stores_of(&index, run)?;
        if stores.len() > 1 {
            eprintln!(
                "{COMMAND}: run {run} is recorded by {} Stores; name one with --store ID:",
                stores.len()
            );
            for store in stores {
                eprintln!("  --store {store}");
            }
            return Ok(2);
        }
    }
    // Looking for new bytes a quarter of the flush latency apart, a record the consumer wrote is
    // printed well within `flush_latency_ms` of its creation.
    let interval = Duration::from_millis((settings.flush_latency_ms / 4).clamp(10, 250));
    let stdout = std::io::stdout();
    let selection = TailSelection {
        run_id: run,
        store_id: store,
    };
    view::tail(&root, &selection, interval, &|| false, &mut |line| {
        let mut handle = stdout.lock();
        if json {
            serde_json::to_writer(&mut handle, line).map_err(std::io::Error::other)?;
            handle.write_all(b"\n")?;
        } else {
            writeln!(handle, "{}", render_tail_line(line))?;
        }
        handle.flush()
    })?;
    Ok(0)
}

pub(crate) fn render_tail_line(line: &view::TailLine) -> String {
    let attributes: Vec<String> = line
        .attributes
        .iter()
        .map(|(key, value)| format!("{key}={}", value_text(value)))
        .collect();
    match line.record.as_str() {
        "span_end" => format!(
            "{} end   {} {} {}{}",
            iso_ns(&line.time_unix_ns),
            line.span,
            line.status.as_deref().unwrap_or("unset"),
            line.duration_us.map_or("-".into(), duration),
            if attributes.is_empty() {
                String::new()
            } else {
                format!("  {}", attributes.join(" "))
            }
        ),
        _ => format!(
            "{} event {} on {}{}",
            iso_ns(&line.time_unix_ns),
            line.name.as_deref().unwrap_or(""),
            line.span,
            if attributes.is_empty() {
                String::new()
            } else {
                format!("  {}", attributes.join(" "))
            }
        ),
    }
}

/// `af/telemetry-gc@1`: what the sweep would evict from (or, applied, evicted from) the
/// telemetry kind, and the telemetry entries exactly as `af storage` lists them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Gc {
    pub(crate) schema: String,
    pub(crate) applied: bool,
    /// `[telemetry] max_bytes`; `null` when the table cannot be read.
    pub(crate) max_bytes: Option<u64>,
    /// `[storage] max_bytes`.
    pub(crate) storage_max_bytes: u64,
    pub(crate) total_before: u64,
    pub(crate) total_after: u64,
    pub(crate) storage_total_before: u64,
    pub(crate) storage_total_after: u64,
    /// `fits` or `nothing_evictable`.
    pub(crate) stop: String,
    /// The telemetry entries as `af storage --json` lists them, before this command.
    pub(crate) entries: Vec<Value>,
    pub(crate) removals: Vec<Value>,
    pub(crate) reclaimed: Vec<Value>,
    pub(crate) registry_dropped: Vec<String>,
    pub(crate) notes: Vec<String>,
    pub(crate) failures: Vec<String>,
    pub(crate) problems: Vec<Value>,
}

/// `af telemetry gc`.
pub(crate) fn gc(json: bool, apply: bool) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry gc";
    if let Err(refusal) = super::settings() {
        return Ok(super::refused(COMMAND, json, &refusal));
    }
    let policy = storage::policy()?;
    let roots = storage::Roots::current()?;
    let (report, entries, problems) = storage::sweep_telemetry(&roots, &policy, apply);
    let to_values = |values: &[storage::Entry]| -> Result<Vec<Value>, String> {
        values
            .iter()
            .map(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
            .collect()
    };
    let over_telemetry = report
        .telemetry
        .max_bytes
        .is_some_and(|cap| report.telemetry.total_after > cap);
    let document = Gc {
        schema: GC_SCHEMA.into(),
        applied: apply,
        max_bytes: report.telemetry.max_bytes,
        storage_max_bytes: report.max_bytes,
        total_before: report.telemetry.total_before,
        total_after: report.telemetry.total_after,
        storage_total_before: report.total_before,
        storage_total_after: report.total_after,
        stop: if report.stop == storage::Stop::NothingEvictable
            || report.telemetry.stop == storage::Stop::NothingEvictable
            || over_telemetry
        {
            "nothing_evictable".into()
        } else {
            "fits".into()
        },
        entries: to_values(&entries)?,
        removals: report
            .removals
            .iter()
            .map(|removal| serde_json::to_value(removal).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?,
        reclaimed: report
            .telemetry
            .reclaimed
            .iter()
            .map(|reclaimed| serde_json::to_value(reclaimed).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?,
        registry_dropped: report
            .telemetry
            .registry_dropped
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        notes: report.telemetry.notes.clone(),
        failures: report.failures.clone(),
        problems: problems
            .iter()
            .map(|problem| serde_json::to_value(problem).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?,
    };
    if json {
        print_json(&document)?;
    } else {
        print!("{}", render_gc(&document));
    }
    Ok(0)
}

pub(crate) fn render_gc(document: &Gc) -> String {
    let mut out = String::new();
    let verb = if document.applied {
        "removed"
    } else {
        "would remove"
    };
    for entry in &document.entries {
        let field = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("-");
        let _ = writeln!(
            out,
            "{:<12} {:>10}  {}{}",
            field("state"),
            human_bytes(entry["bytes"].as_u64().unwrap_or(0)),
            entry
                .get("segment_id")
                .and_then(Value::as_str)
                .map_or_else(|| field("path").to_string(), str::to_string),
            entry
                .get("in_use")
                .and_then(Value::as_str)
                .map(|why| format!("  (kept: {why})"))
                .unwrap_or_default()
        );
    }
    for removal in &document.removals {
        let _ = writeln!(
            out,
            "{verb} {} {} ({})",
            removal
                .get("segment_id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| removal["path"].as_str().unwrap_or("-")),
            human_bytes(removal["bytes"].as_u64().unwrap_or(0)),
            removal["rule"].as_str().unwrap_or("-")
        );
    }
    for reclaimed in &document.reclaimed {
        let _ = writeln!(
            out,
            "reclaimed in {}: temporaries {}, blobs {}, index {} ({})",
            super::home_relative(reclaimed["directory"].as_str().unwrap_or("")),
            human_bytes(reclaimed["temporary_bytes"].as_u64().unwrap_or(0)),
            human_bytes(reclaimed["blob_bytes"].as_u64().unwrap_or(0)),
            human_bytes(reclaimed["index_bytes"].as_u64().unwrap_or(0)),
            reclaimed["index"].as_str().unwrap_or("-")
        );
    }
    for dropped in &document.registry_dropped {
        let _ = writeln!(
            out,
            "{} registry entry {} (no telemetry directory there)",
            if document.applied {
                "dropped"
            } else {
                "would drop"
            },
            super::home_relative(dropped)
        );
    }
    for note in &document.notes {
        let _ = writeln!(out, "note: {note}");
    }
    for failure in &document.failures {
        let _ = writeln!(out, "failed: {failure}");
    }
    let _ = writeln!(
        out,
        "telemetry {} → {} of a {} cap ([telemetry] max_bytes); af {} → {} of a {} budget; {}{}",
        human_bytes(document.total_before),
        human_bytes(document.total_after),
        document
            .max_bytes
            .map_or("unknown".to_string(), human_bytes),
        human_bytes(document.storage_total_before),
        human_bytes(document.storage_total_after),
        human_bytes(document.storage_max_bytes),
        if document.stop == "fits" {
            "fits"
        } else {
            "still over: everything left is in use or was used within the hour"
        },
        if document.applied {
            ""
        } else {
            ". Nothing was removed; run with --apply to remove it."
        }
    );
    out
}
