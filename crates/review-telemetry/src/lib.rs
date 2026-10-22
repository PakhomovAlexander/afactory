//! Telemetry (ADR-0153): what `af` does while it works, recorded in its own store under the
//! state root's `telemetry/`, never in a Task or Campaign Store, and never read by the kernel's
//! execution authority.
//!
//! A leaf crate: it links nothing in the workspace, and `review-process` never links it
//! (ADR-0026). The write side — [`record`], [`segment::writer`], [`blob`], [`redact`],
//! [`level`], [`subscriber`], [`trace`], [`dirs`], [`process`], [`stream`], [`content`] — is
//! for every crate that records. The read side — [`segment::reader`], [`index`], [`status`],
//! [`view`], [`ledger`], [`accounting`], [`turns`], [`content_view`], [`budget`],
//! [`blob::reader`] and [`blob::collect`] — is linked only by
//! `af telemetry`, `af storage` and the TUI (clause 1); a source test holds every other crate
//! to that.
//!
//! [`Telemetry::start`] is the one entry point of a recording process. At `off` it returns a
//! handle that touches nothing — no directory, no subscriber; at any other level the segment
//! opens lazily at the first record, after the directory is registered, and a telemetry-only
//! failure is one problem while the run continues (clause 7).

pub mod accounting;
pub mod blob;
pub mod budget;
pub mod content;
pub mod content_view;
pub mod dirs;
pub mod index;
pub mod layout;
pub mod ledger;
pub mod level;
pub mod process;
pub mod record;
pub mod redact;
pub mod segment;
pub mod status;
pub mod stream;
pub mod subscriber;
pub mod trace;
pub mod turns;
pub mod view;

use std::path::PathBuf;

/// `tracing`, for [`process_span!`]: a recording crate opens a process span without naming the
/// substrate's version itself.
#[doc(hidden)]
pub use tracing as __tracing;

use crate::dirs::Accounted;
use crate::level::{Level, Settings};
use crate::record::{ProblemWhat, SealExit};
use crate::segment::writer::{Registration, Sealed, Writer, WriterOptions};

/// What a recording process starts Telemetry with.
#[derive(Clone)]
pub struct Start {
    pub settings: Settings,
    /// The telemetry directory: `[telemetry] dir`, or the state root's `telemetry/`.
    pub directory: PathBuf,
    /// The directory registry, `$XDG_STATE_HOME/af/telemetry-dirs.v1.json`.
    pub registry: PathBuf,
    /// Every directory the storage inventory accounts on its own; registration refuses a
    /// telemetry directory that overlaps one.
    pub accounted: Vec<Accounted>,
    /// Why `accounted` is known incomplete (the Store registry could not be read): registration
    /// then refuses, since no overlap can be ruled out.
    pub accounted_unknown: Option<String>,
    pub af_version: String,
    pub af_commit: String,
    /// Called by the segment's owner before each write (fixtures stall it); `None` in
    /// production.
    pub write_hook: Option<segment::writer::WriteHook>,
    /// The subcommand chain the caller's argument parser matched (`["provider", "setup"]` for
    /// `af provider setup alice --kind claude`), passed in by the caller that parsed it: at
    /// `metrics` the header records `argv[0]` and exactly these words. Empty when no chain is
    /// known: the header then records `argv[0]` alone, never a guess from `argv`.
    pub command: Vec<String>,
}

/// One process's Telemetry.
pub struct Telemetry {
    writer: Option<Writer>,
    problems: Vec<(ProblemWhat, String)>,
}

/// What a finished Telemetry left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Finished {
    /// The sealed segment, when one was written.
    pub sealed: Option<Sealed>,
    /// Every distinct telemetry-only problem: reported once by the caller; never a run failure.
    pub problems: Vec<(ProblemWhat, String)>,
}

impl Telemetry {
    /// Start Telemetry. At `off` nothing is created, now or later.
    pub fn start(start: Start) -> Self {
        if start.settings.level == Level::Off {
            return Self::off();
        }
        let mut options = WriterOptions::new(start.directory, start.settings.level)
            .with_settings(&start.settings);
        options.af_version = start.af_version;
        options.af_commit = start.af_commit;
        options.write_hook = start.write_hook;
        if !start.command.is_empty() {
            options.command = options
                .argv
                .iter()
                .take(1)
                .cloned()
                .chain(start.command)
                .collect();
        }
        options.registration = Some(Registration {
            registry: start.registry,
            accounted: start.accounted,
            accounted_unknown: start.accounted_unknown,
        });
        Self {
            writer: Writer::new(options),
            problems: Vec::new(),
        }
    }

    /// Telemetry with a writer configured by hand (fixtures, and P2's kernel wiring).
    pub fn with_writer(options: WriterOptions) -> Self {
        Self {
            writer: Writer::new(options),
            problems: Vec::new(),
        }
    }

    /// Telemetry at `off`.
    pub fn off() -> Self {
        Self {
            writer: None,
            problems: Vec::new(),
        }
    }

    /// The writer, at a level that records and while no telemetry-only failure stopped it.
    pub fn writer(&self) -> Option<&Writer> {
        self.writer.as_ref().filter(|writer| !writer.failed())
    }

    /// Install the `tracing` subscriber as the process's global default. At `off` nothing is
    /// installed. A failure is a problem, never a run failure.
    pub fn install_subscriber(&mut self) {
        if let Some(writer) = &self.writer
            && let Err(reason) = subscriber::install(writer.clone())
        {
            self.problems.push((ProblemWhat::Write, reason));
        }
    }

    /// The shutdown begins (the run spans' ends, the `af.command` end, then the seal): from here
    /// every write is the consumer thread's and every wait shares one bound — 2 s, renewed by
    /// each content a released stream publishes under the seal (`Writer::renew_shutdown`).
    pub fn begin_terminal(&self) {
        if let Some(writer) = self.writer() {
            writer.begin_terminal();
        }
    }

    /// Seal the segment and report every problem.
    pub fn finish(self, exit: SealExit) -> Finished {
        let mut problems = self.problems;
        let Some(writer) = self.writer else {
            return Finished {
                sealed: None,
                problems,
            };
        };
        let sealed = if writer.failed() {
            None
        } else {
            match writer.finish(exit) {
                Ok(sealed) => sealed,
                Err(reason) => {
                    problems.push((ProblemWhat::Write, reason));
                    None
                }
            }
        };
        for problem in writer.problems() {
            if !problems.contains(&problem) {
                problems.push(problem);
            }
        }
        Finished { sealed, problems }
    }
}
