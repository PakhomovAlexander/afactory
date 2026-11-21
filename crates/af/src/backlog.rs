//! Backlog command parsing and presentation. Project Store operations live in af-project.

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use af_contracts::backlog::BacklogRecordV2;
use af_contracts::backlog::batch::{Reconsideration, Settlement};
use af_contracts::backlog::intake::MAX_EXCHANGE_BYTES;
use af_project::backlog::{prepare_report, read_evidence};
use af_project::{ProjectStore, require_checkout, store_directory};
use clap::Subcommand;
use serde::{Serialize, de::DeserializeOwned};
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
    /// List locally captured Report IDs awaiting an authority receipt
    Outbox {
        /// Print pending Report IDs as JSON
        #[arg(long)]
        json: bool,
    },
    /// Write a signed private submission to stdout
    Export {
        /// Producer Report ID to sign and export
        id: String,
        /// Local producer's Minisign secret key file
        #[arg(long)]
        key: PathBuf,
    },
    /// Commit a signed submission and write the signed authority receipt to stdout
    Accept {
        /// Path to the signed producer submission
        submission: PathBuf,
        /// Designated authority's Minisign secret key file
        #[arg(long)]
        key: PathBuf,
    },
    /// Apply a signed authority receipt to the local outbox
    Ack {
        /// Path to the signed authority receipt
        receipt: PathBuf,
        /// Print the acknowledged receipt identity as JSON
        #[arg(long)]
        json: bool,
    },
    /// Manually admit and account for local daily authority batches
    Batch {
        #[command(subcommand)]
        command: BatchCommand,
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

#[derive(Debug, Subcommand)]
pub(crate) enum BatchCommand {
    /// Evaluate occurrences through an explicit clock and print the private JSON audit
    Tick {
        /// Evaluate scheduled occurrences through this Unix time in milliseconds
        #[arg(long)]
        at_unix_ms: u64,
    },
    /// Print private JSON audit without changing the Store
    Audit,
    /// Queue an accounted Report for reconsideration in a future batch
    Reconsider {
        /// Existing item to reconsider
        #[arg(long, value_name = "ID")]
        item_id: String,
        /// Previously accounted authority Report ID
        #[arg(long, value_name = "ID")]
        report_id: String,
        /// Later accepted Report supplying new evidence
        #[arg(long)]
        evidence_report_id: Option<String>,
        /// Earliest eligible occurrence time in Unix milliseconds
        #[arg(long, value_name = "MS")]
        eligible_unix_ms: u64,
    },
    /// Record a local disposition for a frozen member
    Settle {
        /// Frozen authority batch to settle
        batch_id: String,
        /// Exact frozen member within the batch
        member_id: String,
        /// Local disposition to record for the member
        outcome: LocalOutcome,
        /// Item identity for a local reservation
        #[arg(long, required_if_eq("outcome", "reserve-publication"))]
        item_id: Option<String>,
    },
    /// Close a batch whose members all have durable dispositions
    Close {
        /// Frozen authority batch to close
        batch_id: String,
        /// Close at this Unix time in milliseconds
        #[arg(long)]
        at_unix_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum LocalOutcome {
    Accounted,
    Held,
    NeedsEvidence,
    ReservePublication,
}

fn batch_write(store: &mut ProjectStore, command: BatchCommand) -> Result<i32, String> {
    let audit = match command {
        BatchCommand::Audit => {
            unreachable!("read-only batch audit handled before opening a writer")
        }
        BatchCommand::Tick { at_unix_ms } => store.tick_backlog_batch(at_unix_ms)?,
        BatchCommand::Reconsider {
            item_id,
            report_id,
            evidence_report_id,
            eligible_unix_ms,
        } => store.reconsider_backlog_batch(Reconsideration {
            item_id,
            report_id,
            evidence_report_id,
            eligible_unix_ms,
        })?,
        BatchCommand::Settle {
            batch_id,
            member_id,
            outcome,
            item_id,
        } => {
            let settlement = match (outcome, item_id) {
                (LocalOutcome::Accounted, None) => Settlement::Accounted,
                (LocalOutcome::Held, None) => Settlement::Held,
                (LocalOutcome::NeedsEvidence, None) => Settlement::NeedsEvidence,
                (LocalOutcome::ReservePublication, Some(item_id)) => {
                    Settlement::ReservePublication { item_id }
                }
                _ => return Err("provide --item-id only with reserve-publication".into()),
            };
            store.settle_backlog_batch(batch_id, member_id, settlement)?
        }
        BatchCommand::Close {
            batch_id,
            at_unix_ms,
        } => store.close_backlog_batch(batch_id, at_unix_ms)?,
    };
    print_document(&audit)
}

fn new_report_id() -> Result<String, String> {
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|_| "could not generate a backlog report ID")?;
    Ok(format!("r-{}", af_contracts::hex::encode(&bytes)))
}

fn print_document(value: &impl Serialize) -> Result<i32, String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(0)
}

fn print_json(view: &str, data: Value) -> Result<i32, String> {
    print_document(&json!({"schema": "af/backlog@2", "view": view, "data": data}))
}

fn read_exchange<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = af_store::local_fs::read_bounded(path, MAX_EXCHANGE_BYTES)
        .map_err(|error| format!("reading backlog exchange: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|_| "invalid backlog exchange document".into())
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
        let authority = receipt.authority_receipt.as_deref().unwrap_or("pending");
        println!(
            "Captured {} in Project {}. Authority receipt {}.",
            receipt.report_id, receipt.project_id, authority
        );
        return Ok(0);
    }

    if let Command::Accept { submission, key } = &command {
        let submission = read_exchange(submission)?;
        let (mut store, _) = crate::project::adopt(&checkout, None)?;
        let receipt = store.accept_backlog_submission(submission, key)?;
        return print_document(&receipt);
    }
    if let Command::Ack { receipt, json } = &command {
        let receipt = read_exchange(receipt)?;
        let (mut store, _) = crate::project::adopt(&checkout, None)?;
        let receipt_id = store.acknowledge_backlog_submission(receipt)?;
        if *json {
            return print_json("acknowledged", json!({"authority_receipt": receipt_id}));
        }
        println!("Acknowledged authority receipt {receipt_id}");
        return Ok(0);
    }

    let command = match command {
        audit @ Command::Batch {
            command: BatchCommand::Audit,
        } => audit,
        Command::Batch { command } => {
            let (mut store, _) = crate::project::adopt(&checkout, None)?;
            return batch_write(&mut store, command);
        }
        command => command,
    };

    let state = crate::xdg_state_root()?;
    let store = match std::fs::symlink_metadata(store_directory(&state, &checkout.project_id)) {
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        _ => Some(ProjectStore::open_read_only(&state, &checkout.project_id)?),
    };
    let project_id = checkout.project_id;
    match command {
        Command::Batch {
            command: BatchCommand::Audit,
        } => {
            let store = store.ok_or("no local Project backlog")?;
            return print_document(&store.backlog_batch_audit()?);
        }
        Command::Record { .. }
        | Command::Accept { .. }
        | Command::Ack { .. }
        | Command::Batch { .. } => {
            unreachable!("backlog writes handled above")
        }
        Command::Export { id, key } => {
            let store = store.ok_or("no local Project backlog")?;
            return print_document(&store.backlog_submission(&id, &key)?);
        }
        Command::Outbox { json } => {
            let pending = store
                .as_ref()
                .map(ProjectStore::backlog_outbox)
                .transpose()?
                .unwrap_or_default();
            if json {
                return print_json(
                    "outbox",
                    json!({"project_id": project_id, "report_ids": pending}),
                );
            }
            println!("Pending backlog submissions for {project_id}");
            for id in pending {
                println!("{id}");
            }
        }
        Command::Inbox { json } => {
            let reports = store
                .as_ref()
                .map(ProjectStore::backlog_inbox)
                .transpose()?
                .unwrap_or_default();
            if json {
                let reports = reports
                    .into_iter()
                    .map(BacklogRecordV2::Report)
                    .collect::<Vec<_>>();
                return print_json(
                    "inbox",
                    json!({"project_id": project_id, "reports": reports}),
                );
            }
            println!("Private backlog inbox for {project_id}");
            for report in reports {
                println!(
                    "{}  {}",
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
            let details = store
                .as_ref()
                .map(|store| store.private_backlog_report(&id))
                .transpose()?
                .flatten()
                .ok_or("no private report with that ID in this Project")?;
            let report = details.report;
            if json {
                let report = BacklogRecordV2::Report(report);
                if details.outcomes.is_empty() && details.authority_receipt.is_none() {
                    return print_json("report", json!(report));
                }
                return print_json(
                    "report_details",
                    json!({
                        "report": report,
                        "authority_receipt": details.authority_receipt,
                        "outcomes": details.outcomes,
                    }),
                );
            }
            println!(
                "{}  {}",
                report.report_id,
                crate::terminal_text(&report.title)
            );
            if let Some(receipt) = details.authority_receipt {
                println!(
                    "Authority {} received report at sequence {}.",
                    receipt.authority_id,
                    receipt.sequence.get()
                );
            } else {
                println!("Captured locally. Authority receipt pending.");
            }
            for outcome in details.outcomes {
                let disposition =
                    serde_json::to_value(outcome.disposition).map_err(|e| e.to_string())?;
                println!(
                    "{}  {}  {}",
                    outcome.decision_id,
                    disposition.as_str().unwrap_or_default(),
                    crate::terminal_lines(&outcome.reason)
                );
                if let Some(matched) = outcome.local_match {
                    println!(
                        "Matched item {} revision {}",
                        matched.item_id, matched.revision_id
                    );
                }
            }
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
