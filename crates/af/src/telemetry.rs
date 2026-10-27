//! `af telemetry` (ADR-0153): `status`, `reindex`, `runs`, `show`, `config`, `tail`, `ledger`,
//! `turns`, `tools`, `context`, `reasoning`, `search` and `gc` — the reader side of Telemetry,
//! which only `af telemetry`, `af storage` and the TUI link — and the recording a command that
//! runs a kernel run does ([`begin`], [`Recording::finish`]).
//!
//! The `[telemetry]` table is read from machine layers only. A refused key exits 2, an
//! unreadable telemetry directory, a busy index or an unknown run exits 1; no `af telemetry`
//! command ever records telemetry itself.

use std::fmt::Write as _;
use std::path::PathBuf;

use review_telemetry::dirs::{self, Accounted};
use review_telemetry::level::Settings;
use review_telemetry::status::{
    self as telemetry_status, BlobVerification, OverlapCheck, Reindex, Status, StatusInput,
};

use crate::config;
use crate::storage::{self, human_bytes};

mod config_record;
mod content;
mod harness;
mod recording;
mod views;

pub(crate) use content::{context, reasoning, search};
pub(crate) use harness::{tools, turns};
pub(crate) use recording::{begin, command_chain};
pub(crate) use views::{config, gc, ledger, runs, show, tail};

/// The machine's `[telemetry]` table, or the refusal naming its key.
pub(crate) fn settings() -> Result<Settings, String> {
    config::load_machine()?.telemetry_policy()
}

/// The telemetry directory `settings` name: `[telemetry] dir`, or the state root's `telemetry/`.
pub(crate) fn directory(settings: &Settings) -> Result<PathBuf, String> {
    match &settings.dir {
        Some(dir) => Ok(dir.clone()),
        None => Ok(config::state_home()?.join("af").join("telemetry")),
    }
}

/// The directory registry, `$XDG_STATE_HOME/af/telemetry-dirs.v1.json`.
pub(crate) fn registry() -> Result<PathBuf, String> {
    Ok(dirs::location(&config::state_home()?))
}

/// Every directory the storage inventory accounts on its own: a telemetry directory may
/// overlap none of them, and may lie inside the state root but never contain it. The second
/// value says why the list is known incomplete: a Store registry that cannot be read (or is of
/// another version) is never silently omitted, so every overlap check becomes `unknown`.
pub(crate) fn accounted() -> Result<(Vec<Accounted>, Option<String>), String> {
    Ok(storage::accounted(&storage::Roots::current()?))
}

/// A refused `[telemetry]` key: exit 2, one line on stderr, and under `--json` the error
/// document on stdout.
pub(super) fn refused(command: &str, json: bool, refusal: &str) -> i32 {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "schema": "af/error@2",
                "command": command,
                "error": refusal,
                "exit_code": 2,
            })
        );
    }
    eprintln!("{command}: {refusal}");
    2
}

fn absolute(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()
        .map_err(|error| format!("current directory: {error}"))?
        .join(path))
}

/// `--dir PATH` must name an existing directory; without it, the configured one is read and
/// may not exist yet.
pub(super) fn chosen(dir: Option<PathBuf>, settings: &Settings) -> Result<PathBuf, String> {
    match dir {
        Some(dir) => {
            let dir = absolute(dir)?;
            if !dir.is_dir() {
                return Err(format!("no telemetry directory at {}", dir.display()));
            }
            Ok(dir)
        }
        None => directory(settings),
    }
}

/// `path` as the text output writes it: under the home directory, `~/…`, by
/// [`telemetry_status::home_relative`]; as it is when no home directory is known.
pub(super) fn home_relative(path: &str) -> String {
    match config::home() {
        Ok(home) => telemetry_status::home_relative(path, &home),
        Err(_) => path.to_string(),
    }
}

/// The warning `status` gives when the directory is over `[telemetry] max_bytes`, which the
/// Storage Budget's next sweep meets by evicting sealed segments (ADR-0144, ADR-0153).
fn over_max_bytes_warning(status: &Status) -> Option<String> {
    status.over_max_bytes().map(|over| {
        format!(
            "warning: the telemetry directory holds {}, {} over [telemetry] max_bytes ({}); the \
             next storage sweep evicts sealed segments to meet it",
            human_bytes(status.bytes.total),
            human_bytes(over),
            human_bytes(status.bytes.max_bytes)
        )
    })
}

/// `af telemetry status`.
pub(crate) fn status(json: bool, dir: Option<PathBuf>) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry status";
    let settings = match settings() {
        Ok(settings) => settings,
        Err(refusal) => return Ok(refused(COMMAND, json, &refusal)),
    };
    review_telemetry::index::set_content_index(settings.content_index);
    let root = chosen(dir, &settings)?;
    let (accounted, accounted_unknown) = accounted()?;
    let (status, note) = telemetry_status::status(&StatusInput {
        root,
        level: settings.level,
        max_bytes: settings.max_bytes,
        registry: Some(registry()?),
        accounted,
        accounted_unknown,
    })?;
    if let Some(note) = note {
        eprintln!("{COMMAND}: {note}");
    }
    if let Some(warning) = over_max_bytes_warning(&status) {
        eprintln!("{COMMAND}: {warning}");
    }
    let status = match config::home() {
        Ok(home) => status.relative_to(&home),
        Err(_) => status,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status).map_err(|error| error.to_string())?
        );
    } else {
        print!("{}", render_status(&status));
    }
    Ok(0)
}

fn render_status(status: &Status) -> String {
    let mut out = String::new();
    let level = status.level.as_str();
    let perturbing = if status.perturbing {
        " (perturbing: keeps content blobs of what Workers saw and did)"
    } else {
        ""
    };
    let _ = writeln!(out, "level        {level}{perturbing}");
    let _ = writeln!(
        out,
        "directory    {}{}",
        status.directory,
        if status.exists {
            ""
        } else {
            "   (nothing written yet)"
        }
    );
    let _ = writeln!(
        out,
        "bytes        {} of a {} cap ([telemetry] max_bytes; the storage sweep evicts sealed \
         segments to meet it){}",
        human_bytes(status.bytes.total),
        human_bytes(status.bytes.max_bytes),
        if status.over_max_bytes().is_some() {
            " — over"
        } else {
            ""
        }
    );
    for entry in &status.bytes.entries {
        if entry.bytes == 0 && entry.kind != "segment" {
            continue;
        }
        let state = entry
            .state
            .as_deref()
            .map(|state| match entry.capture.as_deref() {
                Some(capture) if capture != "complete" => format!(" ({state}; {capture})"),
                _ => format!(" ({state})"),
            })
            .unwrap_or_default();
        // Unrecognized paths are named, never read and never removed.
        let name = match (&entry.count, &entry.names) {
            (Some(count), Some(names)) => format!(
                "{count} path{}: {}{}",
                if *count == 1 { "" } else { "s" },
                names.join(", "),
                if *count > names.len() as u64 {
                    ", …"
                } else {
                    ""
                }
            ),
            _ => entry.name.clone(),
        };
        let _ = writeln!(
            out,
            "  {:<12} {:>10}  {name}{state}",
            entry.kind,
            human_bytes(entry.bytes),
        );
    }
    let segments = &status.segments;
    let _ = writeln!(
        out,
        "segments     {} live · {} unsealed · {} sealed · {} corrupt",
        segments.live, segments.unsealed, segments.sealed, segments.corrupt
    );
    let missing = match status.blob_verification {
        BlobVerification::Verified => status.missing_blobs.to_string(),
        BlobVerification::Deferred => "deferred (gc.lock busy)".into(),
    };
    let _ = writeln!(
        out,
        "runs         {} · withheld Attempts {} · missing blobs {missing}",
        status.runs, status.withheld_attempts
    );
    let index = &status.index;
    let _ = writeln!(
        out,
        "index        {} (ingested now: {} segments, {} records, {})",
        index.state,
        index.ingested_segments,
        index.ingested_records,
        human_bytes(index.ingested_bytes)
    );
    if status.problems.is_empty() {
        let _ = writeln!(out, "problems     none");
    } else {
        let _ = writeln!(out, "problems");
        for problem in &status.problems {
            let _ = writeln!(
                out,
                "  {:<10} {} ({} segments)",
                problem.what, problem.reason, problem.segments
            );
        }
    }
    for unreadable in &status.unreadable {
        let _ = writeln!(
            out,
            "unreadable   {}: {}",
            unreadable.path, unreadable.reason
        );
    }
    if !status.registered.is_empty() {
        let _ = writeln!(out, "registered directories");
        for dir in &status.registered {
            let mut notes = Vec::new();
            if dir.current {
                notes.push("this one".to_string());
            }
            if !dir.exists {
                notes.push("gone".to_string());
            }
            match (dir.overlap, &dir.overlapping) {
                (OverlapCheck::Unknown, Some(overlap)) => {
                    notes.push(format!("overlap unknown; at least: {overlap}"));
                }
                (OverlapCheck::Unknown, None) => {
                    notes.push("overlap unknown: the Store registry cannot be read".to_string());
                }
                (_, Some(overlap)) => notes.push(format!("overlapping: {overlap}")),
                (_, None) => {}
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join("; "))
            };
            let _ = writeln!(out, "  {}{notes}", dir.path);
        }
    }
    out
}

/// `af telemetry reindex`.
pub(crate) fn reindex(json: bool, dir: Option<PathBuf>) -> Result<i32, String> {
    const COMMAND: &str = "af telemetry reindex";
    let settings = match settings() {
        Ok(settings) => settings,
        Err(refusal) => return Ok(refused(COMMAND, json, &refusal)),
    };
    review_telemetry::index::set_content_index(settings.content_index);
    let root = chosen(dir, &settings)?;
    if !root.is_dir() {
        return Err(format!(
            "no telemetry directory at {}; nothing has been recorded",
            root.display()
        ));
    }
    let document = telemetry_status::reindex(&root)?;
    let document = match config::home() {
        Ok(home) => document.relative_to(&home),
        Err(_) => document,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?
        );
    } else {
        print!("{}", render_reindex(&document));
    }
    Ok(0)
}

fn render_reindex(document: &Reindex) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "reindexed    {}", document.directory);
    let _ = writeln!(
        out,
        "segments     {} ({} records): {} spans, {} events, {} blobs, {} occurrences",
        document.segments,
        document.records,
        document.spans,
        document.events,
        document.blobs,
        document.blob_refs
    );
    for recovered in &document.recovered {
        let _ = writeln!(
            out,
            "recovered    {} (discarded {} bytes, {} unended spans)",
            recovered.segment_id, recovered.discarded_bytes, recovered.unended_spans
        );
    }
    for corrupt in &document.corrupt {
        let _ = writeln!(out, "corrupt      {corrupt} (renamed .corrupt, never read)");
    }
    if document.blob_verification == BlobVerification::Deferred {
        let _ = writeln!(
            out,
            "blobs        verification deferred: gc.lock was not had within 2 s; no blob file \
             was read"
        );
    }
    if document.missing_blobs > 0 {
        let _ = writeln!(
            out,
            "missing      {} occurrences: content unavailable: missing blob",
            document.missing_blobs
        );
    }
    for digest in &document.corrupt_blobs {
        let _ = writeln!(out, "corrupt blob {digest}");
    }
    for unreadable in &document.unreadable {
        let _ = writeln!(
            out,
            "unreadable   {}: {}",
            unreadable.path, unreadable.reason
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(total: u64, max_bytes: u64) -> Status {
        serde_json::from_value(serde_json::json!({
            "schema": "af/telemetry-status@1",
            "level": "content",
            "perturbing": true,
            "directory": "~/.local/state/af/telemetry",
            "exists": true,
            "bytes": {"total": total, "max_bytes": max_bytes, "entries": []},
            "segments": {"live": 0, "unsealed": 0, "sealed": 0, "corrupt": 0},
            "runs": 0,
            "withheld_attempts": 0,
            "missing_blobs": 0,
            "blob_verification": "verified",
            "index": {
                "state": "fresh",
                "ingested_segments": 0,
                "ingested_records": 0,
                "ingested_bytes": 0
            },
            "problems": [],
            "unreadable": [],
            "registered": []
        }))
        .unwrap()
    }

    /// The Storage Budget enforces `[telemetry] max_bytes`: `status` says so on its bytes line
    /// and warns when the directory is over it that the next sweep evicts (audit L17), and names
    /// no overhead gate (I3).
    #[test]
    fn status_warns_that_the_next_sweep_evicts_when_over_the_cap() {
        let under = status(1 << 20, 256 << 20);
        assert_eq!(over_max_bytes_warning(&under), None);
        let text = render_status(&under);
        assert!(
            text.contains("the storage sweep evicts sealed segments"),
            "{text}"
        );
        assert!(!text.contains("advisory"), "{text}");
        assert!(!text.contains("— over"), "{text}");
        assert!(!text.contains("overhead gate"), "{text}");

        let over = status(300 << 20, 256 << 20);
        let warning = over_max_bytes_warning(&over).unwrap();
        assert!(warning.contains("over [telemetry] max_bytes"), "{warning}");
        assert!(
            warning.contains("the next storage sweep evicts sealed segments to meet it"),
            "{warning}"
        );
        assert!(!warning.contains("advisory"), "{warning}");
        assert!(render_status(&over).contains("— over"));
    }

    /// At `metrics` the segment header records exactly the subcommand chain its caller passes
    /// to `Telemetry::start`: this test parses `af provider setup alice --kind claude` with clap
    /// itself and passes `provider setup`, never the positional `alice` a guess from leading
    /// lower-case words would take; without a chain it records `argv[0]` alone (audit E1). The
    /// chain is the one [`recording::command_chain`] passes to `begin` for the same `argv`.
    #[test]
    fn the_metrics_header_records_the_subcommand_chain_its_caller_passes() {
        use clap::CommandFactory as _;
        use review_telemetry::level::Level;
        use review_telemetry::record::{Attrs, Record, SealExit, SpanKind, SpanStatus};
        use review_telemetry::{Start, Telemetry};

        let matches = crate::cli::Af::command()
            .try_get_matches_from(["af", "provider", "setup", "alice", "--kind", "claude"])
            .unwrap();
        let mut chain = Vec::new();
        let mut current = &matches;
        while let Some((name, next)) = current.subcommand() {
            chain.push(name.to_owned());
            current = next;
        }
        assert_eq!(chain, ["provider", "setup"]);
        let argv = ["af", "provider", "setup", "alice", "--kind", "claude"].map(String::from);
        assert_eq!(
            recording::command_chain(&argv),
            chain,
            "what `begin` is passed"
        );
        let header = |command: Vec<String>| {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let telemetry = Telemetry::start(Start {
                settings: Settings {
                    level: Level::Metrics,
                    ..Settings::default()
                },
                directory: root.join("state/af/telemetry"),
                registry: dirs::location(&root.join("state")),
                accounted: vec![Accounted::container(
                    root.join("state/af"),
                    "the state root",
                )],
                accounted_unknown: None,
                af_version: "0.0.0-test".into(),
                af_commit: "test".into(),
                write_hook: None,
                command,
            });
            let writer = telemetry.writer().unwrap();
            let span = writer
                .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
                .unwrap();
            writer.span_end(&span, SpanStatus::Ok, Attrs::new(), 0);
            let finished = telemetry.finish(SealExit::Ok);
            let sealed = finished.sealed.unwrap();
            let records = review_telemetry::segment::reader::read_sealed(&sealed.path).unwrap();
            records
                .into_iter()
                .find_map(|record| match record {
                    Record::Segment(header) => Some(header.argv),
                    _ => None,
                })
                .unwrap()
        };
        // `argv[0]` is this test binary, its home prefix redacted to `~`.
        let recorded = header(chain);
        assert_eq!(recorded[1..], ["provider", "setup"], "{recorded:?}");
        assert!(!recorded.iter().any(|word| word.contains("alice")));
        let alone = header(Vec::new());
        assert_eq!(alone.len(), 1, "{alone:?}");
        assert_eq!(alone[0], recorded[0]);
    }
}
