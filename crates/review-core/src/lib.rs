//! Review Kernel v1 contracts.
//!
//! The types here mirror the checked-in JSON Schemas in `../../schemas/` one-for-one; the
//! schemas are the language-neutral contract and these are the Rust view of them. Tests assert
//! the two agree, in both directions, so a field added to one and forgotten in the other fails
//! the build rather than surfacing as a silently dropped value at runtime.
//!
//! Two rules from the design are enforced in code rather than left to reviewers:
//!
//! - Reports are immutable claims. Nothing here offers a way to merge, edit, or collapse one.
//! - JSON payloads live in the I-JSON numeric domain ([`json::admit`]) before they are hashed,
//!   so a value cannot change meaning between producer and consumer.

pub mod envelope;
pub mod event;
pub mod exec;
pub mod finding;
pub mod json;
pub mod legacy;
pub mod patch;
pub mod snapshot;
pub mod subject;

pub use envelope::{ArtifactEnvelope, Producer};
pub use event::{
    EventType, MissingNodeV2, NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1, PortArtifactsV1,
    PortCardinality, RunEvent, RunFailureReasonV2, RunNodeOutcomeV2, RunNodeReportV2,
    RunReportPayloadV2, RunSuppressionReasonV2, RunVerdictV2, SnapshotAffinity, UnknownEventType,
    run_report_closes_round,
};
pub use exec::{Arg, ArgError, Command, Provenance};
pub use finding::{FindingReport, Location, Relation, RelationKind, Severity};
pub use json::{NumericDomainError, admit};
pub use legacy::{LegacyImportError, LegacyStageOutput};
pub use patch::{ClaimRef, ClaimRefKind, PatchProposal};
pub use snapshot::{Capture, SourceSnapshot, Submodule};
pub use subject::SubjectKind;

/// Contract type URIs, as they appear in an [`ArtifactEnvelope::artifact_type`].
pub mod contract {
    pub const FINDING_REPORT_V1: &str = "review.kernel/FindingReport@1";
    pub const FINDING_SET_V1: &str = "review.kernel/FindingSet@1";
    pub const GATE_DECISION_V1: &str = "review.kernel/GateDecision@1";
    pub const OPAQUE_V1: &str = "review.kernel/Opaque@1";
    pub const PATCH_PROPOSAL_V1: &str = "review.kernel/PatchProposal@1";
    pub const PRIOR_FINDINGS_V1: &str = "review.kernel/PriorFindings@1";
    pub const REPORT_SET_V1: &str = "review.kernel/ReportSet@1";
    pub const REVIEWER_RESULT_V1: &str = "review.kernel/ReviewerResult@1";
    pub const SOURCE_SNAPSHOT_V1: &str = "review.kernel/SourceSnapshot@1";
}
