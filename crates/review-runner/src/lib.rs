//! Worker runners: the supervised process transport, the pure input composition that
//! `af review render` and the Task host share, and the Task Worker adapter contract.
//!
//! A `command` Worker is deterministic — its output is a function of its input — so every
//! property of the Worker contract can be proved before a model is ever invoked. A model
//! adapter is then a *different runner behind the same contract*, and nothing above it has to
//! change.

pub mod model;
pub mod session;
pub mod task;

pub use model::{
    CommandAdapter, ContextEntry, ContextManifest, InputTransport, ModelRunner, NotesRequest,
    RenderedInput, ReviewerAdapter, ReviewerAttemptContext, ReviewerInputArtifact, ReviewerInputs,
    ReviewerNoteHint, ReviewerNotesDeclaration, ReviewerProposalDeclaration, RunnerError,
    TokenUsage, append_focus, compose_command_input, compose_model_prompt, estimate_tokens,
    parse_notes_declaration, parse_proposal_declaration, parse_reviewer_result,
};
pub use review_core::{MAX_CHANGE_SET_BYTES, MAX_PRIOR_FINDINGS_BYTES};
pub use review_process::run_supervised;
pub use session::{
    CapturedSession, SessionCapture, SessionDeletion, SessionLayer, SessionResume,
    rehydrate_transcript, sanitize_transcript,
};

use std::collections::BTreeMap;
use std::path::PathBuf;

use review_core::Command;

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
    pub runner: Command,
    files: BTreeMap<String, Vec<u8>>,
}

impl ResolvedReviewer {
    /// Assemble package bytes for an adapter. This type is transport, not authorization: the
    /// lock resolver is responsible for pin admission before placing it in a loaded pipeline.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        digest: impl Into<String>,
        root: impl Into<PathBuf>,
        runner: Command,
        files: BTreeMap<String, Vec<u8>>,
    ) -> ResolvedReviewer {
        ResolvedReviewer {
            name: name.into(),
            version: version.into(),
            digest: digest.into(),
            root: root.into(),
            runner,
            files,
        }
    }

    /// A package file, from the digest-verified bytes. The only way to read package content
    /// after resolution; there is deliberately no path back to the filesystem.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    /// Every verified package file. Used to publish campaign authority without returning to
    /// the package directory after resolution.
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }
}
