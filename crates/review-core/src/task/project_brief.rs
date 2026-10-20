//! The Project Brief (ADR-0159): the bounded selection of one Survey that a pipeline's declared
//! `project` port carries into a Worker.
//!
//! Four records, each kernel-written and in the Task Store's CAS:
//!
//! - `af/ProjectSurveyPin@1`: the Survey a Task's initial capture pinned, its `identity` and
//!   sections verbatim and its Survey ID as provenance, with no `previous_survey_id` and no other
//!   link, so collecting an earlier Task releases its pin while a later Task keeps its own.
//! - `af/ProjectBriefCapture@1`: what the initial capture recorded beside the pin in the Task
//!   revision's provenance: the pin (or none), the project's `[brief.<name>]` variants and the
//!   ancestry observation of every Survey head against the source commit. Absent when it would
//!   say nothing: no pin, no variant of the project's own and no bound Brief.
//! - `af/ProjectBrief@1`: one variant rendered from one pin, at most the variant's `max_bytes`
//!   of canonical JSON, the sections it dropped to fit named with their reasons.
//! - `af/ProjectBriefReceipt@1`: the plan-time receipt of one Brief port of one revision: what
//!   the port carries and, for a Brief, its staleness keyed by the Brief ID and the source
//!   commit. Staleness is the consuming Task's, never a field of the Brief.
//!
//! `af/project-brief-preview@1` is the document `af project brief --json` prints.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::usage::DecimalU64;
use super::{is_name, present_option, require};
use crate::is_digest;
use crate::project::{
    MAX_SURVEY_ENTRIES, PROJECT_SURVEY_V1, ProjectSurveyV1, SurveyChecksV1, SurveyCompletenessV1,
    SurveyContributorsV1, SurveyDecisionV1, SurveyDecisionsV1, SurveyHistoryV1, SurveyHotPathV1,
    SurveyIdentityV1, SurveySectionV1, SurveyTreeV1, SurveyVocabularyV1, is_commit_id,
    is_project_id,
};

pub const PROJECT_BRIEF_V1: &str = "af/ProjectBrief@1";
pub const PROJECT_SURVEY_PIN_V1: &str = "af/ProjectSurveyPin@1";
pub const PROJECT_BRIEF_CAPTURE_V1: &str = "af/ProjectBriefCapture@1";
pub const PROJECT_BRIEF_RECEIPT_V1: &str = "af/ProjectBriefReceipt@1";
pub const PROJECT_BRIEF_PREVIEW_V1: &str = "af/project-brief-preview@1";

/// The smallest `max_bytes` a variant may declare: a Brief is context, never a dump, yet it has
/// to hold its own metadata.
pub const MIN_BRIEF_BYTES: u64 = 1024;
/// The largest `max_bytes` a variant may declare, well inside the Worker request renderer's
/// 1 MiB cap on the whole request.
pub const MAX_BRIEF_BYTES: u64 = 262_144;
pub const DEFAULT_BRIEF_BYTES: u64 = 8192;
pub const DEFAULT_BRIEF_DECISIONS: u64 = 12;
pub const DEFAULT_BRIEF_HOT_PATHS: u64 = 20;
/// The built-in variant a pipeline names for an implementer.
pub const ORIENT: &str = "orient";
/// The built-in variant that constructs nothing.
pub const NONE: &str = "none";
/// The context manifest's `required_by` of a Brief port is `port:<name>`.
pub const PORT_REQUIRED_BY_PREFIX: &str = "port:";

/// The typed reason a Brief the kernel did not construct is refused, at a Worker's output
/// admission and at a Task file's binding: ADR-0134 grants a bound output provenance, not
/// verification.
pub const NOT_KERNEL_CONSTRUCTED: &str = "not_kernel_constructed";

/// Refuse a Worker output of `artifact_type` when it is a Project Brief: a Worker never
/// authors one, whatever its declared output ports say.
pub fn refuse_worker_authored(artifact_type: &str) -> Result<(), String> {
    if artifact_type == PROJECT_BRIEF_V1 {
        return Err(format!(
            "{NOT_KERNEL_CONSTRUCTED}: a Worker output of type {PROJECT_BRIEF_V1} is refused; a \
             Project Brief is rendered by the kernel from the Survey the Task pinned"
        ));
    }
    Ok(())
}

/// Every section a variant may list, in Survey order: the Survey's own sections, `hot_paths`
/// (which is `history.hot_paths` alone) and `activity`, which no Survey of this release
/// measures, so it is never rendered.
pub const BRIEF_SECTIONS: [&str; 9] = [
    "identity",
    "tree",
    "history",
    "contributors",
    "decisions",
    "vocabulary",
    "checks",
    "hot_paths",
    "activity",
];

/// The `orient` sections: what an implementer needs to find its way, and no names or bills.
pub const ORIENT_SECTIONS: [&str; 5] = ["tree", "vocabulary", "decisions", "checks", "hot_paths"];

/// One `[brief.<name>]` variant as the policy declares it once merged over its defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefVariantV1 {
    /// Rendered in this order; dropped from the end when the Brief does not fit.
    pub sections: Vec<String>,
    pub max_bytes: DecimalU64,
    /// How many decision records `decisions` keeps: the newest.
    pub decisions: DecimalU64,
    /// How many hot paths `hot_paths` keeps: the first.
    pub hot_paths: DecimalU64,
}

impl ProjectBriefVariantV1 {
    pub fn orient() -> Self {
        Self {
            sections: ORIENT_SECTIONS.iter().map(|s| s.to_string()).collect(),
            max_bytes: DEFAULT_BRIEF_BYTES.into(),
            decisions: DEFAULT_BRIEF_DECISIONS.into(),
            hot_paths: DEFAULT_BRIEF_HOT_PATHS.into(),
        }
    }

    /// The closed schema of a variant named `name`: known, distinct sections; `max_bytes`
    /// within [`MIN_BRIEF_BYTES`] and [`MAX_BRIEF_BYTES`]; both counts at most the Survey's
    /// `max_entries` ceiling; `none` never overridden; no `contributors` or `activity` in
    /// `orient`. The error names the variant and the field.
    pub fn validate(&self, name: &str) -> Result<(), String> {
        let refused = |field: &str, why: String| Err(format!("[brief.{name}] {field}: {why}"));
        if !is_name(name) {
            return refused(
                "name",
                "a variant is named by 1 to 128 ASCII letters, digits, `-` and `_`".into(),
            );
        }
        if name == NONE {
            return refused(
                "name",
                "`none` is built in and constructs nothing; it cannot be overridden".into(),
            );
        }
        let mut seen = BTreeSet::new();
        for section in &self.sections {
            if !BRIEF_SECTIONS.contains(&section.as_str()) {
                return refused(
                    "sections",
                    format!(
                        "`{section}` is no section; a variant lists {}",
                        BRIEF_SECTIONS.join(", ")
                    ),
                );
            }
            if !seen.insert(section) {
                return refused("sections", format!("`{section}` is listed twice"));
            }
            if name == ORIENT && matches!(section.as_str(), "contributors" | "activity") {
                return refused(
                    "sections",
                    format!(
                        "`orient` never renders `{section}`: an implementer needs no names or bills"
                    ),
                );
            }
        }
        if self.sections.is_empty() {
            return refused("sections", "a variant lists at least one section".into());
        }
        let max = self.max_bytes.get();
        if !(MIN_BRIEF_BYTES..=MAX_BRIEF_BYTES).contains(&max) {
            return refused(
                "max_bytes",
                format!(
                    "{max} is outside {MIN_BRIEF_BYTES}..={MAX_BRIEF_BYTES}: a Brief is context \
                     inside the Worker request's 1 MiB cap, and has to hold its own metadata"
                ),
            );
        }
        for (field, count) in [("decisions", self.decisions), ("hot_paths", self.hot_paths)] {
            if count.get() > MAX_SURVEY_ENTRIES {
                return refused(
                    field,
                    format!(
                        "{} is above the Survey's max_entries {MAX_SURVEY_ENTRIES}",
                        count.get()
                    ),
                );
            }
        }
        Ok(())
    }
}

/// What a port's variant asks for, given the project's variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedBriefV1 {
    /// `none`: no Brief, no manifest entry, nothing on the port.
    Nothing,
    Render(ProjectBriefVariantV1),
}

/// The effective variant `name` under the project's `variants`, which override or add to the
/// built-in `orient`; `None` when neither the project nor a built-in defines it.
pub fn requested_variant(
    variants: &BTreeMap<String, ProjectBriefVariantV1>,
    name: &str,
) -> Option<RequestedBriefV1> {
    if name == NONE {
        return Some(RequestedBriefV1::Nothing);
    }
    if let Some(variant) = variants.get(name) {
        return Some(RequestedBriefV1::Render(variant.clone()));
    }
    (name == ORIENT).then(|| RequestedBriefV1::Render(ProjectBriefVariantV1::orient()))
}

/// The Survey a Task's initial capture pinned: everything a Brief is rendered from, and nothing
/// that links to another record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSurveyPinV1 {
    pub schema: String,
    /// The pinned Survey's ID in the Project Store, as provenance: no Task Store object.
    pub survey_id: String,
    pub identity: SurveySectionV1<SurveyIdentityV1>,
    pub tree: SurveySectionV1<SurveyTreeV1>,
    pub history: SurveySectionV1<SurveyHistoryV1>,
    pub contributors: SurveySectionV1<SurveyContributorsV1>,
    pub decisions: SurveySectionV1<SurveyDecisionsV1>,
    pub vocabulary: SurveySectionV1<SurveyVocabularyV1>,
    pub checks: SurveySectionV1<SurveyChecksV1>,
}

impl ProjectSurveyPinV1 {
    /// The pin of the complete Survey `survey_id` names.
    pub fn of(survey_id: &str, survey: &ProjectSurveyV1) -> Result<Self, String> {
        let pin = Self {
            schema: PROJECT_SURVEY_PIN_V1.into(),
            survey_id: survey_id.into(),
            identity: survey.identity.clone(),
            tree: survey.tree.clone(),
            history: survey.history.clone(),
            contributors: survey.contributors.clone(),
            decisions: survey.decisions.clone(),
            vocabulary: survey.vocabulary.clone(),
            checks: survey.checks.clone(),
        };
        pin.validate()?;
        Ok(pin)
    }

    /// The Survey this pin holds, without its link to the Survey before it.
    pub fn survey(&self) -> ProjectSurveyV1 {
        ProjectSurveyV1 {
            schema: PROJECT_SURVEY_V1.into(),
            completeness: SurveyCompletenessV1::Complete,
            previous_survey_id: None,
            stopped: None,
            identity: self.identity.clone(),
            tree: self.tree.clone(),
            history: self.history.clone(),
            contributors: self.contributors.clone(),
            decisions: self.decisions.clone(),
            vocabulary: self.vocabulary.clone(),
            checks: self.checks.clone(),
        }
    }

    /// The pinned head: a pin is of a complete Survey, so its identity holds one.
    pub fn identity(&self) -> Result<&SurveyIdentityV1, String> {
        self.identity
            .body
            .as_ref()
            .ok_or_else(|| "a Survey pin holds its identity".to_string())
    }

    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == PROJECT_SURVEY_PIN_V1,
            "a Survey pin's schema is af/ProjectSurveyPin@1",
        )?;
        require(
            is_digest(&self.survey_id),
            "a Survey pin names its Survey ID",
        )?;
        // Only a complete Survey is pinned; its sections validate as the Survey's did.
        self.survey()
            .validate()
            .map_err(|error| format!("a Survey pin holds a complete Survey: {error}"))
    }
}

/// The Survey head a Brief describes; `tree_digest` absent when the Survey has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefHeadV1 {
    pub commit: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub tree_digest: Option<String>,
}

/// The sections a Brief rendered, by name; every one a Survey section verbatim, except
/// `decisions` (its records the variant's newest N) and `hot_paths` (the first N of
/// `history.hot_paths`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefSectionsV1 {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub identity: Option<SurveySectionV1<SurveyIdentityV1>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub tree: Option<SurveySectionV1<SurveyTreeV1>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub history: Option<SurveySectionV1<SurveyHistoryV1>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub contributors: Option<SurveySectionV1<SurveyContributorsV1>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub decisions: Option<SurveySectionV1<SurveyDecisionsV1>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub vocabulary: Option<SurveySectionV1<SurveyVocabularyV1>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub checks: Option<SurveySectionV1<SurveyChecksV1>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub hot_paths: Option<Vec<SurveyHotPathV1>>,
}

impl ProjectBriefSectionsV1 {
    /// Whether `section` is rendered.
    pub fn has(&self, section: &str) -> bool {
        match section {
            "identity" => self.identity.is_some(),
            "tree" => self.tree.is_some(),
            "history" => self.history.is_some(),
            "contributors" => self.contributors.is_some(),
            "decisions" => self.decisions.is_some(),
            "vocabulary" => self.vocabulary.is_some(),
            "checks" => self.checks.is_some(),
            "hot_paths" => self.hot_paths.is_some(),
            _ => false,
        }
    }

    /// Drop `section`: what the drop rule does to the last-declared one still rendered.
    pub fn remove(&mut self, section: &str) {
        match section {
            "identity" => self.identity = None,
            "tree" => self.tree = None,
            "history" => self.history = None,
            "contributors" => self.contributors = None,
            "decisions" => self.decisions = None,
            "vocabulary" => self.vocabulary = None,
            "checks" => self.checks = None,
            "hot_paths" => self.hot_paths = None,
            _ => {}
        }
    }

    /// Render `section` of `pin` for a variant keeping `decisions` and `hot_paths` entries:
    /// `None` for a section the pinned Survey does not hold (`activity`, or `hot_paths` of an
    /// unavailable `history`).
    pub fn render(&mut self, pin: &ProjectSurveyPinV1, section: &str, decisions: u64, hot: u64) {
        let keep = |count: u64| usize::try_from(count).unwrap_or(usize::MAX);
        match section {
            "identity" => self.identity = Some(pin.identity.clone()),
            "tree" => self.tree = Some(pin.tree.clone()),
            "history" => self.history = Some(pin.history.clone()),
            "contributors" => self.contributors = Some(pin.contributors.clone()),
            "decisions" => {
                let mut cut = pin.decisions.clone();
                if let Some(body) = cut.body.as_mut() {
                    body.records = newest_decisions(&body.records, keep(decisions));
                }
                self.decisions = Some(cut);
            }
            "vocabulary" => self.vocabulary = Some(pin.vocabulary.clone()),
            "checks" => self.checks = Some(pin.checks.clone()),
            "hot_paths" => {
                self.hot_paths = pin
                    .history
                    .body
                    .as_ref()
                    .map(|history| history.hot_paths.iter().take(keep(hot)).cloned().collect());
            }
            _ => {}
        }
    }
}

/// The newest `count` decision records: by date descending, undated last, then by number
/// descending, then by path.
pub fn newest_decisions(records: &[SurveyDecisionV1], count: usize) -> Vec<SurveyDecisionV1> {
    let mut ordered = records.to_vec();
    ordered.sort_by(|left, right| {
        let date = |record: &SurveyDecisionV1| record.date.clone();
        match (date(left), date(right)) {
            (Some(left), Some(right)) => right.cmp(&left),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| right.number.cmp(&left.number))
        .then_with(|| left.path.cmp(&right.path))
    });
    ordered.truncate(count);
    ordered
}

/// Why a section a variant lists is not in a Brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectBriefDropReasonV1 {
    /// Dropped whole, last-declared first, until the Brief's canonical bytes fit `max_bytes`.
    MaxBytes,
    /// The pinned Survey holds no such section: `activity`, or `hot_paths` of an unavailable
    /// `history`.
    NotInSurvey,
}

impl ProjectBriefDropReasonV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxBytes => "max_bytes",
            Self::NotInSurvey => "not_in_survey",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefDropV1 {
    pub section: String,
    pub reason: ProjectBriefDropReasonV1,
}

/// `af/ProjectBrief@1`: one variant of one pinned Survey. Canonical JSON, numbers decimal
/// text; no host path, Provider ID or email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefV1 {
    pub schema: String,
    pub survey_id: String,
    pub project_id: String,
    pub variant: String,
    pub head: ProjectBriefHeadV1,
    pub sections: ProjectBriefSectionsV1,
    /// In drop order.
    pub dropped: Vec<ProjectBriefDropV1>,
    /// The byte length of this record's canonical JSON, this field included: the number the
    /// drop rule held at or under `max_bytes`.
    pub rendered_bytes: DecimalU64,
}

impl ProjectBriefV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == PROJECT_BRIEF_V1,
            "a Project Brief's schema is af/ProjectBrief@1",
        )?;
        require(
            is_digest(&self.survey_id) && is_project_id(&self.project_id),
            "a Project Brief names its Survey and Project",
        )?;
        require(
            is_name(&self.variant) && self.variant != NONE,
            "a Project Brief names the variant it rendered",
        )?;
        require(
            is_commit_id(&self.head.commit)
                && self.head.tree_digest.as_deref().is_none_or(is_digest),
            "a Project Brief's head is a commit and a Tree Digest",
        )?;
        let mut dropped = BTreeSet::new();
        for drop in &self.dropped {
            require(
                BRIEF_SECTIONS.contains(&drop.section.as_str())
                    && dropped.insert(drop.section.as_str())
                    && !self.sections.has(&drop.section),
                "a Project Brief drops each section at most once, and never one it rendered",
            )?;
        }
        Ok(())
    }
}

/// Why the plan cannot say how far a Survey head is behind the source commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectBriefUnknownV1 {
    /// The head is no ancestor of the source commit.
    NotAncestor,
    /// The planning directory was no git checkout.
    NoCheckout,
    /// The checkout does not hold the head's objects.
    MissingObjects,
    /// git did not answer within its deadline.
    Deadline,
    /// The Task's source has no commit.
    NoCommittedRevision,
}

impl ProjectBriefUnknownV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAncestor => "not_ancestor",
            Self::NoCheckout => "no_checkout",
            Self::MissingObjects => "missing_objects",
            Self::Deadline => "deadline",
            Self::NoCommittedRevision => "no_committed_revision",
        }
    }
}

/// One Survey head against the Task's source commit, observed once at the initial capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectBriefAncestryV1 {
    /// The head is an ancestor of the source commit, `behind_commits` commits behind it.
    Behind {
        behind_commits: DecimalU64,
    },
    Unknown {
        reason: ProjectBriefUnknownV1,
    },
}

impl ProjectBriefAncestryV1 {
    /// `3` or `unknown`, as the preview prints it.
    pub fn shown(&self) -> String {
        match self {
            Self::Behind { behind_commits } => behind_commits.get().to_string(),
            Self::Unknown { .. } => "unknown".into(),
        }
    }
}

/// What a Task's initial capture recorded for its Briefs, before any pipeline was selected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefCaptureV1 {
    pub schema: String,
    /// The `af/ProjectSurveyPin@1`; absent when no complete Survey was recorded.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub pin_id: Option<String>,
    /// The commit the Task's source was captured at; absent when it has none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub source_commit: Option<String>,
    /// The project's `[brief.<name>]` variants; the built-in ones are this release's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, ProjectBriefVariantV1>,
    /// Every Survey head a Brief of this Task describes (the pin's, each bound Brief's),
    /// against `source_commit`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ancestry: BTreeMap<String, ProjectBriefAncestryV1>,
}

impl ProjectBriefCaptureV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == PROJECT_BRIEF_CAPTURE_V1,
            "a Brief capture's schema is af/ProjectBriefCapture@1",
        )?;
        require(
            self.pin_id.as_deref().is_none_or(is_digest)
                && self.source_commit.as_deref().is_none_or(is_commit_id)
                && self.ancestry.keys().all(|head| is_commit_id(head)),
            "a Brief capture names its pin, source commit and heads exactly",
        )?;
        for (name, variant) in &self.variants {
            variant.validate(name)?;
        }
        Ok(())
    }

    /// Whether the record says anything an absent one would not.
    pub fn is_empty(&self) -> bool {
        self.pin_id.is_none() && self.variants.is_empty() && self.ancestry.is_empty()
    }
}

/// What a Brief port carries in one revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectBriefOutcomeV1 {
    /// Rendered from the pinned Survey by the port's variant.
    Constructed,
    /// The Task file bound it to a recorded Brief; nothing was constructed.
    Bound,
    /// Nothing: see the reason.
    None,
}

/// Why a Brief port carries nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectBriefNoneReasonV1 {
    VariantNone,
    NoSurvey,
}

/// `af/ProjectBriefReceipt@1`: one Brief port of one revision, as the plan saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefReceiptV1 {
    pub schema: String,
    pub port: String,
    pub outcome: ProjectBriefOutcomeV1,
    /// The port's variant; for a bound Brief, the variant it was rendered by.
    pub variant: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub reason: Option<ProjectBriefNoneReasonV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub brief_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub source_commit: Option<String>,
    /// How far the Brief's Survey head is behind `source_commit`, from the capture's
    /// observation; present exactly with a Brief.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub staleness: Option<ProjectBriefAncestryV1>,
}

impl ProjectBriefReceiptV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == PROJECT_BRIEF_RECEIPT_V1
                && is_name(&self.port)
                && is_name(&self.variant),
            "a Brief receipt names its schema, port and variant",
        )?;
        require(
            self.source_commit.as_deref().is_none_or(is_commit_id),
            "a Brief receipt's source commit is a commit",
        )?;
        let carries = self.brief_id.as_deref().is_some_and(is_digest) && self.staleness.is_some();
        let empty = self.brief_id.is_none() && self.staleness.is_none();
        require(
            match self.outcome {
                ProjectBriefOutcomeV1::None => self.reason.is_some() && empty,
                _ => self.reason.is_none() && carries,
            },
            "a Brief receipt holds a Brief and its staleness, or the reason it holds none",
        )
    }
}

/// The manifest entry a Worker wired to the port would see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefManifestEntryV1 {
    pub name: String,
    pub required_by: String,
    pub artifact_id: String,
    pub artifact_type: String,
    pub rendered_bytes: DecimalU64,
    pub estimated_tokens: DecimalU64,
}

/// `af/project-brief-preview@1`: what `af project brief --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefPreviewV1 {
    pub schema: String,
    pub variant: String,
    pub outcome: ProjectBriefPreviewOutcomeV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub reason: Option<ProjectBriefNoneReasonV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub brief_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub brief: Option<ProjectBriefV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub manifest: Option<ProjectBriefManifestEntryV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectBriefPreviewOutcomeV1 {
    Brief,
    None,
}

impl ProjectBriefPreviewV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == PROJECT_BRIEF_PREVIEW_V1 && is_name(&self.variant),
            "a Brief preview names its schema and variant",
        )?;
        match self.outcome {
            ProjectBriefPreviewOutcomeV1::Brief => {
                let brief = self
                    .brief
                    .as_ref()
                    .ok_or("a Brief preview holds its Brief")?;
                brief.validate()?;
                let manifest = self
                    .manifest
                    .as_ref()
                    .ok_or("a Brief preview holds its manifest entry")?;
                require(
                    self.reason.is_none()
                        && manifest.required_by
                            == format!("{PORT_REQUIRED_BY_PREFIX}{}", manifest.name)
                        && self.brief_id.as_deref() == Some(manifest.artifact_id.as_str())
                        && self.brief_id.as_deref().is_some_and(is_digest)
                        && manifest.artifact_type == PROJECT_BRIEF_V1,
                    "a Brief preview's ID, Brief and manifest entry agree",
                )
            }
            ProjectBriefPreviewOutcomeV1::None => require(
                self.reason.is_some()
                    && self.brief_id.is_none()
                    && self.brief.is_none()
                    && self.manifest.is_none(),
                "a Brief preview without a Brief says why",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(number: &str, date: Option<&str>, path: &str) -> SurveyDecisionV1 {
        serde_json::from_value(serde_json::json!({
            "number": number,
            "path": path,
            "title": format!("Decision {number}"),
        }))
        .map(|mut record: SurveyDecisionV1| {
            record.date = date.map(str::to_string);
            record
        })
        .unwrap()
    }

    #[test]
    fn decisions_are_newest_by_date_then_undated_then_number_and_path() {
        let records = vec![
            decision("0001", Some("2026-01-01"), "docs/adr/0001-a.md"),
            decision("0002", None, "docs/adr/0002-b.md"),
            decision("0003", Some("2026-03-01"), "docs/adr/0003-c.md"),
            decision("0004", Some("2026-03-01"), "docs/adr/0004-d.md"),
            decision("0005", None, "docs/adr/0005-e.md"),
            decision("0004", Some("2026-03-01"), "docs/adr/0004-a.md"),
        ];
        let numbers: Vec<_> = newest_decisions(&records, 10)
            .into_iter()
            .map(|record| record.path)
            .collect();
        assert_eq!(
            numbers,
            [
                "docs/adr/0004-a.md",
                "docs/adr/0004-d.md",
                "docs/adr/0003-c.md",
                "docs/adr/0001-a.md",
                "docs/adr/0005-e.md",
                "docs/adr/0002-b.md",
            ]
        );
        assert_eq!(newest_decisions(&records, 2).len(), 2);
        assert!(newest_decisions(&records, 0).is_empty());
    }

    #[test]
    fn a_variant_admits_exactly_its_closed_schema() {
        assert_eq!(ProjectBriefVariantV1::orient().validate(ORIENT), Ok(()));
        let with = |change: &dyn Fn(&mut ProjectBriefVariantV1)| {
            let mut variant = ProjectBriefVariantV1::orient();
            change(&mut variant);
            variant
        };
        for (name, variant, field) in [
            ("none", ProjectBriefVariantV1::orient(), "[brief.none] name"),
            (
                "slim",
                with(&|v| v.sections.push("owners".into())),
                "[brief.slim] sections",
            ),
            (
                "slim",
                with(&|v| v.sections.push("tree".into())),
                "[brief.slim] sections",
            ),
            (
                "orient",
                with(&|v| v.sections.push("contributors".into())),
                "[brief.orient] sections",
            ),
            (
                "orient",
                with(&|v| v.sections.push("activity".into())),
                "[brief.orient] sections",
            ),
            (
                "slim",
                with(&|v| v.max_bytes = 1023.into()),
                "[brief.slim] max_bytes",
            ),
            (
                "slim",
                with(&|v| v.max_bytes = 262_145.into()),
                "[brief.slim] max_bytes",
            ),
            (
                "slim",
                with(&|v| v.decisions = 1001.into()),
                "[brief.slim] decisions",
            ),
            (
                "slim",
                with(&|v| v.hot_paths = 1001.into()),
                "[brief.slim] hot_paths",
            ),
            (
                "slim",
                with(&|v| v.sections.clear()),
                "[brief.slim] sections",
            ),
        ] {
            let error = variant.validate(name).unwrap_err();
            assert!(error.starts_with(field), "{error}");
        }
        let edge = with(&|v| {
            v.max_bytes = MIN_BRIEF_BYTES.into();
            v.sections.push("contributors".into());
            v.decisions = 1000.into();
        });
        assert_eq!(edge.validate("team"), Ok(()));
        assert_eq!(
            with(&|v| v.max_bytes = MAX_BRIEF_BYTES.into()).validate("wide"),
            Ok(())
        );
    }

    #[test]
    fn none_is_built_in_and_orient_may_be_overridden() {
        let mut variants = BTreeMap::new();
        assert_eq!(
            requested_variant(&variants, NONE),
            Some(RequestedBriefV1::Nothing)
        );
        assert_eq!(
            requested_variant(&variants, ORIENT),
            Some(RequestedBriefV1::Render(ProjectBriefVariantV1::orient()))
        );
        assert_eq!(requested_variant(&variants, "slim"), None);
        let mut slim = ProjectBriefVariantV1::orient();
        slim.sections = vec!["tree".into()];
        variants.insert(ORIENT.to_string(), slim.clone());
        assert_eq!(
            requested_variant(&variants, ORIENT),
            Some(RequestedBriefV1::Render(slim))
        );
    }

    #[test]
    fn a_receipt_holds_a_brief_with_its_staleness_or_a_reason() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let mut receipt = ProjectBriefReceiptV1 {
            schema: PROJECT_BRIEF_RECEIPT_V1.into(),
            port: "project".into(),
            outcome: ProjectBriefOutcomeV1::Constructed,
            variant: ORIENT.into(),
            reason: None,
            brief_id: Some(digest),
            source_commit: Some("b".repeat(40)),
            staleness: Some(ProjectBriefAncestryV1::Behind {
                behind_commits: 3.into(),
            }),
        };
        assert_eq!(receipt.validate(), Ok(()));
        receipt.staleness = None;
        assert!(receipt.validate().is_err());
        receipt.outcome = ProjectBriefOutcomeV1::None;
        receipt.brief_id = None;
        assert!(receipt.validate().is_err());
        receipt.reason = Some(ProjectBriefNoneReasonV1::NoSurvey);
        assert_eq!(receipt.validate(), Ok(()));
    }
}
