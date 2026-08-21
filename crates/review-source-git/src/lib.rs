//! The Git source adapter.
//!
//! Its whole job is to turn a repository into something immutable and identified by content, so
//! that "every reviewer inspected the same thing" becomes a digest anyone can recompute rather
//! than a hope. Everything here is offline and read-only: no fetch, no transport, no hook, no
//! filter, and no write to the checkout under review.
//!
//! Git is used through a typed adapter rather than a library. Generic calls admit only plumbing
//! that does not transform content; tree diffing is reachable only through opaque resolved tree
//! ids and a fully fixed invocation. Worktree bytes are read from the filesystem directly, so a
//! `.gitattributes` in the candidate tree has nothing to interpose on during capture.

pub mod capture;
pub mod git;
pub mod manifest;
pub mod materialize;

pub use capture::{Capture, CaptureError, CaptureObserver, NoObserver, Snapshot, worktree_state};
pub use git::{GitError, Repo, TreeChange, TreeChangeKind, TreeDiff, TreeId};
pub use manifest::{Entry, EntryKind, Manifest, decode_path, digest_bytes, encode_path, fs_path};
pub use materialize::{MaterializeError, materialize};
