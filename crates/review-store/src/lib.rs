//! Review Kernel storage: canonical identity, durable artifacts, the append-only run log, and
//! the rebuildable Findings Ledger projection.
//!
//! The layering is deliberate and one-directional:
//!
//! ```text
//!   canonical  ->  cas  ->  store  ->  ledger        legacy drives all four
//!   (identity)    (bytes)   (log)     (projection)
//! ```
//!
//! Nothing below a layer knows about anything above it, and the projection holds no state the
//! log cannot rebuild. `Ledger::rebuild` is the only constructor for that reason: there is no
//! path by which hand-edited state can enter.

pub mod canonical;
pub mod cas;
pub mod ledger;
pub mod legacy;
pub mod store;
pub mod subject;

pub use canonical::{CanonicalError, artifact_id, canonicalize, content_id};
pub use cas::{Cas, CasError, OpenedCasObject};
pub use ledger::{
    AttachedReport, Convergence, ConvergencePolicy, Finding, Ledger, LedgerProjection, ReportScope,
    ScopeAuthorityFailure, ScopeAuthorityKind, Status, Verdict,
};
pub use legacy::{AddSummary, Ingest, LegacyRow, import_ledger_jsonl, legacy_fingerprint};
pub use store::{EventStore, NewEvent, StoreError, validate_reviewer_result};
pub use subject::{
    ResolvedChangeSet, ResolvedSubject, ResolvedSubjectScope, resolve_subject,
    resolve_subject_scope,
};
