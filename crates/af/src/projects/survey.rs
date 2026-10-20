//! The Surveyor (ADR-0152): the token-free operator that measures a Project into Surveys.
//!
//! A Survey is `af/ProjectSurvey@1`, one canonical JSON record of the head: `identity`, `tree`,
//! `history`, `contributors`, `decisions`, `vocabulary` and `checks`, every number decimal text,
//! every section bounded by `[survey]` and marked complete, partial or unavailable. It goes into
//! the Project Store's CAS and `SurveyRecorded@1` into its log; a Survey that cannot be taken
//! records `SurveyFailed@1`. The kernel measures and a model never writes a number (ADR-0132):
//! the Surveyor calls no Provider, takes no writer lease on a Task Store, reads the checkout only
//! through the supervised plumbing of `projects/git.rs`, runs no hook or project code and writes
//! nothing inside the checkout.
//!
//! It runs synchronously at `af onboard --apply` and on `af project refresh`. A complete newest
//! Survey is reused while its survey key is unchanged: the head commit, the digest of the one ref
//! observation (`refs/heads` and `refs/tags`: names, targets, object types and creator dates,
//! which also give `history` its tags and branches), the policy digest and the Surveyor version.
//! Then nothing past `rev-parse HEAD`, `for-each-ref`, the policy and the log is read, and
//! nothing is recorded; a `partial` or `unavailable` newest Survey is measured again, and
//! `--force` records regardless of the key. Replay binds every Survey event to its record, or to
//! its tombstone once collected, before any of its fields is printed or used.

mod measure;
pub(crate) mod policy;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::time::Instant;

use review_core::EventType;
use review_core::project::{
    MAX_SURVEY_BYTES, PROJECT_SURVEY_V1, ProjectSurveyV1, SurveyCollectedPayloadV1,
    SurveyCompletenessV1, SurveyFailedPayloadV1, SurveyRecordedPayloadV1,
};
use review_store::NewEvent;
use serde_json::Value;

use policy::Policy;

use super::{Checkout, Git, Log, ProjectStore, RUN_ID};

/// The Surveyor's measurement rules. Bumped whenever a rule changes, so a head measured under
/// the old rules is surveyed again. 2: the survey key's refs, the fixed maxima, one pattern
/// grammar, the stated measurement policy and the agent-assisted partition. 3: the deadline
/// through every section's post-processing and the key, contributor keys and ranking before
/// every cut, the status-line grammar, the checks discovery set, language patterns, UTF-8
/// declarations and the bounded walk and records. 4: one ref observation for the key and the
/// statistics, history from objects alone, the digest-only Tree Digest, calendar dates, the
/// clock through every finishing step and every declaration, and name hashing of blank names
/// under both identity policies.
pub(crate) const SURVEYOR_VERSION: u64 = 4;

/// Why a refresh whose deadline passed before its survey key existed failed.
const KEY_DEADLINE: &str = "the survey key could not be computed within max_wall_ms";

/// A debug build's fixture may fix the cutoff, so a fixture repository's Survey is
/// byte-identical across runs. Release binaries never read it.
#[cfg(debug_assertions)]
const TEST_CUTOFF: &str = "AF_TEST_SURVEY_CUTOFF_UNIX_S";

/// The clock at the start of a Survey, in Unix seconds: the one field of the record that
/// depends on when it was taken.
fn cutoff_now() -> u64 {
    #[cfg(debug_assertions)]
    if let Some(fixed) = std::env::var(TEST_CUTOFF)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        return fixed;
    }
    crate::storage::now_unix_ms() / 1000
}

/// The bytes of a record: RFC 8785 canonical JSON, every map sorted by key, and no JSON number
/// at all, since every number of a Survey is decimal text. A float or an integer is refused.
pub(crate) fn canonical_bytes(value: &Value) -> Result<Vec<u8>, String> {
    fn numberless(value: &Value, pointer: &mut String) -> Result<(), String> {
        match value {
            Value::Number(number) => Err(format!(
                "{}: the number {number} is not decimal text",
                if pointer.is_empty() { "/" } else { pointer }
            )),
            Value::Array(items) => items.iter().enumerate().try_for_each(|(index, item)| {
                let mark = pointer.len();
                pointer.push_str(&format!("/{index}"));
                numberless(item, pointer)?;
                pointer.truncate(mark);
                Ok(())
            }),
            Value::Object(fields) => fields.iter().try_for_each(|(key, field)| {
                let mark = pointer.len();
                pointer.push('/');
                pointer.push_str(key);
                numberless(field, pointer)?;
                pointer.truncate(mark);
                Ok(())
            }),
            Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        }
    }
    numberless(value, &mut String::new())?;
    review_store::canonicalize(value).map_err(|error| error.to_string())
}

/// The canonical bytes of `survey`, refused unless it validates.
pub(crate) fn record_bytes(survey: &ProjectSurveyV1) -> Result<Vec<u8>, String> {
    survey.validate()?;
    canonical_bytes(&serde_json::to_value(survey).map_err(|error| error.to_string())?)
}

/// One Survey as the log and the CAS hold it.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    pub(crate) event: SurveyRecordedPayloadV1,
    pub(crate) survey: ProjectSurveyV1,
    pub(crate) bytes: Vec<u8>,
}

/// What a refresh did.
#[derive(Debug)]
pub(crate) enum Refreshed {
    Recorded(Recorded),
    /// The newest Survey is complete and its survey key is the current one.
    Unchanged(Recorded),
}

/// The survey key: what a complete Survey is reused for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    head: String,
    refs_digest: String,
    policy_digest: String,
    surveyor_version: u64,
}

impl Key {
    /// Whether `recorded` is complete and was measured under this key, as its record, which
    /// replay bound to its event, says.
    fn reuses(&self, recorded: &Recorded) -> bool {
        let Some(identity) = recorded.survey.identity.body.as_ref() else {
            return false;
        };
        recorded.survey.completeness == SurveyCompletenessV1::Complete
            && identity.head == self.head
            && identity.refs_digest == self.refs_digest
            && identity.policy_digest == self.policy_digest
            && identity.surveyor_version.get() == self.surveyor_version
    }
}

/// The 16 hex digits after `sha256:` of a digest.
fn hex16(digest: &str) -> &str {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    &hex[..hex.len().min(16)]
}

/// One line naming the survey key a reused Survey was measured under.
pub(crate) fn unchanged_line(recorded: &SurveyRecordedPayloadV1) -> String {
    format!(
        "unchanged: head {}, refs {}, policy {}, surveyor {}",
        short(&recorded.head),
        hex16(&recorded.refs_digest),
        hex16(&recorded.policy_digest),
        recorded.surveyor_version.get()
    )
}

/// What the log records about Surveys, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Recorded(SurveyRecordedPayloadV1),
    Failed(SurveyFailedPayloadV1),
}

impl Log {
    /// The newest `SurveyRecorded@1`.
    pub(crate) fn newest_survey(&self) -> Option<&SurveyRecordedPayloadV1> {
        self.surveys.iter().rev().find_map(|outcome| match outcome {
            Outcome::Recorded(recorded) => Some(recorded),
            Outcome::Failed(_) => None,
        })
    }

    /// The newest `SurveyFailed@1` when no Survey was recorded after it.
    pub(crate) fn failure_since_newest_survey(&self) -> Option<&SurveyFailedPayloadV1> {
        match self.surveys.last() {
            Some(Outcome::Failed(failed)) => Some(failed),
            _ => None,
        }
    }

    /// Every recorded Survey, newest first.
    pub(crate) fn chain(&self) -> Vec<&SurveyRecordedPayloadV1> {
        self.surveys
            .iter()
            .rev()
            .filter_map(|outcome| match outcome {
                Outcome::Recorded(recorded) => Some(recorded),
                Outcome::Failed(_) => None,
            })
            .collect()
    }
}

/// The first field in which a `SurveyRecorded@1` and the record it names disagree: what replay
/// refuses, so an event can never stand for a Survey it does not describe.
fn unbound_field(
    event: &SurveyRecordedPayloadV1,
    survey: &ProjectSurveyV1,
) -> Option<&'static str> {
    let Some(identity) = survey.identity.body.as_ref() else {
        return Some("head");
    };
    [
        ("head", event.head == identity.head),
        ("refs_digest", event.refs_digest == identity.refs_digest),
        (
            "cutoff_unix_s",
            event.cutoff_unix_s == identity.cutoff_unix_s,
        ),
        ("completeness", event.completeness == survey.completeness),
        (
            "policy_digest",
            event.policy_digest == identity.policy_digest,
        ),
        (
            "surveyor_version",
            event.surveyor_version == identity.surveyor_version,
        ),
        (
            "previous_survey_id",
            event.previous_survey_id == survey.previous_survey_id,
        ),
    ]
    .into_iter()
    .find_map(|(field, equal)| (!equal).then_some(field))
}

/// The first field in which a `SurveyRecorded@1` and the tombstone that bound its Survey's
/// fields at collection disagree.
fn untombstoned_field(
    event: &SurveyRecordedPayloadV1,
    tombstone: &SurveyCollectedPayloadV1,
) -> Option<&'static str> {
    [
        ("head", event.head == tombstone.head),
        (
            "cutoff_unix_s",
            event.cutoff_unix_s == tombstone.cutoff_unix_s,
        ),
        ("completeness", event.completeness == tombstone.completeness),
    ]
    .into_iter()
    .find_map(|(field, equal)| (!equal).then_some(field))
}

/// A `SurveyRecorded@1` that does not describe the Survey it names: replay refuses it, naming
/// the event's sequence, the first field that differs and what it differs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unbound {
    pub(crate) sequence: u64,
    pub(crate) field: &'static str,
    /// `Survey <id>`, or the tombstone `SurveyCollected@1 at sequence <n>`.
    pub(crate) against: String,
}

impl std::fmt::Display for Unbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SurveyRecorded@1 at sequence {} names a {} its {} does not record",
            self.sequence, self.field, self.against
        )
    }
}

impl ProjectStore {
    /// The record `recorded` names, read back from the CAS and validated, never more than a
    /// Survey can hold: the ceiling the writer and collection share.
    fn read_survey(
        &self,
        recorded: &SurveyRecordedPayloadV1,
    ) -> Result<(ProjectSurveyV1, Vec<u8>), String> {
        let bytes = self
            .cas
            .get_bounded(&recorded.survey_id, MAX_SURVEY_BYTES)
            .map_err(|error| {
                format!(
                    "the Project Store does not hold Survey {}: {error}",
                    recorded.survey_id
                )
            })?;
        let survey: ProjectSurveyV1 = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Survey {}: {error}", recorded.survey_id))?;
        survey.validate()?;
        Ok((survey, bytes))
    }

    fn does_not_replay(&self, unbound: &Unbound) -> String {
        format!(
            "the Project Store {} does not replay: {unbound}",
            self.directory.display()
        )
    }

    /// The record `recorded` names, read back from the CAS, validated, and bound to its event:
    /// an event whose head, refs, cutoff, completeness, policy, Surveyor version or previous
    /// Survey differs from its record's is a replay error naming the event's sequence and the
    /// field. Every reader of a record (`af project survey`, `af project show`, the reuse of the
    /// survey key, a refresh's newest Survey) reads it here.
    pub(crate) fn survey(&self, recorded: &SurveyRecordedPayloadV1) -> Result<Recorded, String> {
        let (survey, bytes) = self.read_survey(recorded)?;
        if let Some(field) = unbound_field(recorded, &survey) {
            let unbound = Unbound {
                sequence: self.recorded_sequence(recorded)?,
                field,
                against: format!("Survey {}", recorded.survey_id),
            };
            return Err(self.does_not_replay(&unbound));
        }
        Ok(Recorded {
            event: recorded.clone(),
            survey,
            bytes,
        })
    }

    /// The sequence of the newest `SurveyRecorded@1` whose payload is `recorded`.
    fn recorded_sequence(&self, recorded: &SurveyRecordedPayloadV1) -> Result<u64, String> {
        Ok(self
            .recorded_events()?
            .into_iter()
            .rev()
            .find(|(_, payload)| payload == recorded)
            .map_or(0, |(sequence, _)| sequence))
    }

    /// Every `SurveyRecorded@1` of the log with its sequence, in log order.
    fn recorded_events(&self) -> Result<Vec<(u64, SurveyRecordedPayloadV1)>, String> {
        let events = self
            .events
            .replay(RUN_ID)
            .map_err(|error| format!("the Project Store {}: {error}", self.directory.display()))?;
        events
            .into_iter()
            .filter(|event| event.event_type == EventType::SurveyRecordedV1)
            .map(|event| {
                serde_json::from_value::<SurveyRecordedPayloadV1>(event.payload)
                    .map(|payload| (event.sequence, payload))
                    .map_err(|error| format!("SurveyRecorded@1: {error}"))
            })
            .collect()
    }

    /// Bind every `SurveyRecorded@1` of the chain, newest first, before `af project history`
    /// prints any of it: a collected Survey's event to the tombstone that bound its fields at
    /// collection, every other one to its record, and the first that disagrees is the replay
    /// error, so no event's fields reach the history unverified. A record the CAS cannot hand
    /// over (gone, unreadable, corrupt) is no evidence against its event, and the Storage
    /// Budget's to collect: it refuses nothing here, and every reader of the record refuses it.
    pub(crate) fn verify_chain(&self) -> Result<(), String> {
        let mut tombstones: std::collections::BTreeMap<String, (u64, SurveyCollectedPayloadV1)> =
            std::collections::BTreeMap::new();
        for event in self
            .events
            .replay(collect::COLLECTION_RUN)
            .map_err(|error| format!("the Survey tombstones: {error}"))?
        {
            if event.event_type == EventType::SurveyCollectedV1 {
                let tombstone: SurveyCollectedPayloadV1 = serde_json::from_value(event.payload)
                    .map_err(|error| format!("SurveyCollected@1: {error}"))?;
                tombstones
                    .entry(tombstone.survey_id.clone())
                    .or_insert((event.sequence, tombstone));
            }
        }
        for (sequence, recorded) in self.recorded_events()?.into_iter().rev() {
            let unbound = match tombstones.get(&recorded.survey_id) {
                Some((at, tombstone)) => {
                    untombstoned_field(&recorded, tombstone).map(|field| Unbound {
                        sequence,
                        field,
                        against: format!("tombstone SurveyCollected@1 at sequence {at}"),
                    })
                }
                None => match self.read_survey(&recorded) {
                    Ok((survey, _)) => unbound_field(&recorded, &survey).map(|field| Unbound {
                        sequence,
                        field,
                        against: format!("Survey {}", recorded.survey_id),
                    }),
                    Err(_) => None,
                },
            };
            if let Some(unbound) = unbound {
                return Err(self.does_not_replay(&unbound));
            }
        }
        Ok(())
    }

    fn record_survey(
        &mut self,
        survey: &ProjectSurveyV1,
        bytes: Vec<u8>,
    ) -> Result<Recorded, String> {
        let survey_id = self.cas.put(&bytes).map_err(|error| error.to_string())?;
        let identity = survey
            .identity
            .body
            .as_ref()
            .ok_or("a Survey without its identity")?;
        let event = SurveyRecordedPayloadV1 {
            survey_id: survey_id.clone(),
            head: identity.head.clone(),
            cutoff_unix_s: identity.cutoff_unix_s,
            completeness: survey.completeness,
            policy_digest: identity.policy_digest.clone(),
            surveyor_version: identity.surveyor_version,
            refs_digest: identity.refs_digest.clone(),
            previous_survey_id: survey.previous_survey_id.clone(),
            recorded_unix_ms: crate::storage::now_unix_ms().into(),
        };
        event.validate()?;
        self.append_new(
            NewEvent::new(
                EventType::SurveyRecordedV1,
                serde_json::to_value(&event).map_err(|error| error.to_string())?,
            )
            .referencing(vec![survey_id]),
        )?;
        Ok(Recorded {
            event,
            survey: survey.clone(),
            bytes,
        })
    }

    fn record_failure(&mut self, head: &str, reason: &str) -> Result<(), String> {
        let payload = SurveyFailedPayloadV1 {
            head: head.to_string(),
            reason: public_reason(reason),
            recorded_unix_ms: crate::storage::now_unix_ms().into(),
        };
        payload.validate()?;
        self.append_new(NewEvent::new(
            EventType::SurveyFailedV1,
            serde_json::to_value(&payload).map_err(|error| error.to_string())?,
        ))
    }
}

/// A reason a `SurveyFailed@1` may hold: one line, bounded, never `@`.
fn public_reason(reason: &str) -> String {
    let line: String = reason
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let mut line = line.trim().to_string();
    if line.contains('@') || line.is_empty() {
        line = "the Survey failed; run `af project refresh` to see why".into();
    }
    let mut end = line
        .len()
        .min(review_core::project::MAX_SURVEY_FAILURE_REASON_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.truncate(end);
    line
}

fn open(checkout: &Checkout) -> Result<ProjectStore, String> {
    let store = ProjectStore::open(&crate::xdg_state_root()?, &checkout.project_id)?;
    if store.log()?.project.is_none() {
        return Err("the Project is not registered; run `af project show` first".into());
    }
    Ok(store)
}

/// Record a Survey of the checkout's head now, synchronously, unless the newest Survey is
/// complete under the current survey key and `force` is not asked. One `max_wall_ms` deadline
/// runs from before `rev-parse` to the last section: a deadline that passes before the key
/// exists records `SurveyFailed@1`. A Survey that cannot be taken, or whose record is over
/// `max_survey_bytes`, records `SurveyFailed@1` and is the error.
pub(crate) fn refresh(checkout: &Checkout, force: bool) -> Result<Refreshed, String> {
    let started = Instant::now();
    let git = Git::open(&checkout.toplevel)?;
    let policy = Policy::load(&checkout.toplevel).and_then(within_pattern_limits);
    let clock = measure::Clock::new(
        started,
        policy
            .as_ref()
            .map_or(Policy::default().max_wall_ms, |policy| policy.max_wall_ms),
    );
    // The key's reads are no section's: a fixture that slows one section leaves them alone.
    let key_clock = clock.section("key");
    let head = match measure::head(&git, &key_clock) {
        Ok(Some(head)) => head,
        Ok(None) => return Err(format!("{KEY_DEADLINE}; no head was read")),
        Err(failure) => return Err(format!("{}: {}", failure.reason, failure.detail)),
    };
    let policy = match policy {
        Ok(policy) => policy,
        Err(reason) => {
            open(checkout)?.record_failure(&head, &reason)?;
            return Err(format!(
                "{reason}; fix the [survey] table and run `af project refresh` again"
            ));
        }
    };
    // One ref observation: the key's `refs_digest` and `history`'s tags and branches.
    let observed = match measure::observe_refs(&git, &key_clock, &policy) {
        Ok(Ok(observed)) => observed,
        Ok(Err(reason)) => {
            let reason = match reason {
                review_core::project::SurveyReasonV1::MaxWallMs => KEY_DEADLINE.to_string(),
                other => format!(
                    "the survey key could not be computed: a ref is longer than {} bytes ({})",
                    review_core::project::MAX_SURVEY_RECORD_BYTES,
                    other.as_str()
                ),
            };
            open(checkout)?.record_failure(&head, &reason)?;
            return Err(reason);
        }
        Err(failure) => {
            open(checkout)?.record_failure(&head, &failure.reason)?;
            return Err(format!("{}: {}", failure.reason, failure.detail));
        }
    };
    let refs_digest = observed.digest.clone();
    let store = open(checkout)?;
    let key = Key {
        head: head.clone(),
        refs_digest: refs_digest.clone(),
        policy_digest: policy.digest()?,
        surveyor_version: SURVEYOR_VERSION,
    };
    if !force
        && let Some(newest) = newest(&store, &store.log()?)?
        && key.reuses(&newest)
    {
        return Ok(Refreshed::Unchanged(newest));
    }
    // The Store is not held while the head is measured.
    drop(store);
    let git_version = match measure::git_version(&git, &key_clock) {
        Ok(Some(version)) => version,
        Ok(None) => {
            open(checkout)?.record_failure(&head, KEY_DEADLINE)?;
            return Err(KEY_DEADLINE.into());
        }
        Err(failure) => {
            let reason = "git --version failed; run `af project refresh` again";
            open(checkout)?.record_failure(&head, reason)?;
            return Err(format!("{reason}: {}", failure.detail));
        }
    };
    let basis = measure::Basis {
        project_id: &checkout.project_id,
        repository_id: &checkout.repository_id,
        head: &head,
        refs_digest: &refs_digest,
        policy: &policy,
        policy_digest: &key.policy_digest,
        cutoff: cutoff_now(),
        surveyor_version: SURVEYOR_VERSION,
        git_version: &git_version,
    };
    measure::hand_over(observed);
    let measured = collect::measure(&git, &basis, started, clock);
    let mut store = open(checkout)?;
    let sections = match measured {
        Ok(sections) => sections,
        Err(failure) => {
            store.record_failure(&head, &failure.reason)?;
            return Err(format!("{}: {}", failure.reason, failure.detail));
        }
    };
    let log = store.log()?;
    if !force
        && let Some(newest) = newest(&store, &log)?
        && key.reuses(&newest)
    {
        // Another refresh recorded this head while this one measured it.
        return Ok(Refreshed::Unchanged(newest));
    }
    let completeness = SurveyCompletenessV1::fold([
        sections.identity.completeness,
        sections.tree.completeness,
        sections.history.completeness,
        sections.contributors.completeness,
        sections.decisions.completeness,
        sections.vocabulary.completeness,
        sections.checks.completeness,
    ]);
    let survey = ProjectSurveyV1 {
        schema: PROJECT_SURVEY_V1.into(),
        completeness,
        previous_survey_id: log.newest_survey().map(|newest| newest.survey_id.clone()),
        identity: sections.identity,
        tree: sections.tree,
        history: sections.history,
        contributors: sections.contributors,
        decisions: sections.decisions,
        vocabulary: sections.vocabulary,
        checks: sections.checks,
    };
    let bytes = record_bytes(&survey)?;
    if bytes.len() as u64 > policy.max_survey_bytes.min(MAX_SURVEY_BYTES) {
        let reason = format!(
            "the Survey needs {} bytes, more than [survey] max_survey_bytes = {}; raise it to at \
             most {}, exclude paths or lower max_entries, and run `af project refresh` again",
            bytes.len(),
            policy.max_survey_bytes,
            review_core::project::MAX_SURVEY_BYTES
        );
        store.record_failure(&head, &reason)?;
        return Err(reason);
    }
    store.record_survey(&survey, bytes).map(Refreshed::Recorded)
}

/// The newest recorded Survey of the Project while it is uncollected (ADR-0152 clause 13): a
/// newest Survey a tombstone names is none.
pub(crate) fn newest(store: &ProjectStore, log: &Log) -> Result<Option<Recorded>, String> {
    let collected = collect::collected_ids(&store.events)?;
    log.newest_survey()
        .filter(|newest| !collected.contains(&newest.survey_id))
        .map(|newest| store.survey(newest))
        .transpose()
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

fn time_s(unix_s: u64) -> String {
    review_core::task::collection::collected_time(unix_s.saturating_mul(1000))
}

/// One line: which Survey, of which head, how complete.
pub(crate) fn summary_line(recorded: &Recorded) -> String {
    format!(
        "Survey {} of {} ({})",
        recorded.event.survey_id,
        short(&recorded.event.head),
        recorded.survey.completeness.as_str()
    )
}

/// The sections of a Survey, in record order: what `af project survey --section` names.
pub(crate) const SECTION_NAMES: [&str; 7] = [
    "identity",
    "tree",
    "history",
    "contributors",
    "decisions",
    "vocabulary",
    "checks",
];

/// The Survey in a few lines, one per section, every number the record's.
pub(crate) fn summary(recorded: &Recorded) -> Vec<String> {
    summary_of(recorded, &[])
}

/// [`summary`] with only the sections `only` names, its first lines and then each named
/// section's line in record order; every section when `only` is empty.
pub(crate) fn summary_of(recorded: &Recorded, only: &[String]) -> Vec<String> {
    let mut lines = all_lines(recorded);
    let sections = lines.split_off(lines.len() - SECTION_NAMES.len());
    lines.extend(
        sections
            .into_iter()
            .zip(SECTION_NAMES)
            .filter(|(_, name)| only.is_empty() || only.iter().any(|named| named == name))
            .map(|(line, _)| line),
    );
    lines
}

fn all_lines(recorded: &Recorded) -> Vec<String> {
    let survey = &recorded.survey;
    let mut lines = vec![
        format!("Survey {}", recorded.event.survey_id),
        format!("  head          {}", recorded.event.head),
        format!(
            "  cutoff        {}",
            time_s(recorded.event.cutoff_unix_s.get())
        ),
        format!("  completeness  {}", survey.completeness.as_str()),
    ];
    let state = |completeness: SurveyCompletenessV1,
                 reason: Option<review_core::project::SurveyReasonV1>| {
        match (completeness, reason) {
            (SurveyCompletenessV1::Complete, _) => String::new(),
            (completeness, Some(reason)) => {
                format!(" ({}: {})", completeness.as_str(), reason.as_str())
            }
            (completeness, None) => format!(" ({})", completeness.as_str()),
        }
    };
    let line = |name: &str, measured: Option<String>, completeness, reason| {
        format!(
            "  {name:<13} {}{}",
            measured.unwrap_or_else(|| "-".into()),
            state(completeness, reason)
        )
    };
    let identity = survey.identity.body.as_ref();
    lines.push(line(
        "identity",
        identity.map(|identity| {
            format!(
                "{}, tree {}",
                identity.name.as_deref().unwrap_or("(no name)"),
                identity.tree_digest.clone().unwrap_or_else(|| format!(
                    "digest unavailable ({})",
                    identity
                        .tree_digest_reason
                        .map_or("-", review_core::project::SurveyTreeDigestReasonV1::as_str)
                ))
            )
        }),
        survey.identity.completeness,
        survey.identity.reason,
    ));
    lines.push(line(
        "tree",
        survey.tree.body.as_ref().map(|tree| {
            format!(
                "{} files, {} lines, {} bytes, {} test files",
                tree.files.get(),
                tree.lines.get(),
                tree.bytes.get(),
                tree.tests.files.get()
            )
        }),
        survey.tree.completeness,
        survey.tree.reason,
    ));
    lines.push(line(
        "history",
        survey.history.body.as_ref().map(|history| {
            format!(
                "{} commits ({} merges), {} in 30 days, {} in 90 days",
                history.commits.get(),
                history.merges.get(),
                history.windows.days_30.commits.get(),
                history.windows.days_90.commits.get()
            )
        }),
        survey.history.completeness,
        survey.history.reason,
    ));
    lines.push(line(
        "contributors",
        survey.contributors.body.as_ref().map(|contributors| {
            format!(
                "{} identities, {} active in 90 days, agent commits {} bp",
                contributors.identities.get(),
                contributors.active_90d.get(),
                contributors.agent_share_bp.get()
            )
        }),
        survey.contributors.completeness,
        survey.contributors.reason,
    ));
    lines.push(line(
        "decisions",
        survey.decisions.body.as_ref().map(|decisions| {
            let statuses: Vec<String> = decisions
                .by_status
                .iter()
                .map(|(status, count)| format!("{status} {}", count.get()))
                .collect();
            if statuses.is_empty() {
                decisions.count.get().to_string()
            } else {
                format!("{} ({})", decisions.count.get(), statuses.join(", "))
            }
        }),
        survey.decisions.completeness,
        survey.decisions.reason,
    ));
    lines.push(line(
        "vocabulary",
        survey.vocabulary.body.as_ref().map(|vocabulary| {
            if let (true, Some(count)) = (vocabulary.present, vocabulary.count) {
                format!("{} terms in {}", count.get(), vocabulary.file)
            } else if vocabulary.present {
                format!("{}, not read", vocabulary.file)
            } else {
                format!("no {}", vocabulary.file)
            }
        }),
        survey.vocabulary.completeness,
        survey.vocabulary.reason,
    ));
    lines.push(line(
        "checks",
        survey.checks.body.as_ref().map(|checks| {
            format!(
                "{} declared, {} toolchain files, {} CI workflows",
                checks.declared.len(),
                checks.toolchain.len(),
                checks.ci_workflows.get()
            )
        }),
        survey.checks.completeness,
        survey.checks.reason,
    ));
    lines
}

/// After `af onboard --apply` registered the Project and the Workspace: record the first Survey
/// and print its summary line, on stdout, or on stderr when stdout carries `--json`. A Survey
/// that fails prints one line on stderr; nothing changes onboarding's output or exit code.
pub(crate) fn after_onboard(repo: &Path, json: bool) {
    let Ok(super::Located::Checkout(checkout)) = super::locate(repo) else {
        return;
    };
    let say = |line: String| {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    match refresh(&checkout, false) {
        Ok(Refreshed::Recorded(recorded)) => {
            say(format!("af onboard: recorded {}", summary_line(&recorded)))
        }
        Ok(Refreshed::Unchanged(recorded)) => say(format!(
            "af onboard: {}; {}",
            unchanged_line(&recorded.event),
            summary_line(&recorded)
        )),
        Err(error) => eprintln!("af onboard: no Survey was recorded: {error}"),
    }
}

pub(crate) mod collect;

/// `af project`, as `collect::run` answers it, with what replay binds first: `af project
/// history` verifies every Survey event of the chain against its record or its tombstone before
/// any of it is printed, and `af project survey --section`, a text-only option, prints the named
/// sections of the newest Survey's summary. `--section` with `--json` never reaches here: it is
/// usage error 2.
pub(crate) fn run(command: crate::cli::ProjectCommand, sections: &[String]) -> Result<i32, String> {
    use crate::cli::ProjectCommand;
    match command {
        ProjectCommand::History { json } => {
            if let Some(store) = existing_store_here()? {
                store.verify_chain()?;
            }
            collect::run(ProjectCommand::History { json })
        }
        ProjectCommand::Survey { json: false } if !sections.is_empty() => survey_sections(sections),
        other => collect::run(other),
    }
}

/// The invoking checkout's Project Store, when it holds a log already; opening it creates
/// nothing a later `af project` command would not.
fn existing_store_here() -> Result<Option<ProjectStore>, String> {
    let checkout = super::require_checkout(Path::new("."))?;
    let state_home = crate::xdg_state_root()?;
    let directory = super::store_directory(&state_home, &checkout.project_id);
    if std::fs::symlink_metadata(directory.join("events.sqlite")).is_err() {
        return Ok(None);
    }
    ProjectStore::open(&state_home, &checkout.project_id).map(Some)
}

/// `af project survey --section NAME…`: the newest Survey's first lines and the named sections.
fn survey_sections(sections: &[String]) -> Result<i32, String> {
    let none = "this Project has no Survey yet; run `af project refresh` to record one";
    let store = existing_store_here()?.ok_or(none)?;
    let log = store.log()?;
    if let Some(newest) = log.newest_survey()
        && let Some(tombstone) = collect::tombstones(&store.events)?
            .into_iter()
            .find(|tombstone| tombstone.survey_id == newest.survey_id)
    {
        return Err(format!(
            "the newest Survey {} of {} was collected at {}; run `af project refresh` to \
             record one",
            newest.survey_id,
            short(&newest.head),
            review_core::task::collection::collected_time(tombstone.collected_unix_ms.get())
        ));
    }
    let recorded = newest(&store, &log)?.ok_or(none)?;
    for line in summary_of(&recorded, sections) {
        println!("{line}");
    }
    Ok(0)
}

/// The pattern limits of ADR-0152 clause 14, where the Surveyor loads its patterns: every
/// pattern list, `[survey.languages]` included, at most 64 patterns and every pattern at most
/// 256 bytes, refused naming the key.
fn within_pattern_limits(policy: Policy) -> Result<Policy, String> {
    for (key, patterns) in [
        ("exclude", &policy.exclude),
        ("tests", &policy.tests),
        ("agent_authors", &policy.agent_authors),
        ("agent_trailers", &policy.agent_trailers),
    ] {
        collect::check_patterns(key, patterns)?;
    }
    for (key, pattern) in [
        ("decisions", &policy.decisions),
        ("release_tags", &policy.release_tags),
    ] {
        collect::check_patterns(key, std::slice::from_ref(pattern))?;
    }
    // `[survey.languages]` is a pattern list like the others: only the built-in fallback is an
    // exact lookup.
    let languages: Vec<String> = policy.languages.keys().cloned().collect();
    collect::check_patterns("[survey.languages]", &languages)?;
    Ok(policy)
}
