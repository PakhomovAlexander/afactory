//! `af telemetry context`, `reasoning` and `search` (design §9, §10): the content level read
//! back for the operator — the composed Worker input of one Attempt, its thinking blocks and
//! reasoning summaries in order, and full-text search over the retained text blobs — as human
//! text on a terminal and one versioned JSON document with `--json`
//! (`af/telemetry-context@1`, `af/telemetry-reasoning@1`, `af/telemetry-search@1`).
//!
//! The content is the operator's: nothing here is read by any code that composes a Worker's
//! input (ADR-0153 clause 2). Below `content` `context` and `reasoning` say the content was not
//! captured, and `search` is refused.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use review_telemetry::content_view::{self, Context, Part, Reasoning, Search};
use review_telemetry::level::Level;
use review_telemetry::view;

use super::views::{iso_ns, print_json, resolve, root_of};
use crate::storage::human_bytes;

/// What `context` and `reasoning` read: the directory's index, the run, and how it was read.
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

/// `af telemetry context`.
pub(crate) fn context(
    json: bool,
    raw: bool,
    dir: Option<PathBuf>,
    run: String,
    attempt: String,
    store: Option<String>,
) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry context";
    let opened = match open(COMMAND, json, dir, &run, store.as_deref())? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    let (store_id, trace_id, kind) = opened.found;
    let document = content_view::context(
        &opened.index,
        &opened.root,
        opened.read,
        opened.unreadable,
        (&store_id, &run, &trace_id, kind),
        &attempt,
    )?;
    if json {
        print_json(&document)?;
    } else if raw {
        // Exactly the retained bytes the Worker's input became, nothing around them.
        match document
            .input
            .as_ref()
            .and_then(|input| input.content.as_deref())
        {
            Some(content) => std::io::stdout()
                .write_all(content.as_bytes())
                .map_err(|error| error.to_string())?,
            None => eprintln!(
                "{COMMAND}: {}",
                document.reason.as_deref().unwrap_or("content unavailable")
            ),
        }
    } else {
        print!("{}", render_context(&document));
    }
    Ok(0)
}

/// `af telemetry reasoning`.
pub(crate) fn reasoning(
    json: bool,
    dir: Option<PathBuf>,
    run: String,
    attempt: String,
    store: Option<String>,
) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry reasoning";
    let opened = match open(COMMAND, json, dir, &run, store.as_deref())? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    let (store_id, trace_id, kind) = opened.found;
    let document = content_view::reasoning(
        &opened.index,
        &opened.root,
        opened.read,
        opened.unreadable,
        (&store_id, &run, &trace_id, kind),
        &attempt,
    )?;
    if json {
        print_json(&document)?;
    } else {
        print!("{}", render_reasoning(&document));
    }
    Ok(0)
}

/// `af telemetry search`: refused below `content` and without `content_index`, exit 2.
pub(crate) fn search(
    json: bool,
    dir: Option<PathBuf>,
    text: String,
    since: Option<String>,
    run: Option<String>,
    store: Option<String>,
) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry search";
    let settings = match super::settings() {
        Ok(settings) => settings,
        Err(refusal) => return Ok(super::refused(COMMAND, json, &refusal)),
    };
    if settings.level != Level::Content {
        return Ok(super::refused(
            COMMAND,
            json,
            &format!(
                "content search exists only at [telemetry] level = \"content\"; the level here \
                 is {}, which keeps no searchable text",
                settings.level.as_str()
            ),
        ));
    }
    if !settings.content_index {
        return Ok(super::refused(
            COMMAND,
            json,
            "[telemetry] content_index = false: no full-text index is kept, so nothing can be \
             searched",
        ));
    }
    let since_unix_ns = match &since {
        Some(text) => {
            let window = view::parse_since(text)
                .ok_or_else(|| format!("--since {text}: a duration such as 30m, 12h, 14d or 2w"))?;
            let now = review_telemetry::layout::unix_ns_now();
            Some(i64::try_from(now.saturating_sub(window.as_nanos())).unwrap_or(i64::MAX))
        }
        None => None,
    };
    let root = match root_of(COMMAND, json, dir)? {
        Ok(root) => root,
        Err(code) => return Ok(code),
    };
    let document = if root.is_dir() {
        let (index, read, unreadable) = view::open(&root)?;
        if let Some(note) = &read.reason {
            eprintln!("{COMMAND}: {note}");
        }
        let trace = match &run {
            Some(run) => match resolve(COMMAND, &index, run, store.as_deref())? {
                Ok((_, trace_id, _)) => Some(trace_id),
                Err(code) => return Ok(code),
            },
            None => None,
        };
        content_view::search(
            &index,
            &root,
            read,
            unreadable,
            &text,
            since_unix_ns,
            run.as_deref().zip(trace.as_deref()),
        )?
    } else {
        if let Some(run) = &run {
            return Err(format!(
                "unknown run {run}: nothing has been recorded in {}",
                root.display()
            ));
        }
        Search {
            schema: content_view::SEARCH_SCHEMA.into(),
            directory: root.display().to_string(),
            index: view::IndexRead {
                state: "absent".into(),
                reason: Some("nothing has been recorded here".into()),
            },
            query: text,
            since_unix_ns: since_unix_ns.map(|ns| ns.to_string()),
            run_id: None,
            matches: Vec::new(),
            more: false,
            unreadable: Vec::new(),
        }
    };
    if json {
        print_json(&document)?;
    } else {
        print!("{}", render_search(&document));
    }
    Ok(0)
}

fn heading(out: &mut String, what: &str, run: &review_telemetry::turns::HarnessRun) {
    let _ = writeln!(
        out,
        "{what}  {} run {}  history {} — {}",
        run.kind.as_deref().unwrap_or("unknown"),
        run.run_id,
        run.history,
        run.retained
    );
}

fn state_line(state: &str, reason: Option<&str>) -> String {
    match (state, reason) {
        ("captured", _) => "captured".into(),
        ("not_captured", Some(reason)) => format!("not captured: {reason}"),
        (state, Some(reason)) => format!("{state}: {reason}"),
        (state, None) => state.to_string(),
    }
}

/// One occurrence's identity and sizes on one line.
fn part_line(label: &str, part: &Part) -> String {
    let mut line = format!(
        "  {label:<8} {}  {}",
        part.occurrence_id.as_deref().unwrap_or("-"),
        part.digest.as_deref().unwrap_or("-")
    );
    if let Some(bytes) = part.retained_bytes {
        let _ = write!(line, "  {}", human_bytes(bytes));
    }
    if let Some(original) = part.original_bytes {
        let _ = write!(line, " (original {})", human_bytes(original));
    }
    if part.truncated {
        line.push_str("  truncated");
    }
    if let Some(reason) = &part.reason {
        let _ = write!(line, "  {reason}");
    }
    line
}

pub(crate) fn render_context(document: &Context) -> String {
    let mut out = String::new();
    heading(
        &mut out,
        &format!("context of Attempt {}", document.attempt),
        &document.run,
    );
    let _ = writeln!(
        out,
        "  {}",
        state_line(&document.state, document.reason.as_deref())
    );
    if let Some(span) = &document.compose_span_id {
        let mut line = format!(
            "  composed span {span}  level {}",
            document.level.as_deref().unwrap_or("-")
        );
        if let Some(manifest) = &document.manifest {
            let _ = write!(line, "  manifest {manifest}");
        }
        if let Some(bytes) = document.rendered_bytes {
            let _ = write!(line, "  {}", human_bytes(bytes));
        }
        if let Some(tokens) = &document.estimated_tokens {
            let _ = write!(line, "  ~{tokens} tokens");
        }
        let _ = writeln!(out, "{line}");
        if !document.layers.is_empty() {
            let _ = writeln!(out, "  layers {}", document.layers.join(", "));
        }
    }
    for (label, part) in [("input", &document.input), ("system", &document.system)] {
        if let Some(part) = part {
            let _ = writeln!(out, "{}", part_line(label, part));
        }
    }
    if let Some(content) = document
        .input
        .as_ref()
        .and_then(|input| input.content.as_deref())
    {
        let _ = writeln!(out, "--- input ---");
        out.push_str(content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

pub(crate) fn render_reasoning(document: &Reasoning) -> String {
    let mut out = String::new();
    heading(
        &mut out,
        &format!("reasoning of Attempt {}", document.attempt),
        &document.run,
    );
    let _ = writeln!(
        out,
        "  {}",
        state_line(&document.state, document.reason.as_deref())
    );
    for harness in &document.harnesses {
        let _ = writeln!(
            out,
            "  harness {}  level {} — {}",
            harness.span_id,
            harness.level,
            state_line(&harness.state, harness.reason.as_deref())
        );
    }
    let _ = writeln!(out, "{} thoughts", document.thoughts.len());
    for thought in &document.thoughts {
        let turn = thought
            .turn_ordinal
            .map_or_else(|| "-".to_string(), |ordinal| ordinal.to_string());
        let _ = writeln!(
            out,
            "#{} {}  turn {turn}  {}",
            thought.ordinal,
            thought.kind,
            part_line("", &thought.part).trim_start()
        );
        if let Some(content) = &thought.part.content {
            for line in content.lines() {
                let _ = writeln!(out, "    {line}");
            }
        }
    }
    out
}

pub(crate) fn render_search(document: &Search) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} blob(s) hold \"{}\"{}",
        document.matches.len(),
        document.query,
        if document.more {
            " (more not listed)"
        } else {
            ""
        }
    );
    for found in &document.matches {
        let _ = writeln!(
            out,
            "{}  {}",
            found.digest,
            found.excerpt.replace('\n', " ")
        );
        for occurrence in &found.occurrences {
            let _ = writeln!(
                out,
                "    {}  {}  {}  run {}  attempt {}  {}",
                occurrence.occurrence_id,
                occurrence.event,
                iso_ns(&occurrence.time_unix_ns),
                occurrence.run_id.as_deref().unwrap_or("-"),
                occurrence.attempt_id.as_deref().unwrap_or("-"),
                occurrence.span_path.join(" > ")
            );
        }
        if found.more_occurrences {
            let _ = writeln!(out, "    … more occurrences");
        }
    }
    out
}
