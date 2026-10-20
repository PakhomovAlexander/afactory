//! The Project and its Workspaces (ADR-0151): the events a Project Store's log holds.
//!
//! A Project is the durable identity of one repository, derived from the `repository_id` every
//! Snapshot origin already carries (its sorted root commits). A Workspace is one registered
//! checkout of it. The log names a Workspace by an opaque ID and a label only: checkout, Store,
//! bindings and Task-file paths are machine-local and live in `workspaces.toml` beside the Store,
//! never in a record. The identities are derived by the `af` binary; these types fix their
//! spelling and refuse anything that could carry a host path.
//!
//! A Survey (ADR-0152) is `af/ProjectSurvey@2`, one kernel-measured description of the Project at
//! one head, kept in the Project Store's CAS; `SurveyRecorded@1` and `SurveyFailed@1` record in
//! the log that one was taken or could not be. Every number is decimal text, and neither the
//! record nor an event holds a host path, a Provider ID or an email.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::task::usage::DecimalU64;

/// Hex digits of a Workspace ID after its `w-` prefix.
pub const PROJECT_WORKSPACE_ID_HEX_LEN: usize = 16;

/// Upper bound on the `[project] name` a registration records.
pub const MAX_PROJECT_NAME_BYTES: usize = 256;

/// Upper bound on the `min_af` text a registration records.
pub const MAX_MIN_AF_BYTES: usize = 64;

/// Upper bound on a `repository_id`: 64 root commits of a SHA-256 repository.
pub const MAX_REPOSITORY_ID_BYTES: usize = 64 * 65;

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether `value` is a Project ID: `sha256:` and 64 lowercase hex digits.
pub fn is_project_id(value: &str) -> bool {
    crate::is_digest(value)
}

/// Whether `value` is a Workspace ID: `w-` and 16 lowercase hex digits.
pub fn is_project_workspace_id(value: &str) -> bool {
    value
        .strip_prefix("w-")
        .is_some_and(|hex| hex.len() == PROJECT_WORKSPACE_ID_HEX_LEN && is_lower_hex(hex))
}

/// Whether `value` is a Workspace label: the safe-name rule of Task IDs and pipeline names.
pub fn is_workspace_label(value: &str) -> bool {
    crate::task::is_name(value)
}

/// Whether `value` is a `repository_id`: sorted, distinct root commit IDs (SHA-1 or SHA-256),
/// joined by `,`.
pub fn is_repository_id(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_REPOSITORY_ID_BYTES {
        return false;
    }
    let roots: Vec<&str> = value.split(',').collect();
    roots
        .iter()
        .all(|root| matches!(root.len(), 40 | 64) && is_lower_hex(root))
        && roots.windows(2).all(|pair| pair[0] < pair[1])
}

/// How a checkout became a Workspace of its Project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOriginV1 {
    /// An existing checkout, registered lazily or by `af project workspace add`.
    Adopted,
    /// A worktree `af project workspace new` made.
    Created,
    /// The worktree `af task deliver` made, labelled by the Task ID.
    Delivered,
}

impl WorkspaceOriginV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adopted => "adopted",
            Self::Created => "created",
            Self::Delivered => "delivered",
        }
    }
}

/// Payload of `ProjectRegistered@1`: the first event of a Project Store's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRegisteredPayloadV1 {
    pub project_id: String,
    pub repository_id: String,
    /// `[project] name` of the registering checkout's `.af/af.toml`, when that file parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `[project] min_af` of the same file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_af: Option<String>,
    pub registered_unix_ms: DecimalU64,
}

impl ProjectRegisteredPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_project_id(&self.project_id) {
            return Err("ProjectRegistered@1 has an invalid Project ID".into());
        }
        if !is_repository_id(&self.repository_id) {
            return Err("ProjectRegistered@1 has an invalid repository_id".into());
        }
        if let Some(name) = &self.name
            && (name.trim().is_empty()
                || name.len() > MAX_PROJECT_NAME_BYTES
                || name.chars().any(char::is_control))
        {
            return Err("ProjectRegistered@1 has an invalid project name".into());
        }
        if let Some(min_af) = &self.min_af
            && (min_af.is_empty()
                || min_af.len() > MAX_MIN_AF_BYTES
                || !min_af
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+')))
        {
            return Err("ProjectRegistered@1 has an invalid min_af".into());
        }
        Ok(())
    }
}

/// Payload of `WorkspaceRegistered@1`. Never a path: the checkout, its Store, bindings and Task
/// files are named only in the machine-local `workspaces.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRegisteredPayloadV1 {
    pub workspace_id: String,
    pub label: String,
    pub origin: WorkspaceOriginV1,
    pub registered_unix_ms: DecimalU64,
}

impl WorkspaceRegisteredPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_project_workspace_id(&self.workspace_id) {
            return Err("WorkspaceRegistered@1 has an invalid Workspace ID".into());
        }
        if !is_workspace_label(&self.label) {
            return Err("WorkspaceRegistered@1 has an invalid label".into());
        }
        Ok(())
    }
}

/// Payload of `WorkspaceRemoved@1`: the Workspace is no longer registered. Its checkout and its
/// Store are left where they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRemovedPayloadV1 {
    pub workspace_id: String,
    pub removed_unix_ms: DecimalU64,
}

impl WorkspaceRemovedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_project_workspace_id(&self.workspace_id) {
            return Err("WorkspaceRemoved@1 has an invalid Workspace ID".into());
        }
        Ok(())
    }
}

// ------------------------------------------------------------------------------------------
// Surveys (ADR-0152)

/// The record type of a Survey: one kernel-measured description of a Project at one head.
/// Version 2 is the first that holds `stopped_record` in `history` and `contributors`.
pub const PROJECT_SURVEY_V2: &str = "af/ProjectSurvey@2";

/// The superseded record type: what an earlier build wrote before `stopped_record`. Refused by
/// type (ADR-0145): never read, never migrated.
pub const PROJECT_SURVEY_V1_SUPERSEDED: &str = "af/ProjectSurvey@1";

/// How a record of the superseded type is refused.
pub const SUPERSEDED_SURVEY: &str =
    "af/ProjectSurvey@1 is a superseded version of the Survey record; another af release wrote it";

/// The `schema` of a Survey record, deserialized only as [`PROJECT_SURVEY_V2`]: the superseded
/// `@1` label is refused by type with [`SUPERSEDED_SURVEY`], before any other field is read as a
/// field of this version.
fn survey_schema<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    use serde::de::Error as _;
    let schema = String::deserialize(deserializer)?;
    match schema.as_str() {
        PROJECT_SURVEY_V2 => Ok(schema),
        PROJECT_SURVEY_V1_SUPERSEDED => Err(D::Error::custom(SUPERSEDED_SURVEY)),
        _ => Err(D::Error::custom(format!(
            "a Survey's schema is {PROJECT_SURVEY_V2}"
        ))),
    }
}

/// Whether `bytes` are a record of the superseded type: only its `schema` label is read, never
/// a field of it, so it is recognised to be refused and is never decoded.
pub fn is_superseded_survey(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Label {
        schema: String,
    }
    serde_json::from_slice::<Label>(bytes)
        .is_ok_and(|label| label.schema == PROJECT_SURVEY_V1_SUPERSEDED)
}

/// How many release tags a Survey lists, newest first.
pub const SURVEY_RELEASE_TAGS: usize = 5;

/// Upper bound on the `hot_paths` a Survey lists.
pub const MAX_SURVEY_HOT_PATHS: usize = 1000;

/// Upper bound on one display string a Survey holds: a contributor name, a decision title, a
/// term, a tag, a check name or program. A longer one is cut at a character boundary and marked
/// `truncated`. Identity strings (every field of `identity`, commit IDs, digests, ref names and
/// paths) are recorded exactly and never bounded by it.
pub const MAX_SURVEY_TEXT_BYTES: usize = 256;

/// Upper bound on one record of a git stream the Surveyor reads (a tree entry, a numstat line, a
/// ref): a longer one stops the measurement with reason `max_record_bytes`.
pub const MAX_SURVEY_RECORD_BYTES: usize = 64 * 1024;

/// The ceilings of `[survey] max_paths`, `max_entries` and `max_survey_bytes`: a project may only
/// lower them. The Surveyor never writes a record longer than `MAX_SURVEY_BYTES`, af never reads
/// one back as a Survey, and collection refuses one as `oversize` rather than taking it for
/// corrupt (ADR-0152 clause 13).
pub const MAX_SURVEY_PATHS: u64 = 200_000;
pub const MAX_SURVEY_ENTRIES: u64 = 1_000;
pub const MAX_SURVEY_BYTES: u64 = 1_048_576;

/// The source of the checks `.af/code-policy.toml` declares; a pipeline's checks name the
/// pipeline file's basename.
pub const SURVEY_CODE_POLICY_SOURCE: &str = "code-policy";

/// Upper bound on the reason a `SurveyFailed@1` records.
pub const MAX_SURVEY_FAILURE_REASON_BYTES: usize = 512;

/// Hex digits of a contributor identity under `identity = "email_hash"`.
pub const SURVEY_EMAIL_HASH_HEX_LEN: usize = 16;

/// The prefix of a contributor named by the hash of an author name that is itself an email
/// address, or that a record cannot hold as text.
pub const SURVEY_NAME_HASH_PREFIX: &str = "h:";

/// The status of a decision record that is over `max_file_bytes` or not text.
pub const SURVEY_UNREADABLE: &str = "unreadable";

/// The statuses a decision record may name.
pub const SURVEY_DECISION_STATUSES: [&str; 5] = [
    "accepted",
    "proposed",
    "superseded",
    "deprecated",
    "rejected",
];

/// Whether `value` is a commit ID: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits.
pub fn is_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && is_lower_hex(value)
}

/// Whether `value` is a display string a Survey may hold: non-blank, at most
/// [`MAX_SURVEY_TEXT_BYTES`], no control character. `@` is permitted: repository text is
/// recorded as committed.
pub fn is_survey_text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_SURVEY_TEXT_BYTES
        && !value.chars().any(char::is_control)
}

/// Whether `value` carries an email address: any of its whitespace-separated tokens holds an
/// `@` between non-empty parts. An author name of this shape is never recorded as text: under
/// either identity policy it is replaced whole by [`SURVEY_NAME_HASH_PREFIX`] and the 16-hex hash
/// of the name.
pub fn is_email_shaped(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        token
            .char_indices()
            .any(|(index, character)| character == '@' && index > 0 && index + 1 < token.len())
    })
}

/// Whether `value` is a contributor named by hash: [`SURVEY_NAME_HASH_PREFIX`] and 16
/// lowercase hex digits.
pub fn is_name_hash(value: &str) -> bool {
    value
        .strip_prefix(SURVEY_NAME_HASH_PREFIX)
        .is_some_and(|hex| hex.len() == SURVEY_EMAIL_HASH_HEX_LEN && is_lower_hex(hex))
}

/// Whether `value` names a language: 1 to 64 of lowercase letters, digits, `+`, `#`, `.`, `_`
/// and `-`.
pub fn is_language(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'+' | b'#' | b'.' | b'_' | b'-')
        })
}

/// How far a Survey section, or the Survey, got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurveyCompletenessV1 {
    Complete,
    Partial,
    Unavailable,
}

impl SurveyCompletenessV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }

    /// The Survey's completeness from its sections': `unavailable` only when every section is,
    /// `partial` when any section is not complete, otherwise `complete`.
    pub fn fold(sections: impl IntoIterator<Item = Self>) -> Self {
        let mut all_unavailable = true;
        let mut all_complete = true;
        for section in sections {
            all_unavailable &= section == Self::Unavailable;
            all_complete &= section == Self::Complete;
        }
        if all_complete {
            Self::Complete
        } else if all_unavailable {
            Self::Unavailable
        } else {
            Self::Partial
        }
    }
}

/// Why a section is not complete: the bound it reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurveyReasonV1 {
    /// The Survey's one deadline, `[survey] max_wall_ms`, passed.
    MaxWallMs,
    /// History reached `[survey] max_commits` before the root commits.
    MaxCommits,
    /// The tree held more entries than `[survey] max_paths` after `exclude`, or the history walk
    /// met more distinct paths than it.
    MaxPaths,
    /// A list of the section held more items than `[survey] max_entries`.
    MaxEntries,
    /// A declaration the section reads is over `[survey] max_file_bytes`, not text or not UTF-8.
    MaxFileBytes,
    /// A streamed record (a tree entry, a numstat line, a ref) is longer than
    /// [`MAX_SURVEY_RECORD_BYTES`].
    MaxRecordBytes,
    /// `history` and `contributors` only: a commit's `log` record does not parse, so the walk
    /// stopped at it rather than guess its identity.
    MalformedRecord,
    /// `identity` only: the Tree Digest stopped at the fixed manifest buffer of 16 MiB.
    ManifestBytes,
    /// `identity` only: the Tree Digest stopped at the fixed 256 MiB of temporary manifest data.
    TempBytes,
}

impl SurveyReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxWallMs => "max_wall_ms",
            Self::MaxCommits => "max_commits",
            Self::MaxPaths => "max_paths",
            Self::MaxEntries => "max_entries",
            Self::MaxFileBytes => "max_file_bytes",
            Self::MaxRecordBytes => "max_record_bytes",
            Self::MalformedRecord => "malformed_record",
            Self::ManifestBytes => "manifest_bytes",
            Self::TempBytes => "temp_bytes",
        }
    }
}

/// Why a Survey's Tree Digest is absent (ADR-0152 clause 9): never the digest of a part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurveyTreeDigestReasonV1 {
    /// The Survey's deadline, `[survey] max_wall_ms`, passed before the digest was finished.
    Deadline,
    /// A record of the head's listing is longer than [`MAX_SURVEY_RECORD_BYTES`].
    MaxRecordBytes,
    /// One manifest record does not fit the fixed manifest buffer of 16 MiB.
    ManifestBytes,
    /// The manifest would take more than the fixed 256 MiB of temporary storage.
    TempBytes,
}

impl SurveyTreeDigestReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deadline => "deadline",
            Self::MaxRecordBytes => "max_record_bytes",
            Self::ManifestBytes => "manifest_bytes",
            Self::TempBytes => "temp_bytes",
        }
    }
}

/// One section of a Survey: its completeness, the reason when it is not complete, and what it
/// measured. On the wire the measurements sit beside `completeness` and `reason`; an
/// `unavailable` section holds nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurveySectionV1<T> {
    pub completeness: SurveyCompletenessV1,
    pub reason: Option<SurveyReasonV1>,
    pub body: Option<T>,
}

impl<T> SurveySectionV1<T> {
    pub fn complete(body: T) -> Self {
        Self {
            completeness: SurveyCompletenessV1::Complete,
            reason: None,
            body: Some(body),
        }
    }

    pub fn partial(reason: SurveyReasonV1, body: T) -> Self {
        Self {
            completeness: SurveyCompletenessV1::Partial,
            reason: Some(reason),
            body: Some(body),
        }
    }

    pub fn unavailable(reason: SurveyReasonV1) -> Self {
        Self {
            completeness: SurveyCompletenessV1::Unavailable,
            reason: Some(reason),
            body: None,
        }
    }

    fn validate_with(
        &self,
        name: &str,
        body: impl FnOnce(&T) -> Result<(), String>,
    ) -> Result<(), String> {
        let shaped = match self.completeness {
            SurveyCompletenessV1::Complete => self.reason.is_none() && self.body.is_some(),
            SurveyCompletenessV1::Partial => self.reason.is_some() && self.body.is_some(),
            SurveyCompletenessV1::Unavailable => self.reason.is_some() && self.body.is_none(),
        };
        if !shaped {
            return Err(format!(
                "the Survey section `{name}` is {} but {}",
                self.completeness.as_str(),
                match self.completeness {
                    SurveyCompletenessV1::Complete => "carries a reason or no measurement",
                    SurveyCompletenessV1::Partial => "lacks its reason or its measurement",
                    SurveyCompletenessV1::Unavailable => "lacks its reason or holds a measurement",
                }
            ));
        }
        match &self.body {
            Some(measured) => body(measured).map_err(|error| format!("{name}: {error}")),
            None => Ok(()),
        }
    }
}

impl<T: Serialize> Serialize for SurveySectionV1<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let mut fields = match &self.body {
            Some(body) => match serde_json::to_value(body).map_err(S::Error::custom)? {
                serde_json::Value::Object(fields) => fields,
                _ => return Err(S::Error::custom("a Survey section measures into an object")),
            },
            None => serde_json::Map::new(),
        };
        if fields.contains_key("completeness") || fields.contains_key("reason") {
            return Err(S::Error::custom(
                "a Survey section's measurement cannot name completeness or reason",
            ));
        }
        fields.insert(
            "completeness".into(),
            serde_json::Value::String(self.completeness.as_str().into()),
        );
        if let Some(reason) = self.reason {
            fields.insert(
                "reason".into(),
                serde_json::Value::String(reason.as_str().into()),
            );
        }
        serde_json::Value::Object(fields).serialize(serializer)
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for SurveySectionV1<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut fields = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let completeness: SurveyCompletenessV1 = serde_json::from_value(
            fields
                .remove("completeness")
                .ok_or_else(|| D::Error::missing_field("completeness"))?,
        )
        .map_err(D::Error::custom)?;
        let reason = fields
            .remove("reason")
            .map(serde_json::from_value::<SurveyReasonV1>)
            .transpose()
            .map_err(D::Error::custom)?;
        let body = if completeness == SurveyCompletenessV1::Unavailable {
            if let Some(field) = fields.keys().next() {
                return Err(D::Error::custom(format!(
                    "an unavailable Survey section holds only its reason, not `{field}`"
                )));
            }
            None
        } else {
            Some(
                serde_json::from_value::<T>(serde_json::Value::Object(fields))
                    .map_err(D::Error::custom)?,
            )
        };
        Ok(Self {
            completeness,
            reason,
            body,
        })
    }
}

/// The af release `.af/af.lock` pins at the head: its version and archive digests by target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyAfPinV1 {
    pub version: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub digests: BTreeMap<String, String>,
}

/// `identity`: which Project, which head, under which policy and rules the Survey measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyIdentityV1 {
    pub project_id: String,
    pub repository_id: String,
    /// `[project] name` of the head's `.af/af.toml`, when it declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_af: Option<String>,
    /// The af pin of the head's `.af/af.lock`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub af_pin: Option<SurveyAfPinV1>,
    pub head: String,
    /// `sha256:` + hex(SHA-256 of the exact stdout of the one ref observation, `git for-each-ref
    /// --format=%(refname)%09%(objectname)%09%(objecttype)%09%(creatordate:unix) refs/heads
    /// refs/tags`): the refs of the survey key, and of `history`'s tags and branches.
    pub refs_digest: String,
    /// The Tree Digest of the whole head tree: the committed capture's content digest, computed
    /// by the digest-only path; absent, with `tree_digest_reason`, when it did not finish, never
    /// the digest of a part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_digest_reason: Option<SurveyTreeDigestReasonV1>,
    pub cutoff_unix_s: DecimalU64,
    pub policy_digest: String,
    pub surveyor_version: DecimalU64,
    /// The exact `git --version` stdout, trimmed: git decides the numbers.
    pub git_version: String,
}

/// For each list a section cut at `[survey] max_entries` (or `tree`'s entries at `max_paths`,
/// under `paths`), how many items it saw. Empty, and absent on the wire, when nothing was cut.
pub type SurveySeenV1 = BTreeMap<String, DecimalU64>;

fn is_false(value: &bool) -> bool {
    !*value
}

/// `truncated` is written only when true: a record has one spelling.
fn only_true<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    if bool::deserialize(deserializer)? {
        Ok(true)
    } else {
        Err(serde::de::Error::custom(
            "`truncated` is present only when a string was cut",
        ))
    }
}

/// Files, lines and bytes of one top-level directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyDirectoryV1 {
    pub files: DecimalU64,
    pub lines: DecimalU64,
    pub bytes: DecimalU64,
}

/// Files and lines of one language.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyLanguageV1 {
    pub files: DecimalU64,
    pub lines: DecimalU64,
}

/// The `tests` split: files and lines the `[survey] tests` globs match, and the others.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyTestsV1 {
    pub files: DecimalU64,
    pub lines: DecimalU64,
    pub other_files: DecimalU64,
    pub other_lines: DecimalU64,
}

/// `tree`: the head tree, after `[survey] exclude`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyTreeV1 {
    pub files: DecimalU64,
    pub bytes: DecimalU64,
    pub lines: DecimalU64,
    /// Files without a line count: binary, over `max_file_bytes`, a symlink, a submodule, or
    /// not read before the deadline.
    pub files_without_lines: DecimalU64,
    /// By top-level directory; root-level files under `.`.
    pub directories: BTreeMap<String, SurveyDirectoryV1>,
    pub languages: BTreeMap<String, SurveyLanguageV1>,
    pub tests: SurveyTestsV1,
    /// `paths`, `directories` and `languages`, when cut.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: SurveySeenV1,
}

/// Commits and merges in one window before the cutoff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyWindowV1 {
    pub commits: DecimalU64,
    pub merges: DecimalU64,
}

/// The 30, 90 and 365-day windows: commits whose committer time is after the cutoff minus the
/// window and at or before the cutoff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyWindowsV1 {
    #[serde(rename = "30d")]
    pub days_30: SurveyWindowV1,
    #[serde(rename = "90d")]
    pub days_90: SurveyWindowV1,
    #[serde(rename = "365d")]
    pub days_365: SurveyWindowV1,
}

/// Churn of one top-level directory over 90 days.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyChurnV1 {
    pub commits: DecimalU64,
    pub added: DecimalU64,
    pub removed: DecimalU64,
}

/// One of the paths with the most commits in 90 days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyHotPathV1 {
    pub path: String,
    pub commits: DecimalU64,
    /// Present, and true, when the head tree no longer holds the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<bool>,
}

/// One display string of a list, and whether it was cut to [`MAX_SURVEY_TEXT_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyNameV1 {
    pub name: String,
    #[serde(
        default,
        skip_serializing_if = "is_false",
        deserialize_with = "only_true"
    )]
    pub truncated: bool,
}

/// Tags: how many, and the newest names matching `release_tags`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyTagsV1 {
    pub count: DecimalU64,
    /// Newest first, by creator date.
    pub release: Vec<SurveyNameV1>,
}

/// `history`: the commits reachable from the head, newest first up to `max_commits`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyHistoryV1 {
    /// Every commit reachable from the head, reached or not.
    pub reachable_commits: DecimalU64,
    /// The commits reached, and the merges among them.
    pub commits: DecimalU64,
    pub merges: DecimalU64,
    pub windows: SurveyWindowsV1,
    pub first_commit_unix_s: DecimalU64,
    pub last_commit_unix_s: DecimalU64,
    /// The oldest commit reached, when the walk did not reach every reachable commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_reached: Option<String>,
    /// The commit whose record stopped the walk unfolded: one over `max_paths`, a record over
    /// [`MAX_SURVEY_RECORD_BYTES`] or a malformed record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_in: Option<String>,
    /// The ordinal in the log of the record `stopped_in` names, `1` for the first, the head's:
    /// the record after the commits counted. Present exactly with `stopped_in`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_record: Option<DecimalU64>,
    pub churn_90d: BTreeMap<String, SurveyChurnV1>,
    /// Most commits first, ties by path.
    pub hot_paths: Vec<SurveyHotPathV1>,
    /// Absent when the deadline passed before the tags were read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<SurveyTagsV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_branches: Option<DecimalU64>,
    /// `churn_90d`, `hot_paths` and `tags`, when cut.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: SurveySeenV1,
}

/// How a Survey names a contributor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurveyIdentityPolicyV1 {
    /// The author name; an email-shaped one by `h:` and the 16-hex hash of the name.
    #[default]
    Name,
    /// The first 16 hex digits of SHA-256 of the lowercased, trimmed author email.
    EmailHash,
}

impl SurveyIdentityPolicyV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::EmailHash => "email_hash",
        }
    }
}

/// One contributor identity over the commits reached, grouped and ranked by `key`, never by its
/// display text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyContributorV1 {
    /// Under `identity = "name"` the first 16 hex digits of SHA-256 of the exact author-name
    /// bytes; under `email_hash` the 16-hex email hash; under either, for an author name that
    /// carries an email address (or is blank or holds a control character),
    /// [`SURVEY_NAME_HASH_PREFIX`] and the 16-hex hash of the lowercased, trimmed name.
    pub key: String,
    /// Under `identity = "name"` only: the author name cut to [`MAX_SURVEY_TEXT_BYTES`], or the
    /// key for a name that carries an email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub commits: DecimalU64,
    pub added: DecimalU64,
    pub removed: DecimalU64,
    pub first_unix_s: DecimalU64,
    pub last_unix_s: DecimalU64,
    pub active_90d: bool,
    /// The name was cut to [`MAX_SURVEY_TEXT_BYTES`].
    #[serde(
        default,
        skip_serializing_if = "is_false",
        deserialize_with = "only_true"
    )]
    pub truncated: bool,
}

/// Whether `value` is a contributor key: 16 lowercase hex digits, or a name hash.
pub fn is_contributor_key(value: &str) -> bool {
    is_name_hash(value) || (value.len() == SURVEY_EMAIL_HASH_HEX_LEN && is_lower_hex(value))
}

/// `contributors`: who authored the commits reached, and how many were agent-assisted. Each
/// commit counts once in the human and agent-assisted partition and once for its author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyContributorsV1 {
    pub identity: SurveyIdentityPolicyV1,
    pub identities: DecimalU64,
    /// Identities with a commit in the last 90 days.
    pub active_90d: DecimalU64,
    pub human_commits: DecimalU64,
    pub agent_commits: DecimalU64,
    /// Shares of the commits reached, in basis points, rounded down.
    pub human_share_bp: DecimalU64,
    pub agent_share_bp: DecimalU64,
    /// The share of the identity with the most commits, in basis points, rounded down.
    pub top_share_bp: DecimalU64,
    /// The identities ranked by commits, then lines added, both descending, then key, at most
    /// `max_entries`.
    pub by_identity: Vec<SurveyContributorV1>,
    /// The ordinal in the log of the record that stopped the walk this section folds, as
    /// `history.stopped_record` names it: present exactly when a record stopped the walk
    /// (`malformed_record`, `max_record_bytes` or `max_paths`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_record: Option<DecimalU64>,
    /// `by_identity`, when cut.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: SurveySeenV1,
}

/// One decision record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyDecisionV1 {
    /// The four leading digits of the file name, as written.
    pub number: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The title was cut to [`MAX_SURVEY_TEXT_BYTES`].
    #[serde(
        default,
        skip_serializing_if = "is_false",
        deserialize_with = "only_true"
    )]
    pub truncated: bool,
    /// The first closed status on the status line, or `unreadable` for a file over
    /// `max_file_bytes`, not text or not UTF-8; absent (counted `unknown`) without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// The status line says `amended`.
    #[serde(
        default,
        skip_serializing_if = "is_false",
        deserialize_with = "only_true"
    )]
    pub amended: bool,
}

/// `decisions`: the files the `decisions` pattern matches whose name starts with four ASCII
/// digits and a dash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyDecisionsV1 {
    pub count: DecimalU64,
    /// By status; `unknown` for a record that names none, `unreadable` for one not read.
    pub by_status: BTreeMap<String, DecimalU64>,
    /// Every record up to `max_entries`, by number, then path.
    pub records: Vec<SurveyDecisionV1>,
    /// `records`, when cut.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: SurveySeenV1,
}

/// `vocabulary`: the term names of the `vocabulary` file, never a definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyVocabularyV1 {
    pub file: String,
    pub present: bool,
    /// The terms defined, and the first `max_entries` of them by name, each once. Both absent
    /// when the file is over `max_file_bytes`, not text or not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<DecimalU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<Vec<SurveyNameV1>>,
    /// `terms`, when cut.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: SurveySeenV1,
}

/// One declared check: where it is declared (`code-policy` or the pipeline file's basename),
/// its name and its program, never its arguments. Its identity is `<source>:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyCheckV1 {
    pub source: String,
    pub name: String,
    pub program: String,
    /// The name or the program was cut to [`MAX_SURVEY_TEXT_BYTES`].
    #[serde(
        default,
        skip_serializing_if = "is_false",
        deserialize_with = "only_true"
    )]
    pub truncated: bool,
}

impl SurveyCheckV1 {
    /// `<source>:<name>`: equal names in two sources are two checks.
    pub fn identity(&self) -> String {
        format!("{}:{}", self.source, self.name)
    }
}

/// `checks`: declared checks, toolchain declarations and CI workflow files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyChecksV1 {
    /// By identity, `<source>:<name>`, then program; one per identity.
    pub declared: Vec<SurveyCheckV1>,
    /// The toolchain declarations present at the root, sorted.
    pub toolchain: Vec<String>,
    pub ci_workflows: DecimalU64,
    /// Declarations over `max_file_bytes`, not text or not UTF-8, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
    /// `declared`, `toolchain` and `unreadable`, when cut.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: SurveySeenV1,
}

/// Where a Survey that is not complete stopped (ADR-0152 clause 1), so a replay of its record
/// reproduces it: the section the deadline cut first, or, when no deadline cut one, the first
/// section that is not complete; that section's reason; how many of its items the Survey's
/// clock charged; and the commit or the path the stop names, when it names one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyStopV1 {
    /// One of the section names, in record order.
    pub section: String,
    pub reason: SurveyReasonV1,
    /// The items of the section the Survey's clock charged before the section was recorded: the
    /// one that found the deadline and the section's last check included, `0` for a section the
    /// deadline left unstarted.
    pub item: DecimalU64,
    /// `history` only: the commit whose record stopped the walk (`stopped_in`), else the oldest
    /// commit reached when the walk did not reach every commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Under `max_file_bytes`: the first declaration of the section, in its order, that was not
    /// read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `af/ProjectSurvey@2`: one immutable, kernel-measured description of a Project at one head.
/// Canonical JSON; every number a decimal string; no host path, Provider ID or email. The
/// superseded `af/ProjectSurvey@1` is refused by type ([`SUPERSEDED_SURVEY`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSurveyV2 {
    #[serde(deserialize_with = "survey_schema")]
    pub schema: String,
    pub completeness: SurveyCompletenessV1,
    /// The Survey before this one in the same Project Store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_survey_id: Option<String>,
    /// Where the Survey stopped: present exactly when it is not complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<SurveyStopV1>,
    pub identity: SurveySectionV1<SurveyIdentityV1>,
    pub tree: SurveySectionV1<SurveyTreeV1>,
    pub history: SurveySectionV1<SurveyHistoryV1>,
    pub contributors: SurveySectionV1<SurveyContributorsV1>,
    pub decisions: SurveySectionV1<SurveyDecisionsV1>,
    pub vocabulary: SurveySectionV1<SurveyVocabularyV1>,
    pub checks: SurveySectionV1<SurveyChecksV1>,
}

/// Whether `value` is a calendar date `YYYY-MM-DD`: shaped so, and a day its month has in its
/// year (`2024-02-29` is one, `2025-02-29` and `2026-02-31` are not).
pub fn is_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    let shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        });
    if !shaped {
        return false;
    }
    let number = |range: std::ops::Range<usize>| value[range].parse::<u32>().unwrap_or(0);
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

fn is_top_level(name: &str) -> bool {
    name == "." || (crate::path::is_valid_repo_path(name) && !name.contains('/'))
}

impl ProjectSurveyV2 {
    /// The completeness of each section, in record order.
    pub fn sections(&self) -> [(&'static str, SurveyCompletenessV1, Option<SurveyReasonV1>); 7] {
        [
            ("identity", self.identity.completeness, self.identity.reason),
            ("tree", self.tree.completeness, self.tree.reason),
            ("history", self.history.completeness, self.history.reason),
            (
                "contributors",
                self.contributors.completeness,
                self.contributors.reason,
            ),
            (
                "decisions",
                self.decisions.completeness,
                self.decisions.reason,
            ),
            (
                "vocabulary",
                self.vocabulary.completeness,
                self.vocabulary.reason,
            ),
            ("checks", self.checks.completeness, self.checks.reason),
        ]
    }

    /// The stopping point the sections determine, without its `item`, which only the clock
    /// knew: the section the deadline cut first, else the first section that is not complete,
    /// with its reason, `history`'s commit when its measurement names one, and under
    /// `max_file_bytes` the first declaration of the section that was not read. `None` for a
    /// complete Survey.
    pub fn stopping_point(&self) -> Option<SurveyStopV1> {
        let sections = self.sections();
        let (section, _, reason) = sections
            .iter()
            .find(|(_, _, reason)| *reason == Some(SurveyReasonV1::MaxWallMs))
            .or_else(|| {
                sections
                    .iter()
                    .find(|(_, completeness, _)| *completeness != SurveyCompletenessV1::Complete)
            })?;
        let reason = (*reason)?;
        let commit = match *section {
            "history" => self.history.body.as_ref().and_then(|history| {
                history
                    .stopped_in
                    .clone()
                    .or_else(|| history.oldest_reached.clone())
            }),
            _ => None,
        };
        let path = match (*section, reason) {
            ("decisions", SurveyReasonV1::MaxFileBytes) => {
                self.decisions.body.as_ref().and_then(|decisions| {
                    decisions
                        .records
                        .iter()
                        .find(|record| record.status.as_deref() == Some(SURVEY_UNREADABLE))
                        .map(|record| record.path.clone())
                })
            }
            ("vocabulary", SurveyReasonV1::MaxFileBytes) => self
                .vocabulary
                .body
                .as_ref()
                .map(|vocabulary| vocabulary.file.clone()),
            ("checks", SurveyReasonV1::MaxFileBytes) => self
                .checks
                .body
                .as_ref()
                .and_then(|checks| checks.unreadable.first().cloned()),
            _ => None,
        };
        Some(SurveyStopV1 {
            section: (*section).to_string(),
            reason,
            item: 0.into(),
            commit,
            path,
        })
    }

    /// Whether `stopped` is the stopping point the sections determine: present exactly when the
    /// Survey is not complete, on the section, reason and path they name, and on the commit
    /// `history` names: an unavailable `history` names none.
    fn validate_stop(&self) -> Result<(), String> {
        let (stopped, expected) = match (&self.stopped, self.stopping_point()) {
            (None, None) => return Ok(()),
            (Some(stopped), Some(expected)) => (stopped, expected),
            (Some(_), None) => return Err("a complete Survey names no stopping point".into()),
            (None, Some(_)) => {
                return Err("a Survey that is not complete names where it stopped".into());
            }
        };
        if stopped.section != expected.section || stopped.reason != expected.reason {
            return Err(format!(
                "the Survey stopped in `{}` with {}, not in `{}` with {}",
                expected.section,
                expected.reason.as_str(),
                stopped.section,
                stopped.reason.as_str()
            ));
        }
        if stopped.path != expected.path {
            return Err("the stopping point's path is the first declaration not read".into());
        }
        if stopped.commit != expected.commit {
            return Err("the stopping point's commit is the one `history` stopped in".into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PROJECT_SURVEY_V2 {
            return Err(format!("a Survey's schema is {PROJECT_SURVEY_V2}"));
        }
        if self
            .previous_survey_id
            .as_deref()
            .is_some_and(|id| !crate::is_digest(id))
        {
            return Err("a Survey's previous_survey_id is a digest".into());
        }
        let folded = SurveyCompletenessV1::fold(self.sections().map(|(_, section, _)| section));
        if folded != self.completeness {
            return Err(format!(
                "a Survey whose sections fold to {} cannot be {}",
                folded.as_str(),
                self.completeness.as_str()
            ));
        }
        // The Tree Digest's ceilings stop identity alone.
        if let Some((name, _, Some(reason))) =
            self.sections().into_iter().skip(1).find(|(_, _, reason)| {
                matches!(
                    reason,
                    Some(SurveyReasonV1::ManifestBytes | SurveyReasonV1::TempBytes)
                )
            })
        {
            return Err(format!(
                "the Survey section `{name}` cannot stop at {}: a Tree Digest ceiling",
                reason.as_str()
            ));
        }
        // A malformed log record stops the walk, and only the sections it folds.
        if let Some((name, _, _)) = self.sections().into_iter().find(|(name, _, reason)| {
            *reason == Some(SurveyReasonV1::MalformedRecord)
                && !matches!(*name, "history" | "contributors")
        }) {
            return Err(format!(
                "the Survey section `{name}` cannot stop at malformed_record: a log record"
            ));
        }
        // ... and names the commit it stopped in, the first included: a section it stopped is
        // partial, since an unavailable one holds nothing but its completeness and reason.
        if let Some((name, _, _)) = self
            .sections()
            .into_iter()
            .find(|(_, completeness, reason)| {
                *reason == Some(SurveyReasonV1::MalformedRecord)
                    && *completeness != SurveyCompletenessV1::Partial
            })
        {
            return Err(format!(
                "the Survey section `{name}` stopped at malformed_record is partial, naming the \
                 commit"
            ));
        }
        if self.history.reason == Some(SurveyReasonV1::MalformedRecord)
            && self
                .history
                .body
                .as_ref()
                .is_none_or(|history| history.stopped_in.is_none())
        {
            return Err(
                "a history stopped at malformed_record names the commit in stopped_in".into(),
            );
        }
        // `contributors` folds the walk `history` measures: a record that stopped it is the
        // same record in both, by its ordinal.
        if let Some(contributors) = &self.contributors.body {
            let walked = self
                .history
                .body
                .as_ref()
                .and_then(|history| history.stopped_record);
            if contributors.stopped_record != walked {
                return Err(
                    "contributors names the record that stopped the walk as history does".into(),
                );
            }
            if matches!(
                self.contributors.reason,
                Some(
                    SurveyReasonV1::MalformedRecord
                        | SurveyReasonV1::MaxRecordBytes
                        | SurveyReasonV1::MaxPaths
                )
            ) && contributors.stopped_record.is_none()
            {
                return Err(
                    "contributors stopped by a log record names that record's ordinal".into(),
                );
            }
        }
        self.validate_stop()?;
        self.identity.validate_with("identity", validate_identity)?;
        self.tree.validate_with("tree", validate_tree)?;
        self.history.validate_with("history", validate_history)?;
        self.contributors
            .validate_with("contributors", validate_contributors)?;
        self.decisions
            .validate_with("decisions", validate_decisions)?;
        self.vocabulary
            .validate_with("vocabulary", validate_vocabulary)?;
        self.checks.validate_with("checks", validate_checks)?;
        // A section that cut a list, or measured less than it should, is never complete.
        let cut = |seen: Option<&SurveySeenV1>| seen.is_some_and(|seen| !seen.is_empty());
        for (name, completeness, short) in [
            (
                "identity",
                self.identity.completeness,
                self.identity
                    .body
                    .as_ref()
                    .is_some_and(|identity| identity.tree_digest.is_none()),
            ),
            (
                "tree",
                self.tree.completeness,
                cut(self.tree.body.as_ref().map(|tree| &tree.seen)),
            ),
            (
                "history",
                self.history.completeness,
                cut(self.history.body.as_ref().map(|history| &history.seen)),
            ),
            (
                "contributors",
                self.contributors.completeness,
                cut(self.contributors.body.as_ref().map(|body| &body.seen)),
            ),
            (
                "decisions",
                self.decisions.completeness,
                cut(self.decisions.body.as_ref().map(|body| &body.seen)),
            ),
            (
                "vocabulary",
                self.vocabulary.completeness,
                self.vocabulary
                    .body
                    .as_ref()
                    .is_some_and(|body| !body.seen.is_empty() || body.terms.is_none()),
            ),
            (
                "checks",
                self.checks.completeness,
                self.checks
                    .body
                    .as_ref()
                    .is_some_and(|body| !body.seen.is_empty() || !body.unreadable.is_empty()),
            ),
        ] {
            if short && completeness == SurveyCompletenessV1::Complete {
                return Err(format!(
                    "the Survey section `{name}` is complete but cut a list or left a value \
                     unmeasured"
                ));
            }
        }
        Ok(())
    }
}

/// Whether `seen` names only lists of the section, each with more items than the list holds.
fn validate_seen(seen: &SurveySeenV1, lists: &[(&str, usize)]) -> Result<(), String> {
    for (name, count) in seen {
        let Some((_, held)) = lists.iter().find(|(list, _)| list == name) else {
            return Err(format!("`seen` names `{name}`, which is not a list here"));
        };
        if count.get() <= *held as u64 {
            return Err(format!(
                "`seen` of `{name}` is not more than the list holds"
            ));
        }
    }
    Ok(())
}

fn validate_identity(identity: &SurveyIdentityV1) -> Result<(), String> {
    if !is_project_id(&identity.project_id) {
        return Err("an invalid Project ID".into());
    }
    if !is_repository_id(&identity.repository_id) {
        return Err("an invalid repository_id".into());
    }
    if !is_commit_id(&identity.head) {
        return Err("the head is a commit ID".into());
    }
    if identity
        .tree_digest
        .as_deref()
        .is_some_and(|digest| !crate::is_digest(digest))
        || !crate::is_digest(&identity.policy_digest)
        || !crate::is_digest(&identity.refs_digest)
    {
        return Err("the Tree Digest, the refs digest and the policy digest are digests".into());
    }
    if identity.tree_digest.is_some() == identity.tree_digest_reason.is_some() {
        return Err("the Tree Digest is present, or absent with its reason".into());
    }
    if identity.surveyor_version.get() == 0 {
        return Err("the Surveyor version starts at 1".into());
    }
    if identity.git_version.trim().is_empty()
        || identity.git_version != identity.git_version.trim()
        || identity.git_version.chars().any(char::is_control)
    {
        return Err("the git version is git's trimmed one line".into());
    }
    // The declaration rules of ProjectRegistered@1.
    ProjectRegisteredPayloadV1 {
        project_id: identity.project_id.clone(),
        repository_id: identity.repository_id.clone(),
        name: identity.name.clone(),
        min_af: identity.min_af.clone(),
        registered_unix_ms: 0.into(),
    }
    .validate()
    .map_err(|_| "an invalid [project] name or min_af".to_string())?;
    if let Some(pin) = &identity.af_pin {
        let safe = |value: &str, extra: &[u8]| {
            !value.is_empty()
                && value.len() <= MAX_MIN_AF_BYTES
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || extra.contains(&byte))
        };
        if !safe(&pin.version, b".-+")
            || !pin
                .digests
                .iter()
                .all(|(target, digest)| safe(target, b"-_.") && crate::is_digest(digest))
        {
            return Err("the af pin is a version and target digests".into());
        }
    }
    Ok(())
}

fn validate_tree(tree: &SurveyTreeV1) -> Result<(), String> {
    validate_seen(
        &tree.seen,
        &[
            ("paths", tree.files.get() as usize),
            ("directories", tree.directories.len()),
            ("languages", tree.languages.len()),
        ],
    )?;
    let sum = |values: &mut dyn Iterator<Item = u64>| values.sum::<u64>();
    let files = sum(&mut tree.directories.values().map(|entry| entry.files.get()));
    let lines = sum(&mut tree.directories.values().map(|entry| entry.lines.get()));
    let bytes = sum(&mut tree.directories.values().map(|entry| entry.bytes.get()));
    let language_files = sum(&mut tree.languages.values().map(|entry| entry.files.get()));
    let language_lines = sum(&mut tree.languages.values().map(|entry| entry.lines.get()));
    // A cut list adds up to at most the totals; a whole one to exactly them.
    let adds = |list: &str, parts: &[(u64, u64)]| {
        parts.iter().all(|(part, total)| {
            if tree.seen.contains_key(list) {
                part <= total
            } else {
                part == total
            }
        })
    };
    if !adds(
        "directories",
        &[
            (files, tree.files.get()),
            (lines, tree.lines.get()),
            (bytes, tree.bytes.get()),
        ],
    ) || !adds(
        "languages",
        &[
            (language_files, tree.files.get()),
            (language_lines, tree.lines.get()),
        ],
    ) || tree.tests.files.get() + tree.tests.other_files.get() != tree.files.get()
        || tree.tests.lines.get() + tree.tests.other_lines.get() != tree.lines.get()
        || tree.files_without_lines.get() > tree.files.get()
    {
        return Err("the directories, languages and the tests split add up to the totals".into());
    }
    if !tree.directories.keys().all(|name| is_top_level(name)) {
        return Err("a directory is `.` or one top-level name".into());
    }
    if !tree.languages.keys().all(|name| is_language(name)) {
        return Err("a language is a short lowercase name".into());
    }
    Ok(())
}

fn validate_history(history: &SurveyHistoryV1) -> Result<(), String> {
    let windows = &history.windows;
    let ordered = |window: &SurveyWindowV1| window.merges.get() <= window.commits.get();
    // A walk that folded no commit stopped in the head's record, and names it.
    if history.commits.get() == 0 && history.stopped_in.is_none() {
        return Err("a history that reached no commit names the commit it stopped in".into());
    }
    // The record that stopped the walk is the one after every commit counted.
    match (&history.stopped_in, history.stopped_record) {
        (None, None) => {}
        (Some(_), Some(record)) if record.get() == history.commits.get().saturating_add(1) => {}
        _ => {
            return Err(
                "a history that names the commit it stopped in names its record, the one after \
                 the commits counted"
                    .into(),
            );
        }
    }
    if history.commits.get() > history.reachable_commits.get()
        || history.merges.get() > history.commits.get()
        || windows.days_30.commits.get() > windows.days_90.commits.get()
        || windows.days_90.commits.get() > windows.days_365.commits.get()
        || windows.days_365.commits.get() > history.commits.get()
        || ![&windows.days_30, &windows.days_90, &windows.days_365]
            .into_iter()
            .all(ordered)
        || history.first_commit_unix_s.get() > history.last_commit_unix_s.get()
    {
        return Err("the counts and windows are ordered".into());
    }
    if [&history.oldest_reached, &history.stopped_in]
        .into_iter()
        .flatten()
        .any(|commit| !is_commit_id(commit))
    {
        return Err("the oldest commit reached and the one stopped in are commit IDs".into());
    }
    if history.hot_paths.len() > MAX_SURVEY_HOT_PATHS
        || !history.hot_paths.iter().all(|hot| {
            crate::path::is_valid_repo_path(&hot.path)
                && hot.removed != Some(false)
                && hot.commits.get() > 0
        })
        || !history.hot_paths.windows(2).all(|pair| {
            let (a, b) = (&pair[0], &pair[1]);
            a.commits.get() > b.commits.get() || (a.commits == b.commits && a.path < b.path)
        })
    {
        return Err("hot paths are repository paths, most commits first, ties by path".into());
    }
    if !history.churn_90d.keys().all(|name| is_top_level(name)) {
        return Err("churn is by top-level directory".into());
    }
    if let Some(tags) = &history.tags
        && (tags.release.len() > SURVEY_RELEASE_TAGS
            || tags.release.len() as u64 > tags.count.get()
            || !tags.release.iter().all(|tag| is_survey_text(&tag.name)))
    {
        return Err("at most five release tags, each one of the tags".into());
    }
    validate_seen(
        &history.seen,
        &[
            ("churn_90d", history.churn_90d.len()),
            ("hot_paths", history.hot_paths.len()),
            (
                "tags",
                history.tags.as_ref().map_or(0, |tags| tags.release.len()),
            ),
        ],
    )
}

fn validate_contributors(contributors: &SurveyContributorsV1) -> Result<(), String> {
    validate_seen(
        &contributors.seen,
        &[("by_identity", contributors.by_identity.len())],
    )?;
    let cut = contributors.seen.contains_key("by_identity");
    let commits: u64 = contributors
        .by_identity
        .iter()
        .map(|entry| entry.commits.get())
        .sum();
    let active = contributors
        .by_identity
        .iter()
        .filter(|entry| entry.active_90d)
        .count() as u64;
    let total = contributors.human_commits.get() + contributors.agent_commits.get();
    let listed = contributors.by_identity.len() as u64;
    let whole = contributors.identities.get() == listed
        && contributors.active_90d.get() == active
        && total == commits;
    let partly = contributors.identities.get() > listed
        && contributors.active_90d.get() >= active
        && total >= commits;
    let adds_up = if cut { partly } else { whole };
    if !adds_up {
        return Err("the counts add up over the identities".into());
    }
    if [
        contributors.human_share_bp,
        contributors.agent_share_bp,
        contributors.top_share_bp,
    ]
    .into_iter()
    .any(|share| share.get() > 10_000)
    {
        return Err("a share is at most 10000 basis points".into());
    }
    // Ranked by commits, then lines added, both descending, then key: one entry per key.
    if !contributors.by_identity.windows(2).all(|pair| {
        let rank = |entry: &SurveyContributorV1| {
            (
                std::cmp::Reverse(entry.commits.get()),
                std::cmp::Reverse(entry.added.get()),
                entry.key.clone(),
            )
        };
        rank(&pair[0]) < rank(&pair[1])
    }) {
        return Err(
            "identities are ranked by commits, then lines added, then key, each key once".into(),
        );
    }
    for entry in &contributors.by_identity {
        let named = is_contributor_key(&entry.key)
            && match (contributors.identity, entry.name.as_deref()) {
                (SurveyIdentityPolicyV1::Name, Some(name)) if is_name_hash(&entry.key) => {
                    name == entry.key && !entry.truncated
                }
                (SurveyIdentityPolicyV1::Name, Some(name)) => {
                    is_survey_text(name) && name == name.trim() && !is_email_shaped(name)
                }
                (SurveyIdentityPolicyV1::EmailHash, None) => !entry.truncated,
                _ => false,
            };
        if !named {
            return Err(format!(
                "an identity under `{}` is {}, never an email",
                contributors.identity.as_str(),
                match contributors.identity {
                    SurveyIdentityPolicyV1::Name =>
                        "a 16-hex key with a name that carries no email address, or `h:` and 16 \
                         hex digits as both",
                    SurveyIdentityPolicyV1::EmailHash =>
                        "a key of 16 lowercase hex digits, or `h:` and 16 hex digits, without a name",
                }
            ));
        }
        if entry.commits.get() == 0 || entry.first_unix_s.get() > entry.last_unix_s.get() {
            return Err("an identity has commits, first before last".into());
        }
    }
    Ok(())
}

fn validate_decisions(decisions: &SurveyDecisionsV1) -> Result<(), String> {
    validate_seen(&decisions.seen, &[("records", decisions.records.len())])?;
    let counted: u64 = decisions.by_status.values().map(|count| count.get()).sum();
    if counted != decisions.count.get()
        || !decisions.by_status.keys().all(|status| {
            status == "unknown"
                || status == SURVEY_UNREADABLE
                || SURVEY_DECISION_STATUSES.contains(&status.as_str())
        })
    {
        return Err(
            "counts by status add up, under the closed statuses, `unknown` and `unreadable`".into(),
        );
    }
    let listed = decisions.records.len() as u64;
    let expected = decisions
        .seen
        .get("records")
        .map_or(listed, |seen| seen.get());
    if expected != decisions.count.get() {
        return Err("every record is listed, or the list is cut and says how many".into());
    }
    if !decisions
        .records
        .windows(2)
        .all(|pair| (&pair[0].number, &pair[0].path) < (&pair[1].number, &pair[1].path))
    {
        return Err("records are sorted by number, then path".into());
    }
    for decision in &decisions.records {
        let unreadable = decision.status.as_deref() == Some(SURVEY_UNREADABLE);
        if decision.number.len() != 4
            || !decision.number.bytes().all(|byte| byte.is_ascii_digit())
            || !crate::path::is_valid_repo_path(&decision.path)
            || decision
                .title
                .as_deref()
                .is_some_and(|title| !is_survey_text(title))
            || (decision.truncated && decision.title.is_none())
            || decision.status.as_deref().is_some_and(|status| {
                status != SURVEY_UNREADABLE && !SURVEY_DECISION_STATUSES.contains(&status)
            })
            || (unreadable
                && (decision.title.is_some() || decision.date.is_some() || decision.amended))
            || decision
                .date
                .as_deref()
                .is_some_and(|date| !is_calendar_date(date))
        {
            return Err(
                "a decision has a four-digit number, a path, and a title, status and date of \
                 their own shape; an unreadable one has no title"
                    .into(),
            );
        }
    }
    Ok(())
}

fn validate_vocabulary(vocabulary: &SurveyVocabularyV1) -> Result<(), String> {
    if !crate::path::is_valid_repo_path(&vocabulary.file) {
        return Err("the vocabulary file is a repository path".into());
    }
    match (&vocabulary.count, &vocabulary.terms) {
        (None, None) if vocabulary.present => {
            if !vocabulary.seen.is_empty() {
                return Err("an unread vocabulary file cut no list".into());
            }
        }
        (Some(count), Some(terms)) => {
            validate_seen(&vocabulary.seen, &[("terms", terms.len())])?;
            let expected = vocabulary
                .seen
                .get("terms")
                .map_or(terms.len() as u64, |seen| seen.get());
            if count.get() != expected || (!vocabulary.present && !terms.is_empty()) {
                return Err("the count is the number of terms".into());
            }
            if !terms.iter().all(|term| is_survey_text(&term.name)) {
                return Err("a term is short text".into());
            }
            if !terms.windows(2).all(|pair| pair[0].name < pair[1].name) {
                return Err("terms are sorted by name, each once".into());
            }
        }
        _ => {
            return Err(
                "terms and their count are both present, or both absent for a file not read".into(),
            );
        }
    }
    Ok(())
}

fn validate_checks(checks: &SurveyChecksV1) -> Result<(), String> {
    validate_seen(
        &checks.seen,
        &[
            ("declared", checks.declared.len()),
            ("toolchain", checks.toolchain.len()),
            ("unreadable", checks.unreadable.len()),
        ],
    )?;
    for check in &checks.declared {
        let source = check.source == SURVEY_CODE_POLICY_SOURCE
            || (crate::path::is_valid_repo_path(&check.source)
                && !check.source.contains('/')
                && check.source.ends_with(".toml"));
        if !source
            || !is_survey_text(&check.name)
            || !is_survey_text(&check.program)
            || check.program.starts_with('/')
        {
            return Err(
                "a declared check is `code-policy` or a pipeline file's basename, a name and a \
                 program that is not a host path"
                    .into(),
            );
        }
    }
    if !checks.declared.windows(2).all(|pair| {
        (pair[0].identity(), &pair[0].program) <= (pair[1].identity(), &pair[1].program)
    }) {
        return Err(
            "declared checks are sorted by identity, `<source>:<name>`, then program".into(),
        );
    }
    for list in [&checks.toolchain, &checks.unreadable] {
        if !list.windows(2).all(|pair| pair[0] < pair[1])
            || !list
                .iter()
                .all(|name| crate::path::is_valid_repo_path(name))
        {
            return Err(
                "toolchain declarations and unreadable declarations are sorted repository paths"
                    .into(),
            );
        }
    }
    Ok(())
}

/// Payload of `SurveyRecorded@1`: the Project Store holds the Survey `survey_id` in its CAS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyRecordedPayloadV1 {
    /// The CAS digest of the record's canonical bytes.
    pub survey_id: String,
    pub head: String,
    pub cutoff_unix_s: DecimalU64,
    pub completeness: SurveyCompletenessV1,
    pub policy_digest: String,
    pub surveyor_version: DecimalU64,
    /// The refs of the survey key: the Survey's `identity.refs_digest`.
    pub refs_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_survey_id: Option<String>,
    pub recorded_unix_ms: DecimalU64,
}

impl SurveyRecordedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.survey_id)
            || !crate::is_digest(&self.policy_digest)
            || !crate::is_digest(&self.refs_digest)
            || self
                .previous_survey_id
                .as_deref()
                .is_some_and(|id| !crate::is_digest(id))
        {
            return Err(
                "SurveyRecorded@1 names its Survey, policy, refs and previous Survey by digest"
                    .into(),
            );
        }
        if !is_commit_id(&self.head) {
            return Err("SurveyRecorded@1 has an invalid head commit".into());
        }
        if self.surveyor_version.get() == 0 {
            return Err("SurveyRecorded@1 has an invalid Surveyor version".into());
        }
        Ok(())
    }
}

/// Payload of `SurveyFailed@1`: a Survey of `head` could not be recorded, and why. The reason is
/// af's own sentence, never a path or an email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyFailedPayloadV1 {
    pub head: String,
    pub reason: String,
    pub recorded_unix_ms: DecimalU64,
}

impl SurveyFailedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !is_commit_id(&self.head) {
            return Err("SurveyFailed@1 has an invalid head commit".into());
        }
        if self.reason.trim().is_empty()
            || self.reason.len() > MAX_SURVEY_FAILURE_REASON_BYTES
            || self.reason.chars().any(char::is_control)
            || self.reason.contains('@')
        {
            return Err("SurveyFailed@1 has an invalid reason".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_A: &str = "1111111111111111111111111111111111111111";
    const ROOT_B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn a_decision_date_is_a_day_of_the_calendar() {
        for date in ["2024-02-29", "2026-01-31", "2000-02-29", "2026-12-31"] {
            assert!(is_calendar_date(date), "{date}");
        }
        for date in [
            "2026-02-31",
            "2025-02-29",
            "1900-02-29",
            "2026-04-31",
            "2026-13-01",
            "2026-00-10",
            "2026-01-00",
            "2026-1-01",
            "2026/01/01",
        ] {
            assert!(!is_calendar_date(date), "{date}");
        }
    }

    #[test]
    fn identities_have_one_spelling() {
        assert!(is_project_workspace_id("w-0123456789abcdef"));
        assert!(!is_project_workspace_id("w-0123456789ABCDEF"));
        assert!(!is_project_workspace_id("0123456789abcdef"));
        assert!(!is_project_workspace_id("w-0123"));
        assert!(is_repository_id(ROOT_A));
        assert!(is_repository_id(&format!("{ROOT_A},{ROOT_B}")));
        assert!(!is_repository_id(&format!("{ROOT_B},{ROOT_A}")), "unsorted");
        assert!(!is_repository_id(&format!("{ROOT_A},{ROOT_A}")), "repeated");
        assert!(!is_repository_id("/home/me/repo"));
        assert!(is_workspace_label("research"));
        assert!(!is_workspace_label("-research"));
        assert!(!is_workspace_label("a/b"));
    }

    #[test]
    fn payloads_refuse_paths_and_foreign_identities() {
        let registered = ProjectRegisteredPayloadV1 {
            project_id: format!("sha256:{}", "a".repeat(64)),
            repository_id: ROOT_A.into(),
            name: Some("afactory".into()),
            min_af: Some("0.12".into()),
            registered_unix_ms: 1.into(),
        };
        registered.validate().unwrap();
        for broken in [
            ProjectRegisteredPayloadV1 {
                project_id: "sha256:short".into(),
                ..registered.clone()
            },
            ProjectRegisteredPayloadV1 {
                repository_id: "/srv/repo".into(),
                ..registered.clone()
            },
            ProjectRegisteredPayloadV1 {
                name: Some(" ".into()),
                ..registered.clone()
            },
            ProjectRegisteredPayloadV1 {
                min_af: Some("0.1 /tmp".into()),
                ..registered.clone()
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
        let workspace = WorkspaceRegisteredPayloadV1 {
            workspace_id: "w-0123456789abcdef".into(),
            label: "research".into(),
            origin: WorkspaceOriginV1::Created,
            registered_unix_ms: 2.into(),
        };
        workspace.validate().unwrap();
        assert!(
            WorkspaceRegisteredPayloadV1 {
                label: "/home/me/afactory-wt-research".into(),
                ..workspace.clone()
            }
            .validate()
            .is_err()
        );
        let removed = WorkspaceRemovedPayloadV1 {
            workspace_id: "w-0123456789abcdef".into(),
            removed_unix_ms: 3.into(),
        };
        removed.validate().unwrap();
        assert!(
            WorkspaceRemovedPayloadV1 {
                workspace_id: "research".into(),
                ..removed
            }
            .validate()
            .is_err()
        );
        let text = serde_json::to_value(&workspace).unwrap();
        assert_eq!(text["registered_unix_ms"], "2");
        assert_eq!(text["origin"], "created");
    }
}

/// The totals of a collected Survey's `tree` its tombstone keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyCollectedTreeV1 {
    pub files: DecimalU64,
    pub bytes: DecimalU64,
    pub lines: DecimalU64,
}

/// The totals of a collected Survey's `history` its tombstone keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyCollectedHistoryV1 {
    pub commits: DecimalU64,
    pub merges: DecimalU64,
}

/// Payload of `SurveyCollected@1` (ADR-0152 clause 13): the tombstone of a Survey the Storage
/// Budget collected, appended before the record leaves the Project Store's CAS. It binds the
/// Survey's `SurveyRecorded@1` once the record is gone: `head`, `cutoff_unix_s`, `completeness`,
/// `refs_digest`, `policy_digest`, `surveyor_version` and `previous_survey_id`, copied from the
/// record after the event was verified against it, never from the event, so replay refuses an
/// event that differs from its tombstone on any of them; and it keeps what `af project history`
/// answers for the Survey, the totals of `tree` and `history`, each absent when the record held
/// no such section. A record whose bytes were read but are no Survey, or that is gone from the
/// CAS, vouches for nothing: it is collected all the same, its totals replaced by `unreadable`,
/// af's own sentence of why, at most [`MAX_SURVEY_TEXT_BYTES`], and its tombstone keeps the
/// event's fields as collection read them, which `unreadable` marks as bound by no record. A
/// record that could not be read is never collected. Like every Project Store event, no path
/// and no email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyCollectedPayloadV1 {
    /// The CAS digest the collected record had.
    pub survey_id: String,
    pub head: String,
    pub cutoff_unix_s: DecimalU64,
    pub completeness: SurveyCompletenessV1,
    /// The record's `identity.refs_digest`.
    pub refs_digest: String,
    /// The record's `identity.policy_digest`.
    pub policy_digest: String,
    /// The record's `identity.surveyor_version`.
    pub surveyor_version: DecimalU64,
    /// The record's `previous_survey_id`, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_survey_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<SurveyCollectedTreeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<SurveyCollectedHistoryV1>,
    /// Why the record's bytes are no Survey: present only in place of `tree` and `history`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
    pub collected_unix_ms: DecimalU64,
}

impl SurveyCollectedPayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::is_digest(&self.survey_id)
            || !crate::is_digest(&self.refs_digest)
            || !crate::is_digest(&self.policy_digest)
            || self
                .previous_survey_id
                .as_deref()
                .is_some_and(|id| !crate::is_digest(id))
        {
            return Err(
                "SurveyCollected@1 names its Survey, refs, policy and previous Survey by digest"
                    .into(),
            );
        }
        if !is_commit_id(&self.head) {
            return Err("SurveyCollected@1 has an invalid head commit".into());
        }
        if self.surveyor_version.get() == 0 {
            return Err("SurveyCollected@1 has an invalid Surveyor version".into());
        }
        if self
            .history
            .is_some_and(|history| history.merges.get() > history.commits.get())
        {
            return Err("SurveyCollected@1 counts more merges than commits".into());
        }
        if let Some(unreadable) = &self.unreadable {
            if !is_survey_text(unreadable) {
                return Err(format!(
                    "SurveyCollected@1 says why its record is unreadable in at most \
                     {MAX_SURVEY_TEXT_BYTES} bytes of text"
                ));
            }
            if self.tree.is_some() || self.history.is_some() {
                return Err("SurveyCollected@1 keeps no totals of an unreadable record".into());
            }
        }
        Ok(())
    }
}

/// The schema of a pending-publication entry.
pub const SURVEY_PENDING_V1: &str = "af/survey-pending@1";

/// The identity of the CAS object a pending publication wrote: its device and inode, as the
/// Surveyor stated it without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyObjectIdentityV1 {
    pub device: DecimalU64,
    pub inode: DecimalU64,
}

/// `af/survey-pending@1`: one Survey the Surveyor is publishing (ADR-0152 clause 2), a
/// machine-local file beside the Project Store's lock, never in the repository or the log. It is
/// written under the lock before the record's bytes go into the CAS, naming the Survey and the
/// bytes it will hold; given the object's identity once they are written; and removed once
/// `SurveyRecorded@1` committed. One that outlives its refresh names a record a crash left
/// between the CAS and the log, which no event references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyPendingV1 {
    pub schema: String,
    pub survey_id: String,
    /// The length of the record's canonical bytes.
    pub bytes: DecimalU64,
    /// The CAS object, once the bytes were written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<SurveyObjectIdentityV1>,
}

impl SurveyPendingV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SURVEY_PENDING_V1 {
            return Err(format!(
                "a pending publication's schema is {SURVEY_PENDING_V1}"
            ));
        }
        if !crate::is_digest(&self.survey_id) {
            return Err("a pending publication names its Survey by digest".into());
        }
        if self.bytes.get() == 0 || self.bytes.get() > MAX_SURVEY_BYTES {
            return Err(format!(
                "a pending publication holds a record of 1 to {MAX_SURVEY_BYTES} bytes"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod collected_tests {
    use super::*;

    #[test]
    fn a_tombstone_binds_the_records_identity_and_keeps_only_totals() {
        let tombstone = SurveyCollectedPayloadV1 {
            survey_id: format!("sha256:{}", "a".repeat(64)),
            head: "1".repeat(40),
            cutoff_unix_s: 1_800_000_000.into(),
            completeness: SurveyCompletenessV1::Partial,
            refs_digest: format!("sha256:{}", "b".repeat(64)),
            policy_digest: format!("sha256:{}", "c".repeat(64)),
            surveyor_version: 5.into(),
            previous_survey_id: Some(format!("sha256:{}", "d".repeat(64))),
            tree: Some(SurveyCollectedTreeV1 {
                files: 16.into(),
                bytes: 1496.into(),
                lines: 70.into(),
            }),
            history: Some(SurveyCollectedHistoryV1 {
                commits: 8.into(),
                merges: 1.into(),
            }),
            unreadable: None,
            collected_unix_ms: 1_800_000_000_123.into(),
        };
        tombstone.validate().unwrap();
        let value = serde_json::to_value(&tombstone).unwrap();
        assert_eq!(value["tree"]["files"], "16");
        assert_eq!(value["surveyor_version"], "5");
        assert_eq!(
            value["previous_survey_id"],
            format!("sha256:{}", "d".repeat(64))
        );
        assert!(value.get("unreadable").is_none());
        assert_eq!(value["collected_unix_ms"], "1800000000123");
        let bare = SurveyCollectedPayloadV1 {
            tree: None,
            history: None,
            ..tombstone.clone()
        };
        bare.validate().unwrap();
        assert!(serde_json::to_value(&bare).unwrap().get("tree").is_none());
        let unreadable = SurveyCollectedPayloadV1 {
            unreadable: Some("its bytes do not match its digest".into()),
            ..bare.clone()
        };
        unreadable.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&unreadable).unwrap()["unreadable"],
            "its bytes do not match its digest"
        );
        for broken in [
            SurveyCollectedPayloadV1 {
                survey_id: "survey-1".into(),
                ..tombstone.clone()
            },
            SurveyCollectedPayloadV1 {
                head: "HEAD".into(),
                ..tombstone.clone()
            },
            // Every identity field the tombstone binds is the record's, by digest.
            SurveyCollectedPayloadV1 {
                refs_digest: "refs".into(),
                ..tombstone.clone()
            },
            SurveyCollectedPayloadV1 {
                policy_digest: "policy".into(),
                ..tombstone.clone()
            },
            SurveyCollectedPayloadV1 {
                previous_survey_id: Some("survey-0".into()),
                ..tombstone.clone()
            },
            SurveyCollectedPayloadV1 {
                surveyor_version: 0.into(),
                ..tombstone.clone()
            },
            SurveyCollectedPayloadV1 {
                history: Some(SurveyCollectedHistoryV1 {
                    commits: 1.into(),
                    merges: 2.into(),
                }),
                ..tombstone.clone()
            },
            // An unreadable record has no totals, and its reason is bounded text.
            SurveyCollectedPayloadV1 {
                unreadable: Some("corrupt".into()),
                ..tombstone.clone()
            },
            SurveyCollectedPayloadV1 {
                unreadable: Some("x".repeat(MAX_SURVEY_TEXT_BYTES + 1)),
                ..bare.clone()
            },
            SurveyCollectedPayloadV1 {
                unreadable: Some(" ".into()),
                ..bare.clone()
            },
            SurveyCollectedPayloadV1 {
                unreadable: Some("line\nbreak".into()),
                ..bare.clone()
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
        let mut unknown = value.clone();
        unknown["path"] = serde_json::json!("/home/me/afactory");
        assert!(serde_json::from_value::<SurveyCollectedPayloadV1>(unknown).is_err());
    }
}

/// Schema of the machine-local record of the collected Surveys whose records have left the CAS
/// (ADR-0152 clause 13).
pub const PROJECT_SURVEYS_SETTLED_V1: &str = "af/ProjectSurveysSettled@1";

/// Upper bound on the `af/ProjectSurveysSettled@1` file: some 230 000 Survey IDs.
pub const MAX_SURVEYS_SETTLED_BYTES: u64 = 16 << 20;

/// `af/ProjectSurveysSettled@1` (ADR-0152 clause 13): `settled.json` beside `project.lock`, the
/// collected Surveys whose own records have left the CAS, deleted at their name or settled from
/// a dead pass's container. Collection adds a Survey under the Store's lock once its record has
/// left its CAS name and before it is unlinked, and every later inventory reads it: a file at a
/// settled Survey's CAS name is foreign even when its bytes hash to the Survey ID, sweep after
/// sweep. `survey_ids` is sorted and holds each ID once. Machine-local: it names no path, and is
/// never in a log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSurveysSettledV1 {
    pub schema: String,
    pub survey_ids: Vec<String>,
}

impl ProjectSurveysSettledV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PROJECT_SURVEYS_SETTLED_V1 {
            return Err(format!(
                "the settled record is {PROJECT_SURVEYS_SETTLED_V1}, not `{}`",
                self.schema
            ));
        }
        if self.survey_ids.iter().any(|id| !crate::is_digest(id)) {
            return Err("the settled record names its Surveys by digest".into());
        }
        if self.survey_ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("the settled record lists its Surveys sorted, each once".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod machine_local_tests {
    use super::*;

    fn id(digit: char) -> String {
        format!("sha256:{}", digit.to_string().repeat(64))
    }

    #[test]
    fn the_settled_record_lists_each_survey_once_in_order() {
        let settled = ProjectSurveysSettledV1 {
            schema: PROJECT_SURVEYS_SETTLED_V1.into(),
            survey_ids: vec![id('a'), id('b')],
        };
        settled.validate().unwrap();
        for survey_ids in [
            vec![id('b'), id('a')],
            vec![id('a'), id('a')],
            vec!["a".into()],
        ] {
            assert!(
                ProjectSurveysSettledV1 {
                    survey_ids,
                    ..settled.clone()
                }
                .validate()
                .is_err()
            );
        }
    }
}
