//! Review Kernel storage: canonical identity, durable artifacts, the append-only run log, and
//! the rebuildable Findings Ledger projection.
//!
//! The layering is deliberate and one-directional:
//!
//! ```text
//!   canonical  ->  cas  ->  store  ->  ledger        ingest drives all four
//!   (identity)    (bytes)   (log)     (projection)
//! ```
//!
//! Nothing below a layer knows about anything above it, and the projection holds no state the
//! log cannot rebuild. `LedgerProjection::rebuild` is the only constructor for that reason: there is no
//! path by which hand-edited state can enter.

pub mod canonical;
pub mod cas;
pub mod ingest;
pub mod ledger;
pub mod optimization;
pub mod shared;
pub mod store;
pub mod subject;

pub use canonical::{CanonicalError, artifact_id, canonicalize, content_id, validate_envelope};
pub use cas::{Cas, CasError, OpenedCasObject};
pub use ingest::{
    CanonicalReduction, CanonicalStage, Ingest, PreparedReviewReduction,
    prepare_canonical_task_review,
};
pub use ledger::{
    AttachedReport, Convergence, ConvergencePolicy, Finding, Ledger, LedgerProjection, ReportScope,
    ScopeAuthorityFailure, ScopeAuthorityKind, Status, Verdict,
};
pub use shared::SharedEventStore;
pub use store::{EventStore, NewEvent, StoreError, TaskAttemptWall};
pub use subject::{
    ResolvedChangeSet, ResolvedSubject, ResolvedSubjectScope, resolve_subject,
    resolve_subject_scope,
};
