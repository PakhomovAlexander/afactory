//! The Project and its Workspaces (ADR-0151): the events a Project Store's log holds.
//!
//! A Project is the durable identity of one repository, derived from the `repository_id` every
//! Snapshot origin already carries (its sorted root commits). A Workspace is one registered
//! checkout of it. The log names a Workspace by an opaque ID and a label only: checkout, Store,
//! bindings and Task-file paths are machine-local and live in `workspaces.toml` beside the Store,
//! never in a record. The identities are derived by the `af` binary; these types fix their
//! spelling and refuse anything that could carry a host path.
//!
//! A Survey (ADR-0152) is `af/ProjectSurvey@1`, one kernel-measured description of the Project at
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
pub const PROJECT_SURVEY_V1: &str = "af/ProjectSurvey@1";

/// How many release tags a Survey lists, newest first.
pub const SURVEY_RELEASE_TAGS: usize = 5;

/// Upper bound on the `hot_paths` a Survey lists.
pub const MAX_SURVEY_HOT_PATHS: usize = 1000;

/// Upper bound on one display string a Survey holds: a contributor name, a decision title, a
/// term, a tag, a check name or program. A longer one is cut at a character boundary and marked
/// `truncated`. Identity strings (every field of `identity`, commit IDs, digests, ref names and
/// paths) are recorded exactly and never bounded by it.
pub const MAX_SURVEY_TEXT_BYTES: usize = 256;

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

/// Whether `value` is shaped like an email address: no whitespace, and an `@` between non-empty
/// parts. An author name of this shape is never recorded as text.
pub fn is_email_shaped(value: &str) -> bool {
    !value.chars().any(char::is_whitespace)
        && value
            .char_indices()
            .any(|(index, character)| character == '@' && index > 0 && index + 1 < value.len())
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
    /// The tree held more entries than `[survey] max_paths` after `exclude`.
    MaxPaths,
    /// A list of the section held more items than `[survey] max_entries`.
    MaxEntries,
    /// A declaration the section reads is over `[survey] max_file_bytes` or not text.
    MaxFileBytes,
}

impl SurveyReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxWallMs => "max_wall_ms",
            Self::MaxCommits => "max_commits",
            Self::MaxPaths => "max_paths",
            Self::MaxEntries => "max_entries",
            Self::MaxFileBytes => "max_file_bytes",
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
    /// `sha256:` + hex(SHA-256 of the exact stdout of `git for-each-ref
    /// --format=%(refname)%09%(objectname) refs/heads refs/tags`): the refs of the survey key.
    pub refs_digest: String,
    /// The Tree Digest of the whole head tree, from its committed capture; absent, with
    /// `tree_digest_reason`, when the capture did not finish, never the digest of a part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_digest_reason: Option<SurveyReasonV1>,
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
    /// The oldest commit reached, when `max_commits` stopped the walk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_reached: Option<String>,
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

/// One contributor identity over the commits reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyContributorV1 {
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
    /// The identities with the most commits, at most `max_entries`, keyed by identity.
    pub by_identity: BTreeMap<String, SurveyContributorV1>,
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
    /// A closed status, or `unreadable` for a file over `max_file_bytes` or not text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
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
    /// The terms defined, and the first `max_entries` of them in file order, each once. Both
    /// absent when the file is over `max_file_bytes` or not text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<DecimalU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<Vec<SurveyNameV1>>,
    /// `terms`, when cut.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: SurveySeenV1,
}

/// One declared check: where it is declared, its name and its program, never its arguments.
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

/// `checks`: declared checks, toolchain declarations and CI workflow files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyChecksV1 {
    /// By source, then name, then program.
    pub declared: Vec<SurveyCheckV1>,
    /// The toolchain declarations present at the root, sorted.
    pub toolchain: Vec<String>,
    pub ci_workflows: DecimalU64,
    /// Declarations over `max_file_bytes` or not text, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
    /// `declared`, when cut.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: SurveySeenV1,
}

/// `af/ProjectSurvey@1`: one immutable, kernel-measured description of a Project at one head.
/// Canonical JSON; every number a decimal string; no host path, Provider ID or email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSurveyV1 {
    pub schema: String,
    pub completeness: SurveyCompletenessV1,
    /// The Survey before this one in the same Project Store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_survey_id: Option<String>,
    pub identity: SurveySectionV1<SurveyIdentityV1>,
    pub tree: SurveySectionV1<SurveyTreeV1>,
    pub history: SurveySectionV1<SurveyHistoryV1>,
    pub contributors: SurveySectionV1<SurveyContributorsV1>,
    pub decisions: SurveySectionV1<SurveyDecisionsV1>,
    pub vocabulary: SurveySectionV1<SurveyVocabularyV1>,
    pub checks: SurveySectionV1<SurveyChecksV1>,
}

fn is_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

fn is_top_level(name: &str) -> bool {
    name == "." || (crate::path::is_valid_repo_path(name) && !name.contains('/'))
}

impl ProjectSurveyV1 {
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

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PROJECT_SURVEY_V1 {
            return Err(format!("a Survey's schema is {PROJECT_SURVEY_V1}"));
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
    if history.commits.get() == 0
        || history.commits.get() > history.reachable_commits.get()
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
    if history
        .oldest_reached
        .as_deref()
        .is_some_and(|commit| !is_commit_id(commit))
    {
        return Err("the oldest commit reached is a commit ID".into());
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
        .values()
        .map(|entry| entry.commits.get())
        .sum();
    let active = contributors
        .by_identity
        .values()
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
    for (identity, entry) in &contributors.by_identity {
        let named = match contributors.identity {
            SurveyIdentityPolicyV1::Name => {
                is_name_hash(identity)
                    || (is_survey_text(identity)
                        && identity == identity.trim()
                        && !is_email_shaped(identity))
            }
            SurveyIdentityPolicyV1::EmailHash => {
                identity.len() == SURVEY_EMAIL_HASH_HEX_LEN && is_lower_hex(identity)
            }
        };
        if !named {
            return Err(format!(
                "an identity under `{}` is {}, never an email",
                contributors.identity.as_str(),
                match contributors.identity {
                    SurveyIdentityPolicyV1::Name =>
                        "a name that is not an email address, or `h:` and 16 hex digits",
                    SurveyIdentityPolicyV1::EmailHash => "16 lowercase hex digits",
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
            || (unreadable && (decision.title.is_some() || decision.date.is_some()))
            || decision.date.as_deref().is_some_and(|date| !is_date(date))
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
    validate_seen(&checks.seen, &[("declared", checks.declared.len())])?;
    for check in &checks.declared {
        if !crate::path::is_valid_repo_path(&check.source)
            || !is_survey_text(&check.name)
            || !is_survey_text(&check.program)
            || check.program.starts_with('/')
        {
            return Err(
                "a declared check is a repository source, a name and a program that is not a \
                 host path"
                    .into(),
            );
        }
    }
    if !checks.declared.windows(2).all(|pair| pair[0] <= pair[1]) {
        return Err("declared checks are sorted by source, name and program".into());
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
/// Budget collected, appended before the record leaves the Project Store's CAS. It keeps what
/// `af project history` answers for the Survey once its record is gone: the head, the cutoff,
/// the completeness and the totals of `tree` and `history`, each absent when the record held no
/// such section. A record whose bytes were read but are corrupt is collected all the same, its
/// totals replaced by `unreadable`, af's own sentence of why, at most [`MAX_SURVEY_TEXT_BYTES`];
/// a record that could not be read is never collected. Like every Project Store event, no path
/// and no email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurveyCollectedPayloadV1 {
    /// The CAS digest the collected record had.
    pub survey_id: String,
    pub head: String,
    pub cutoff_unix_s: DecimalU64,
    pub completeness: SurveyCompletenessV1,
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
        if !crate::is_digest(&self.survey_id) {
            return Err("SurveyCollected@1 names its Survey by digest".into());
        }
        if !is_commit_id(&self.head) {
            return Err("SurveyCollected@1 has an invalid head commit".into());
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

#[cfg(test)]
mod collected_tests {
    use super::*;

    #[test]
    fn a_tombstone_names_its_survey_by_digest_and_keeps_only_totals() {
        let tombstone = SurveyCollectedPayloadV1 {
            survey_id: format!("sha256:{}", "a".repeat(64)),
            head: "1".repeat(40),
            cutoff_unix_s: 1_800_000_000.into(),
            completeness: SurveyCompletenessV1::Partial,
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
