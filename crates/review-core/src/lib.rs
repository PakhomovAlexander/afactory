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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("af supports Linux and macOS only");

pub mod build_cache;
pub mod cache;
pub mod campaign;
pub mod change_set;
pub mod credential;
pub mod demand;
pub mod disposition;
pub mod envelope;
pub mod event;
pub mod exec;
pub mod finding;
pub mod finding_set;
pub mod grouping;
pub mod hex;
pub mod integration;
pub mod json;
pub mod legacy;
pub mod patch;
pub mod path;
pub mod resolution;
pub mod session;
pub mod slice;
pub mod snapshot;
pub mod subject;
pub mod task;
pub mod warm;
pub mod workspace;

pub use workspace::{
    WORKSPACE_ID_HEX_LEN, WorkspaceBasisV1, WorkspaceFallbackReasonV1, WorkspaceRebasedPayloadV1,
    is_workspace_id,
};

pub use warm::{
    BuildCacheDropReasonV1, DEFAULT_WORKER_NOTES_BYTES, HeadDeltaDropReasonV1, HeadDeltaEntryV1,
    HeadDeltaInputs, HeadDeltaMarkV1, HeadDeltaV1, InspectedPathV1, MAX_HEAD_DELTA_BYTES,
    MAX_WORKER_NOTES_BYTES, PathHintV1, TreeView, WarmLayerV1, WarmSetSelectedPayloadV1, WarmSetV1,
    WorkerNotesDropReasonV1, WorkerNotesRecordedPayloadV1, WorkerNotesV1, compute_head_delta_marks,
};

pub use session::{
    ColdCloseoutDispatchedPayloadV1, DEFAULT_SESSION_MAX_AGE_SECS, MAX_SESSION_MAX_AGE_SECS,
    MAX_SESSION_TRANSCRIPT_BYTES, SessionCleanupOutcomeV1, SessionCleanupRefusalV1,
    SessionDropReasonV1, SessionSnapshotCleanedPayloadV1, SessionSnapshotPreparedPayloadV1,
    SessionSnapshotV1, SessionSourceV1, attempt_of_session_id, is_session_id,
    session_id_for_attempt,
};

pub use build_cache::{
    BUILD_CACHE_MAX_DEPTH_V1, BUILD_CACHE_MAX_PATH_BYTES_V1, BuildCacheCapturedPayloadV1,
    BuildCacheKindV1, BuildCacheLimitsV1, BuildCacheRefusalReasonV1, BuildCacheTrustV1,
    BuildCacheV1, DEFAULT_BUILD_CACHE_BYTES_V1, DEFAULT_BUILD_CACHE_ENTRIES_V1,
    MAX_BUILD_CACHE_BYTES_V1, MAX_BUILD_CACHE_ENTRIES_V1, validate_build_cache_path_v1,
};

pub use cache::{
    CacheManifestEntryV1, CacheManifestV1, CachePathEncodingV1, MAX_CACHE_BYTES_V1,
    MAX_CACHE_COPY_BYTES_V1, MAX_CACHE_ENTRIES_V1, validate_cache_path_v1,
};
pub use campaign::{
    AuthorityFileV1, CANONICAL_FINDING_IDENTITY_POLICY, CampaignBudgetV1, CampaignConvergenceV1,
    CampaignManifestV1, CampaignOpenedPayloadV1, CampaignReviewerV1, ReviewerPackageV1,
    RoundInputSupersededPayloadV1, RoundStartedPayloadV1,
};
pub use change_set::{ChangeSetV1, PathRenameV1};
pub use credential::CredentialModeV1;
pub use demand::{
    DEMAND_REDUCER_VERSION, DemandRequirement, DemandSetEntryV1, DemandSetV1, DemandStatus,
    DemandV1, DemandWaiverV1, EvidenceReuseAdmissionV1, EvidenceSatisfactionV1, EvidenceV1,
    RecordedArtifactPayloadV1,
};
pub use disposition::{FindingDispositionPosition, FindingDispositionV1};
pub use envelope::{ArtifactEnvelope, Producer};
pub use event::{
    EventType, MissingNodeV2, NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1, PortArtifactsV1,
    PortCardinality, RunCacheFailureReasonV5, RunCacheFailureV5, RunCacheKindV5,
    RunCacheMaterializationV5, RunCacheSnapshotV5, RunEvent, RunExecutionBindingV4,
    RunExecutionProviderV4, RunFailureReasonV3, RunIsolationV4, RunNodeOutcomeV2, RunNodeReportV2,
    RunReportExecutionV6, RunReportPayloadV6, RunSandboxModeV4, RunSuppressionReasonV2,
    RunVerdictV3, SnapshotAffinity, TaskReviewAccountingV1, UnknownEventType, is_artifact_type,
    run_report_closes_round,
};
pub use exec::{Arg, ArgError, Command, Provenance};
pub use finding::{FindingReport, Location, Relation, RelationKind, Severity};
pub use finding_set::{FINDING_REDUCER_VERSION_V2, FindingSetEntryV1, FindingSetV1};
pub use grouping::{FindingGroupingAction, FindingGroupingEventPayloadV1, FindingGroupingV1};
pub use integration::{
    IntegrationCandidateV1, IntegrationCheckV1, IntegrationChecksCompletedPayloadV1,
    IntegrationChecksV1, IntegrationCommittedPayloadV1, IntegrationConflictPayloadV1,
    IntegrationPlanV1, IntegrationPreparedPayloadV1,
};
pub use json::{NumericDomainError, admit};
pub use legacy::{
    LegacyImportError, LegacyStageOutput, ReviewerResultContract, ReviewerResultRejection,
    validate_reviewer_result_v2, validate_reviewer_result_v2_classified,
};
pub use patch::{
    ClaimRef, ClaimRefKind, PatchProposal, ProposalAcceptedPayloadV1, ProposalCandidateV1,
    ProposalPreparedPayloadV1, ProposalRefusalReasonV1, ProposalRefusedPayloadV1,
};
pub use path::{contains_report_path, decode_path, encode_path, is_valid_repo_path};
pub use resolution::{
    ChangeAttestationV1, ChangedRegionV1, FindingResolutionOutcome, FindingResolutionV1,
    FixVerificationV1, PolicyTimeV1, ResolutionChallengeKind, ResolutionChallengeV1,
};
pub use slice::{
    CloseoutPolicyV1, RecordedSetPayloadV1, ReviewSliceV1, SemanticClosureV1,
    SemanticDispositionV1, ShardOutcomeV1, ShardReceiptV1, ShardSetV1, SliceCoverageV1,
    SliceSetAcceptedPayloadV1, SliceSetV1,
};
pub use snapshot::{Capture, SourceSnapshot, Submodule};
pub use subject::{SubjectKind, SubjectV1};

/// Maximum encoded size of the exact prior Finding Set delivered to any reviewer.
pub const MAX_PRIOR_FINDINGS_BYTES: usize = 64 * 1024;

/// Maximum encoded size of one exact Change Set delivered to a reviewer.
pub const MAX_CHANGE_SET_BYTES: usize = 4 * 1024 * 1024;

/// Canonical content-address spelling used by captured authority and persisted contracts.
pub fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// Contract type URIs, as they appear in an [`ArtifactEnvelope::artifact_type`].
pub mod contract {
    pub const CACHE_MANIFEST_V1: &str = "review.kernel/CacheManifest@1";
    pub const CHANGE_SET_V1: &str = "review.kernel/ChangeSet@1";
    pub const FINDING_REPORT_V1: &str = "review.kernel/FindingReport@1";
    pub const FINDING_DISPOSITION_V1: &str = "review.kernel/FindingDisposition@1";
    pub const FINDING_GROUPING_V1: &str = "review.kernel/FindingGrouping@1";
    pub const FINDING_SET_V1: &str = "review.kernel/FindingSet@1";
    pub const INTEGRATION_CHECKS_V1: &str = "review.kernel/IntegrationChecks@1";
    pub const INTEGRATION_PLAN_V1: &str = "review.kernel/IntegrationPlan@1";
    pub const DEMAND_V1: &str = "review.kernel/Demand@1";
    pub const DEMAND_SET_V1: &str = "review.kernel/DemandSet@1";
    pub const DEMAND_WAIVER_V1: &str = "review.kernel/DemandWaiver@1";
    pub const EVIDENCE_V1: &str = "review.kernel/Evidence@1";
    pub const EVIDENCE_REUSE_ADMISSION_V1: &str = "review.kernel/EvidenceReuseAdmission@1";
    pub const EVIDENCE_SATISFACTION_V1: &str = "review.kernel/EvidenceSatisfaction@1";
    pub const CHANGE_ATTESTATION_V1: &str = "review.kernel/ChangeAttestation@1";
    pub const FIX_VERIFICATION_V1: &str = "review.kernel/FixVerification@1";
    pub const FINDING_RESOLUTION_V1: &str = "review.kernel/FindingResolution@1";
    pub const RESOLUTION_CHALLENGE_V1: &str = "review.kernel/ResolutionChallenge@1";
    pub const POLICY_TIME_V1: &str = "review.kernel/PolicyTime@1";
    pub const GATE_DECISION_V1: &str = "review.kernel/GateDecision@1";
    pub const PATCH_PROPOSAL_V1: &str = "review.kernel/PatchProposal@1";
    pub const REPORT_SET_V1: &str = "review.kernel/ReportSet@1";
    pub const REVIEW_SLICE_V1: &str = "review.kernel/ReviewSlice@1";
    pub const REVIEWER_RESULT_V2: &str = "review.kernel/ReviewerResult@2";
    pub const SOURCE_SNAPSHOT_V1: &str = "review.kernel/SourceSnapshot@1";
    pub const SLICE_SET_V1: &str = "review.kernel/SliceSet@1";
    pub const SHARD_SET_V1: &str = "review.kernel/ShardSet@1";
    pub const SEMANTIC_CLOSURE_V1: &str = "review.kernel/SemanticClosure@1";
    pub const WORKER_NOTES_V1: &str = "review.kernel/WorkerNotes@1";
    pub const HEAD_DELTA_V1: &str = "review.kernel/HeadDelta@1";
    pub const WARM_SET_V1: &str = "review.kernel/WarmSet@1";
    /// Explicitly unsafe candidate-built output carried Gate to Worker; never a Cache Snapshot.
    pub const BUILD_CACHE_V1: &str = "review.kernel/BuildCache@1";
    /// One Attempt's captured harness transcript, resumed forked and never mutated in place.
    pub const SESSION_SNAPSHOT_V1: &str = "review.kernel/SessionSnapshot@1";
}
