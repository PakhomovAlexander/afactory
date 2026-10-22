//! `af telemetry turns` and `tools` (design §10): one row per model turn or tool call the
//! harness streams showed (design §7.3), as human text on a terminal and one versioned JSON
//! document with `--json` (`af/telemetry-turns@1`, `af/telemetry-tools@1`).

use std::fmt::Write as _;
use std::path::PathBuf;

use review_telemetry::turns::{self, Harness, Tools, Turns};
use review_telemetry::view;

use super::views::{duration, iso_ns, print_json, resolve, root_of};

/// What both commands read: the directory's index, the run, and how it was read.
struct Opened {
    root: PathBuf,
    index: review_telemetry::index::Index,
    read: view::IndexRead,
    unreadable: Vec<view::Unreadable>,
    found: super::views::Found,
}

fn open(
    command: &str,
    json: bool,
    dir: Option<PathBuf>,
    run: &str,
    store: Option<&str>,
) -> Result<Result<Opened, i32>, String> {
    let root = match root_of(command, json, dir)? {
        Ok(root) => root,
        Err(code) => return Ok(Err(code)),
    };
    if !root.is_dir() {
        return Err(format!(
            "unknown run {run}: nothing has been recorded in {}",
            root.display()
        ));
    }
    let (index, read, unreadable) = view::open(&root)?;
    if let Some(note) = &read.reason {
        eprintln!("{command}: {note}");
    }
    let found = match resolve(command, &index, run, store)? {
        Ok(found) => found,
        Err(code) => return Ok(Err(code)),
    };
    Ok(Ok(Opened {
        root,
        index,
        read,
        unreadable,
        found,
    }))
}

/// `af telemetry turns`.
pub(crate) fn turns(
    json: bool,
    dir: Option<PathBuf>,
    run: String,
    store: Option<String>,
    attempt: Option<String>,
) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry turns";
    let opened = match open(COMMAND, json, dir, &run, store.as_deref())? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    let (store_id, trace_id, kind) = opened.found;
    let document = turns::turns(
        &opened.index,
        &opened.root,
        opened.read,
        opened.unreadable,
        (&store_id, &run, &trace_id, kind),
        attempt.as_deref(),
    )?;
    if json {
        print_json(&document)?;
    } else {
        print!("{}", render_turns(&document));
    }
    Ok(0)
}

/// `af telemetry tools`.
pub(crate) fn tools(
    json: bool,
    dir: Option<PathBuf>,
    run: String,
    store: Option<String>,
    attempt: Option<String>,
) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry tools";
    let opened = match open(COMMAND, json, dir, &run, store.as_deref())? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    let (store_id, trace_id, kind) = opened.found;
    let document = turns::tools(
        &opened.index,
        &opened.root,
        opened.read,
        opened.unreadable,
        (&store_id, &run, &trace_id, kind),
        attempt.as_deref(),
    )?;
    if json {
        print_json(&document)?;
    } else {
        print!("{}", render_tools(&document));
    }
    Ok(0)
}

fn heading(out: &mut String, run: &turns::HarnessRun, rows: usize, what: &str) {
    let _ = writeln!(
        out,
        "{} run {}  history {} — {}",
        run.kind.as_deref().unwrap_or("unknown"),
        run.run_id,
        run.history,
        run.retained
    );
    let _ = writeln!(out, "{rows} {what}");
}

/// One line per harness process the rows came from, the first time it appears.
fn harness_line(out: &mut String, seen: &mut Vec<String>, harness: &Harness) {
    if seen.contains(&harness.span_id) {
        return;
    }
    seen.push(harness.span_id.clone());
    let mut line = format!(
        "harness {} attempt {} node {}  stream {}",
        harness.span_id,
        harness.attempt_id.as_deref().unwrap_or("-"),
        harness.node_id.as_deref().unwrap_or("-"),
        harness.analysis
    );
    if let Some(dropped) = harness.dropped_chunks.filter(|dropped| *dropped > 0) {
        let _ = write!(line, "  dropped_chunks={dropped}");
    }
    if let Some(gaps) = harness.gaps.filter(|gaps| *gaps > 0) {
        let _ = write!(line, "  gaps={gaps}");
    }
    if harness.withheld == Some(true) {
        line.push_str("  content withheld");
    }
    let _ = writeln!(out, "{line}");
}

fn timing(offset: u64, total: Option<u64>) -> (String, String) {
    (
        duration(offset),
        total.map_or_else(|| "open".to_string(), duration),
    )
}

pub(crate) fn render_turns(document: &Turns) -> String {
    let mut out = String::new();
    heading(&mut out, &document.run, document.turns.len(), "turns");
    let mut seen = Vec::new();
    for turn in &document.turns {
        harness_line(&mut out, &mut seen, &turn.harness);
        let (offset, total) = timing(turn.start_offset_us, turn.duration_us);
        let usage = &turn.usage;
        let mut counters = Vec::new();
        for (name, value) in [
            ("in", &usage.input_tokens),
            ("out", &usage.output_tokens),
            ("cache read", &usage.cache_read_tokens),
            ("cache write", &usage.cache_write_tokens),
            ("reasoning", &usage.reasoning_tokens),
        ] {
            if let Some(value) = value {
                counters.push(format!("{name} {value}"));
            }
        }
        let _ = writeln!(
            out,
            "  turn {:<3} {} +{offset:<12} {total:<12} {:<6} {:<10} stop {:<10} {}  text {}/{} B  thinking {}/{} B  tools {}",
            turn.ordinal,
            iso_ns(&turn.start_unix_ns),
            turn.status,
            turn.outcome.as_deref().unwrap_or("-"),
            if turn.finish_reasons.is_empty() {
                "-".to_string()
            } else {
                turn.finish_reasons.join(",")
            },
            if counters.is_empty() {
                format!("usage {}", usage.observation.as_deref().unwrap_or("-"))
            } else {
                counters.join(", ")
            },
            turn.text_blocks,
            turn.text_bytes,
            turn.thinking_blocks,
            turn.thinking_bytes,
            turn.tool_calls,
        );
    }
    out
}

pub(crate) fn render_tools(document: &Tools) -> String {
    let mut out = String::new();
    heading(&mut out, &document.run, document.tools.len(), "tool calls");
    let mut seen = Vec::new();
    for tool in &document.tools {
        harness_line(&mut out, &mut seen, &tool.harness);
        let (offset, total) = timing(tool.start_offset_us, tool.duration_us);
        let mut line = format!(
            "  {:<18} {} +{offset:<12} {total:<12} {:<6} {:<10} in {} B  out {}",
            tool.name.as_deref().unwrap_or("(unlisted)"),
            iso_ns(&tool.start_unix_ns),
            tool.status,
            tool.outcome.as_deref().unwrap_or("-"),
            tool.input_bytes,
            tool.output_bytes
                .map_or_else(|| "-".to_string(), |bytes| format!("{bytes} B")),
        );
        if let Some(code) = tool.exit_code {
            let _ = write!(line, "  exit {code}");
        }
        if let Some(status) = &tool.item_status {
            let _ = write!(line, "  status {status}");
        }
        let _ = writeln!(out, "{line}");
    }
    out
}
