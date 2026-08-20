//! Reviewer adapters, and the gather barrier that makes concurrency deterministic.
//!
//! The `command` adapter comes first deliberately: it is the only one whose output is a function
//! of its input, so every property below can be proved before a model is ever invoked. A model
//! adapter is then a *different runner behind the same contract*, and nothing above it has to
//! change.
//!
//! ## Why gather has a barrier
//!
//! Reviewers run concurrently, so they finish in whatever order the machine felt like. The
//! projection is order-dependent — the legacy semantics give a finding to its **first**
//! reporter, and later ones become duplicates — so ingesting in completion order would make the
//! ledger depend on scheduling. Two identical runs would disagree about which reviewer owns a
//! finding, and a replay would not reproduce the run it replays.
//!
//! So results are admitted in a canonical order (by node ID), never in the order they arrived.
//! Concurrency stays; nondeterminism does not. [`tests/determinism.rs`] proves both halves: the
//! canonical order is stable under randomized completion, and completion order is genuinely
//! order-dependent — so the barrier is load-bearing rather than ceremonial.

pub mod command_runner;
pub mod model;

pub use command_runner::{CommandRunner, RunnerError};
pub use model::{
    Grant, ModelRunner, RESULT_CONTRACT, RawCapture, ReviewerAdapter, ReviewerInputs,
    ReviewerReturn, extract_result, parse_stage_output, unfence,
};

use std::collections::BTreeMap;
use std::path::PathBuf;

use review_core::{Command, LegacyStageOutput, SubjectKind};

/// A reviewer package after resolution: located, digest-verified, manifest-checked — carrying
/// the verified bytes themselves. It lives here, at the adapter boundary, so a provider
/// adapter depends on exactly what it consumes instead of compiling the pipeline-definition
/// parser that produced it.
#[derive(Debug, Clone)]
pub struct ResolvedReviewer {
    pub name: String,
    pub version: String,
    pub digest: String,
    pub root: PathBuf,
    pub subjects: Vec<SubjectKind>,
    pub runner: Command,
    files: BTreeMap<String, Vec<u8>>,
}

impl ResolvedReviewer {
    /// Only a resolver that has just verified `files` against the pinned digest should call
    /// this — the bytes given here are what [`ResolvedReviewer::file`] will forever answer.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        digest: impl Into<String>,
        root: impl Into<PathBuf>,
        subjects: Vec<SubjectKind>,
        runner: Command,
        files: BTreeMap<String, Vec<u8>>,
    ) -> ResolvedReviewer {
        ResolvedReviewer {
            name: name.into(),
            version: version.into(),
            digest: digest.into(),
            root: root.into(),
            subjects,
            runner,
            files,
        }
    }

    /// A package file, from the digest-verified bytes. The only way to read package content
    /// after resolution; there is deliberately no path back to the filesystem.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }
}

/// One reviewer's dispatch: which node, and what it is being asked to inspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The node's identity in the pipeline. Also the canonical gather key, which is why it must
    /// be unique and stable rather than a display name.
    pub node_id: String,
    pub reviewer: String,
    pub subject_snapshot_id: Option<String>,
    /// Exact artifact IDs this reviewer was given. No node consumes ambient input.
    pub input_artifacts: Vec<String>,
}

impl Invocation {
    pub fn new(node_id: impl Into<String>, reviewer: impl Into<String>) -> Invocation {
        Invocation {
            node_id: node_id.into(),
            reviewer: reviewer.into(),
            subject_snapshot_id: None,
            input_artifacts: Vec::new(),
        }
    }

    pub fn on(mut self, snapshot_id: impl Into<String>) -> Invocation {
        self.subject_snapshot_id = Some(snapshot_id.into());
        self
    }
}

/// What a reviewer returned, or why it did not.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub invocation: Invocation,
    pub result: Result<LegacyStageOutput, RunnerError>,
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        self.result.is_ok()
    }
}

/// Admit concurrent outcomes in a canonical order.
///
/// Sorting by node ID is the whole mechanism. It is cheap, and it converts "whichever reviewer
/// happened to finish first" into a property of the pipeline definition instead of the machine.
pub fn gather(mut outcomes: Vec<Outcome>) -> Vec<Outcome> {
    outcomes.sort_by(|a, b| a.invocation.node_id.cmp(&b.invocation.node_id));
    outcomes
}
