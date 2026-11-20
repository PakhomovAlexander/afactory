//! Backlog command parsing and presentation. Project Store operations live in af-project.

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use af_contracts::backlog::BacklogRecordV1;
use af_project::backlog::{prepare_report, read_evidence};
use af_project::{ProjectStore, require_checkout, store_directory};
use clap::Subcommand;
use serde_json::{Value, json};

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Capture a private observed fact and optional UTF-8 evidence
    Record {
        /// Stable producer ID. Supply it before capture to recover a lost receipt.
        #[arg(long)]
        report_id: Option<String>,
        /// Short title for the observed fact
        #[arg(long)]
        title: String,
        /// Fact observed in this Workspace
        #[arg(long)]
        observed: String,
        /// Optional reproduction steps
        #[arg(long)]
        repro: Option<String>,
        /// Local UTF-8 evidence file. Repeat this flag to attach more files.
        #[arg(long = "evidence-file", value_name = "FILE")]
        evidence_files: Vec<PathBuf>,
        /// Print the local capture receipt as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect local private Reports
    Inbox {
        /// Print private Reports as JSON
        #[arg(long)]
        json: bool,
    },
    /// List verified backlog items
    List {
        /// Print verified items as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect a verified item or explicitly request a private Report
    Show {
        /// Verified item ID or, with --private, producer Report ID
        id: String,
        /// Read from the private Report inbox
        #[arg(long)]
        private: bool,
        /// Print the selected item or Report as JSON
        #[arg(long)]
        json: bool,
    },
}

fn new_report_id() -> Result<String, String> {
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|_| "could not generate a backlog report ID")?;
    Ok(format!("r-{}", af_contracts::hex::encode(&bytes)))
}

fn print_json(view: &str, data: Value) -> Result<i32, String> {
    let value = json!({"schema": "af/backlog@1", "view": view, "data": data});
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(0)
}

pub(crate) fn run(command: Command, repo: Option<&Path>) -> Result<i32, String> {
    let checkout = require_checkout(repo.unwrap_or_else(|| Path::new(".")))?;
    if let Command::Record {
        report_id,
        title,
        observed,
        repro,
        evidence_files,
        json,
    } = command
    {
        let id = report_id.map_or_else(new_report_id, Ok)?;
        let evidence = read_evidence(&evidence_files)?;
        let report = prepare_report(&checkout, id, title, observed, repro, evidence)?;
        let (mut store, _) = crate::project::adopt(&checkout, None)?;
        let receipt = store.record_backlog_report(report)?;
        if json {
            return print_json("capture", json!(receipt));
        }
        println!(
            "Captured {} in Project {}. Authority receipt pending.",
            receipt.report_id, receipt.project_id
        );
        return Ok(0);
    }

    let state = crate::xdg_state_root()?;
    let store = match std::fs::symlink_metadata(store_directory(&state, &checkout.project_id)) {
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        _ => Some(ProjectStore::open_read_only(&state, &checkout.project_id)?),
    };
    let project_id = checkout.project_id;
    match command {
        Command::Record { .. } => unreachable!("record handled above"),
        Command::Inbox { json } => {
            let reports = store
                .as_ref()
                .map(ProjectStore::backlog_inbox)
                .transpose()?
                .unwrap_or_default();
            if json {
                let reports = reports
                    .into_iter()
                    .map(BacklogRecordV1::Report)
                    .collect::<Vec<_>>();
                return print_json(
                    "inbox",
                    json!({"project_id": project_id, "reports": reports}),
                );
            }
            println!("Private backlog inbox for {project_id}");
            for report in reports {
                println!(
                    "{}  {} (captured locally)",
                    report.report_id,
                    crate::terminal_text(&report.title)
                );
            }
        }
        Command::Show {
            id,
            private: true,
            json,
        } => {
            let report = store
                .as_ref()
                .map(|store| store.backlog_report(&id))
                .transpose()?
                .flatten()
                .ok_or("no private report with that ID in this Project")?;
            if json {
                return print_json("report", json!(BacklogRecordV1::Report(report)));
            }
            println!(
                "{}  {}",
                report.report_id,
                crate::terminal_text(&report.title)
            );
            println!("Captured locally. Authority receipt pending.");
            println!("Workspace {}", report.workspace_id);
            println!(
                "Source {}",
                report.source_revision.as_deref().unwrap_or("unknown")
            );
            println!("{}", crate::terminal_lines(&report.observed));
            if let Some(repro) = report.repro {
                println!("Reproduction\n{}", crate::terminal_lines(&repro));
            }
            for evidence in report.evidence {
                println!(
                    "[{}]\n{}",
                    evidence.content_id,
                    crate::terminal_lines(&evidence.text)
                );
            }
        }
        Command::List { .. } | Command::Show { private: false, .. } => {
            let items = store
                .as_ref()
                .map(ProjectStore::verified_backlog)
                .transpose()?
                .unwrap_or_default();
            match command {
                Command::List { json } => {
                    if json {
                        return print_json(
                            "list",
                            json!({"project_id": project_id, "items": items}),
                        );
                    }
                    println!("Verified backlog for {project_id}");
                    for item in items {
                        println!(
                            "{}  {}",
                            item.item_id,
                            crate::terminal_text(&item.public.title)
                        );
                    }
                }
                Command::Show { id, json, .. } => {
                    let item = items
                        .into_iter()
                        .find(|item| item.item_id == id)
                        .ok_or("no verified item with that ID in this Project")?;
                    if json {
                        return print_json("item", json!(item));
                    }
                    println!(
                        "{}  {}",
                        item.item_id,
                        crate::terminal_text(&item.public.title)
                    );
                    println!("Verified revision {}", item.revision_id);
                    println!("{}", crate::terminal_lines(&item.public.body));
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(0)
}
