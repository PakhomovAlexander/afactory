//! The Surveyor (ADR-0158): the token-free operator that measures a Project into Surveys.
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
pub(crate) mod pending;
pub(crate) mod policy;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::time::Instant;

use review_core::EventType;
use review_core::project::{
    MAX_SURVEY_BYTES, MAX_SURVEY_TEXT_BYTES, PROJECT_SURVEY_V2, ProjectSurveyV2,
    SurveyCollectedPayloadV1, SurveyCompletenessV1, SurveyFailedPayloadV1, SurveyRecordedPayloadV1,
};
use review_core::task::usage::DecimalU64;
use review_store::NewEvent;
use serde_json::Value;

use policy::Policy;

use super::{Checkout, Git, Log, ProjectStore, RUN_ID, Unopened, Wait};

/// The Surveyor's measurement rules. Bumped whenever a rule changes, so a head measured under
/// the old rules is surveyed again. 2: the survey key's refs, the fixed maxima, one pattern
/// grammar, the stated measurement policy and the agent-assisted partition. 3: the deadline
/// through every section's post-processing and the key, contributor keys and ranking before
/// every cut, the status-line grammar, the checks discovery set, language patterns, UTF-8
/// declarations and the bounded walk and records. 4: one ref observation for the key and the
/// statistics, history from objects alone, the digest-only Tree Digest, calendar dates, the
/// clock through every finishing step and every declaration, and name hashing of blank names
/// under both identity policies. 5: the blank-or-control check over the whole author name
/// before any cut, so a control character past 256 bytes hashes the name. 6: the log read in
/// NUL-separated fields with the commit ID repeated, so author data never splits a record and a
/// record that does not parse stops the walk with `malformed_record`, and the stopping point of
/// a Survey that is not complete. 7: a log that ends inside a record (its last item without its
/// NUL) is a malformed record and never whole history, and a walk stopped in its first record
/// leaves `history` and `contributors` partial with zero counts, naming the commit. 8: the
/// record that stopped the walk named by its ordinal in `history` and `contributors`.
pub(crate) const SURVEYOR_VERSION: u64 = 8;

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
pub(crate) fn record_bytes(survey: &ProjectSurveyV2) -> Result<Vec<u8>, String> {
    survey.validate()?;
    canonical_bytes(&serde_json::to_value(survey).map_err(|error| error.to_string())?)
}

/// One Survey as the log and the CAS hold it.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    pub(crate) event: SurveyRecordedPayloadV1,
    pub(crate) survey: ProjectSurveyV2,
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
pub(crate) fn unbound_field(
    event: &SurveyRecordedPayloadV1,
    survey: &ProjectSurveyV2,
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
/// identity at collection disagree: every field [`unbound_field`] compares with the record.
fn untombstoned_field(
    event: &SurveyRecordedPayloadV1,
    tombstone: &SurveyCollectedPayloadV1,
) -> Option<&'static str> {
    [
        ("head", event.head == tombstone.head),
        ("refs_digest", event.refs_digest == tombstone.refs_digest),
        (
            "cutoff_unix_s",
            event.cutoff_unix_s == tombstone.cutoff_unix_s,
        ),
        ("completeness", event.completeness == tombstone.completeness),
        (
            "policy_digest",
            event.policy_digest == tombstone.policy_digest,
        ),
        (
            "surveyor_version",
            event.surveyor_version == tombstone.surveyor_version,
        ),
        (
            "previous_survey_id",
            event.previous_survey_id == tombstone.previous_survey_id,
        ),
    ]
    .into_iter()
    .find_map(|(field, equal)| (!equal).then_some(field))
}

/// The identity a `SurveyCollected@1` binds (ADR-0158 clauses 2 and 13): what replay compares a
/// collected Survey's event with once its record is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TombstoneIdentity {
    pub(crate) head: String,
    pub(crate) cutoff_unix_s: DecimalU64,
    pub(crate) completeness: SurveyCompletenessV1,
    pub(crate) refs_digest: String,
    pub(crate) policy_digest: String,
    pub(crate) surveyor_version: DecimalU64,
    pub(crate) previous_survey_id: Option<String>,
}

impl TombstoneIdentity {
    /// The identity of the record `bytes` hold, copied from the record after `recorded` was
    /// verified against it, never from the event: `Ok(Err(field))` names the first field in
    /// which the event disagrees with its record, which collection refuses, and `Err` says why
    /// the bytes are no valid Survey, in af's own words of at most [`MAX_SURVEY_TEXT_BYTES`].
    pub(crate) fn of_record(
        recorded: &SurveyRecordedPayloadV1,
        bytes: &[u8],
    ) -> Result<Result<Self, &'static str>, String> {
        let survey = parse_record(bytes).map_err(|why| cut_text(&why.to_string()))?;
        if let Some(field) = unbound_field(recorded, &survey) {
            return Ok(Err(field));
        }
        let Some(identity) = survey.identity.body else {
            return Ok(Err("head"));
        };
        Ok(Ok(Self {
            head: identity.head,
            cutoff_unix_s: identity.cutoff_unix_s,
            completeness: survey.completeness,
            refs_digest: identity.refs_digest,
            policy_digest: identity.policy_digest,
            surveyor_version: identity.surveyor_version,
            previous_survey_id: survey.previous_survey_id,
        }))
    }

    /// The fields of `recorded` as collection read them, for a record that vouches for nothing
    /// (its bytes are no Survey, or it is gone): its tombstone says `unreadable`.
    pub(crate) fn of_event(recorded: &SurveyRecordedPayloadV1) -> Self {
        Self {
            head: recorded.head.clone(),
            cutoff_unix_s: recorded.cutoff_unix_s,
            completeness: recorded.completeness,
            refs_digest: recorded.refs_digest.clone(),
            policy_digest: recorded.policy_digest.clone(),
            surveyor_version: recorded.surveyor_version,
            previous_survey_id: recorded.previous_survey_id.clone(),
        }
    }
}

/// `text` cut at a character boundary to [`MAX_SURVEY_TEXT_BYTES`].
fn cut_text(text: &str) -> String {
    let mut end = text.len().min(MAX_SURVEY_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Why a Survey's record was not read back as one of this release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadError {
    /// A record of the superseded `af/ProjectSurvey@1`, which another af release wrote: refused
    /// by its label, never decoded and never deleted for it (ADR-0145).
    Superseded,
    /// Gone, unreadable, corrupt or no valid Survey: af's own words.
    Other(String),
}

/// How `af project history` lists a Survey whose record is of a superseded version.
pub(crate) const SUPERSEDED_LINE: &str = "unreadable: superseded version";

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Superseded => write!(
                f,
                "it is af/ProjectSurvey@1, a superseded version another af release wrote; run \
                 `af project refresh` to record a Survey of this release"
            ),
            ReadError::Other(error) => f.write_str(error),
        }
    }
}

/// A Survey record's bytes parsed and validated. A parse error is said by its category and
/// place, never with the bytes it met; a record of the superseded `@1` is [`ReadError::Superseded`].
fn parse_record(bytes: &[u8]) -> Result<ProjectSurveyV2, ReadError> {
    if review_core::project::is_superseded_survey(bytes) {
        return Err(ReadError::Superseded);
    }
    parse_current(bytes).map_err(ReadError::Other)
}

fn parse_current(bytes: &[u8]) -> Result<ProjectSurveyV2, String> {
    let survey: ProjectSurveyV2 = serde_json::from_slice(bytes).map_err(|error| {
        let category = match error.classify() {
            serde_json::error::Category::Io => "a read",
            serde_json::error::Category::Syntax => "a syntax",
            serde_json::error::Category::Data => "a data",
            serde_json::error::Category::Eof => "an end-of-input",
        };
        format!(
            "its bytes are not a Survey record: {category} error at line {}, column {}",
            error.line(),
            error.column()
        )
    })?;
    survey
        .validate()
        .map_err(|error| format!("its bytes are not a valid Survey record: {error}"))?;
    Ok(survey)
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

/// A `SurveyRecorded@1` of an uncollected Survey whose record the Project Store cannot hand
/// over (gone, unreadable, corrupt): nothing vouches for the event, so replay refuses it, naming
/// its sequence, its Survey and the read error, and none of its other fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unreadable {
    pub(crate) sequence: u64,
    pub(crate) survey_id: String,
    pub(crate) error: String,
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SurveyRecorded@1 at sequence {} names Survey {}, whose record cannot be read: {}",
            self.sequence, self.survey_id, self.error
        )
    }
}

impl ProjectStore {
    /// The record of Survey `survey_id`, read back from the CAS and validated, never more than a
    /// Survey can hold: the ceiling the writer and collection share. The error is af's own: the
    /// record is gone, its bytes do not match its digest, it cannot be read, or its bytes are no
    /// valid Survey.
    fn read_survey(&self, survey_id: &str) -> Result<(ProjectSurveyV2, Vec<u8>), ReadError> {
        let bytes = self
            .cas
            .get_bounded(survey_id, MAX_SURVEY_BYTES)
            .map_err(|error| {
                ReadError::Other(match error {
                    review_store::CasError::NotFound { .. } => "it is not in the CAS".to_string(),
                    review_store::CasError::Corrupt { .. } => {
                        "its bytes do not match its digest".to_string()
                    }
                    other => other.to_string(),
                })
            })?;
        Ok((parse_record(&bytes)?, bytes))
    }

    /// Whether the record `recorded` names is of the superseded `af/ProjectSurvey@1`: kept, never
    /// reused, listed by `af project history` as [`SUPERSEDED_LINE`].
    pub(crate) fn superseded(&self, recorded: &SurveyRecordedPayloadV1) -> bool {
        matches!(
            self.read_survey(&recorded.survey_id),
            Err(ReadError::Superseded)
        )
    }

    fn does_not_replay(&self, refusal: &dyn std::fmt::Display) -> String {
        format!(
            "the Project Store {} does not replay: {refusal}",
            self.directory.display()
        )
    }

    /// The record `recorded` names, read back from the CAS, validated, and bound to its event:
    /// a record the CAS cannot hand over is a replay error naming the event's sequence, its
    /// Survey and the read error, and an event whose head, refs, cutoff, completeness, policy,
    /// Surveyor version or previous Survey differs from its record's is one naming the event's
    /// sequence and the field. Every reader of a record (`af project survey`, `af project show`,
    /// the reuse of the survey key, a refresh's newest Survey, and after them the post-run
    /// preflight and the Brief) reads it here.
    pub(crate) fn survey(&self, recorded: &SurveyRecordedPayloadV1) -> Result<Recorded, String> {
        let (survey, bytes) = match self.read_survey(&recorded.survey_id) {
            Ok(read) => read,
            Err(error) => {
                return Err(self.does_not_replay(&Unreadable {
                    sequence: self.recorded_sequence(recorded)?,
                    survey_id: recorded.survey_id.clone(),
                    error: error.to_string(),
                }));
            }
        };
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

    /// The tombstone of the Survey `recorded` names, when it was collected, bound to the event
    /// before any of the event's fields is used: an event that differs from its tombstone on any
    /// identity field it binds is a replay error naming the event's sequence and the field.
    pub(crate) fn collected(
        &self,
        recorded: &SurveyRecordedPayloadV1,
    ) -> Result<Option<SurveyCollectedPayloadV1>, String> {
        let Some((at, tombstone)) = self.tombstones()?.remove(&recorded.survey_id) else {
            return Ok(None);
        };
        if let Some(field) = untombstoned_field(recorded, &tombstone) {
            let unbound = Unbound {
                sequence: self.recorded_sequence(recorded)?,
                field,
                against: format!("tombstone SurveyCollected@1 at sequence {at}"),
            };
            return Err(self.does_not_replay(&unbound));
        }
        Ok(Some(tombstone))
    }

    /// The first tombstone of each collected Survey, with its sequence.
    fn tombstones(
        &self,
    ) -> Result<std::collections::BTreeMap<String, (u64, SurveyCollectedPayloadV1)>, String> {
        let mut tombstones = std::collections::BTreeMap::new();
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
        Ok(tombstones)
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
    /// prints any of it: a collected Survey's event to the tombstone that bound its record's
    /// identity at collection, every other one to its record, and the first that disagrees, or
    /// whose record the CAS cannot hand over (gone, unreadable, corrupt), is the replay error,
    /// so no event's fields reach the history unverified. The fold refuses before it reads.
    ///
    /// A record of the superseded `af/ProjectSurvey@1` is no refusal: nothing of this release can
    /// verify its event, so the history prints none of the event's fields but its Survey ID and
    /// when it was recorded, marked [`SUPERSEDED_LINE`]. Returns those Surveys.
    pub(crate) fn verify_chain(&self) -> Result<std::collections::BTreeSet<String>, String> {
        let mut superseded = std::collections::BTreeSet::new();
        let tombstones = self.tombstones()?;
        for (sequence, recorded) in self.recorded_events()?.into_iter().rev() {
            let refusal: Option<Box<dyn std::fmt::Display>> = match tombstones
                .get(&recorded.survey_id)
            {
                Some((at, tombstone)) => untombstoned_field(&recorded, tombstone).map(|field| {
                    Box::new(Unbound {
                        sequence,
                        field,
                        against: format!("tombstone SurveyCollected@1 at sequence {at}"),
                    }) as Box<dyn std::fmt::Display>
                }),
                None => match self.read_survey(&recorded.survey_id) {
                    Ok((survey, _)) => unbound_field(&recorded, &survey).map(|field| {
                        Box::new(Unbound {
                            sequence,
                            field,
                            against: format!("Survey {}", recorded.survey_id),
                        }) as Box<dyn std::fmt::Display>
                    }),
                    Err(ReadError::Superseded) => {
                        superseded.insert(recorded.survey_id.clone());
                        None
                    }
                    Err(error) => Some(Box::new(Unreadable {
                        sequence,
                        survey_id: recorded.survey_id.clone(),
                        error: error.to_string(),
                    })),
                },
            };
            if let Some(refusal) = refusal {
                return Err(self.does_not_replay(&*refusal));
            }
        }
        Ok(superseded)
    }

    /// Publish `survey`, whose canonical bytes are `bytes`, under the Project Store's lock, in
    /// the order collection's reader relies on (`pending.rs`): its pending-publication entry
    /// written and synced first, then its record staged in the writer's own `pending-surveys`
    /// and made durable there, then the staged object's identity recorded and synced in the
    /// entry, then that object renamed to its CAS name, then `SurveyRecorded@1`, and the entry
    /// cleared only once that committed, so a crash anywhere between them leaves an entry, or
    /// staged bytes, that collection settles. An entry a crash left for these same bytes is
    /// this publication's, which finishes it.
    fn record_survey(
        &mut self,
        survey: &ProjectSurveyV2,
        bytes: Vec<u8>,
    ) -> Result<Recorded, String> {
        let survey_id = review_store::canonical::blob_content_id(&bytes);
        let length = bytes.len() as u64;
        // An entry an earlier attempt of these bytes left keeps the object it names.
        let kept = pending::begin(&self.directory, &survey_id, length)?;
        pending::fail_put()?;
        pending::publish(&self.directory, &survey_id, &bytes, kept)?;
        pending::stop_before_event();
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
            .referencing(vec![survey_id.clone()]),
        )?;
        // The event committed: the record is published. An entry that cannot be removed names
        // a logged Survey, which its reader tells apart by the log.
        let _ = pending::clear(&self.directory, &survey_id);
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

/// The registered Project's Store, its lock taken by `deadline` (at least one try): a holder
/// that keeps it past the Survey's deadline never holds a refresh beyond it.
fn open(checkout: &Checkout, deadline: Instant) -> Result<ProjectStore, Unopened> {
    let store = ProjectStore::open_waiting(
        &crate::xdg_state_root()?,
        &checkout.project_id,
        Wait::Until(deadline),
    )?;
    if store.log()?.project.is_none() {
        return Err(Unopened::Failed(
            "the Project is not registered; run `af project show` first".into(),
        ));
    }
    Ok(store)
}

/// Why a refresh whose Project Store's lock another process held until its deadline recorded
/// no Survey: the bounded failure `store_locked`.
const STORE_LOCKED: &str = "the Project Store is locked (store_locked): another af process held \
                            its lock until max_wall_ms passed; run `af project refresh` again \
                            once it ends";

/// Append `SurveyFailed@1` to the log of the registered Project Store at `directory` without
/// its lock, which another process holds: what a refresh that could not take it in time
/// records. The log orders its own appends; the event names no CAS object and changes nothing
/// the holder reads or writes.
fn record_failure_unlocked(directory: &Path, head: &str, reason: &str) -> Result<(), String> {
    let payload = SurveyFailedPayloadV1 {
        head: head.to_string(),
        reason: public_reason(reason),
        recorded_unix_ms: crate::storage::now_unix_ms().into(),
    };
    payload.validate()?;
    let mut events = review_store::EventStore::open(directory.join("events.sqlite"))
        .map_err(|error| error.to_string())?;
    let registered = Log::fold(&events.replay(RUN_ID).map_err(|error| error.to_string())?)?
        .project
        .is_some();
    if !registered {
        return Err("the Project is not registered".into());
    }
    let cas = review_store::Cas::open(directory.join("cas")).map_err(|error| error.to_string())?;
    let event = NewEvent::new(
        EventType::SurveyFailedV1,
        serde_json::to_value(&payload).map_err(|error| error.to_string())?,
    )
    .at(review_core::task::collection::collected_time(
        crate::storage::now_unix_ms(),
    ));
    events
        .append(RUN_ID, &cas, event)
        .map(|_| ())
        .map_err(|error| format!("recording SurveyFailed@1: {error}"))
}

/// Why a refresh recorded no Survey: what its `SurveyFailed@1` says and what the command
/// prints.
#[derive(Debug)]
struct Unrecorded {
    /// The head the attempt surveyed, once `rev-parse` named it.
    head: Option<String>,
    /// af's own sentence for `SurveyFailed@1`: no path, no email.
    reason: String,
    /// What the command prints, which may say more.
    error: String,
}

impl Unrecorded {
    fn new(head: Option<&str>, reason: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            head: head.map(str::to_string),
            reason: reason.into(),
            error: error.into(),
        }
    }

    /// A failure whose reason is also what the command prints.
    fn said(head: &str, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self::new(Some(head), reason.clone(), reason)
    }

    /// A measurement's failure: its reason, and its detail for the command alone.
    fn of(head: &str, failure: &measure::Failure) -> Self {
        Self::new(
            Some(head),
            failure.reason.clone(),
            format!("{}: {}", failure.reason, failure.detail),
        )
    }
}

/// Why a refresh whose newest Survey does not replay recorded none: the refusal itself names
/// sequences and the Project Store's path, which a `SurveyFailed@1` never holds.
const REPLAY_REFUSED: &str = "the Project Store's newest Survey does not replay against its \
                              record; run `af project history` to see why, or `af project \
                              refresh --force` to record a new one";

/// Why a refresh whose record could not be published recorded none.
const NOT_PUBLISHED: &str =
    "the Survey could not be published in the Project Store; run `af project refresh` again";

/// How long the one `rev-parse` may take that names the head of a failure whose Survey clock
/// ran out before its own `rev-parse` answered.
const FAILURE_HEAD_MS: u64 = 1_000;

/// Record a Survey of the checkout's head now, synchronously, unless the newest Survey is
/// complete under the current survey key and `force` is not asked. One `max_wall_ms` deadline
/// runs from before `rev-parse` to the last section: a deadline that passes before the key
/// exists records `SurveyFailed@1`. A Survey that cannot be taken, or whose record is over
/// `max_survey_bytes`, records `SurveyFailed@1` and is the error. Every way a refresh of a
/// registered Project stops without a Survey records `SurveyFailed@1` naming the head, so the
/// Project Store always holds a record of the attempt; the one exception is a head no
/// `rev-parse` names, even within [`FAILURE_HEAD_MS`] of its own after the deadline, since the
/// event names a head.
///
/// With `register`, the checkout is first registered as `af project` registers it, the Project
/// Store's lock tried for at most `max_wall_ms` before the Survey's clock starts. A holder that
/// keeps the lock past either deadline (the registration's, or the Survey's at either of its
/// two openings) stops the refresh with the bounded failure `store_locked`, which is recorded
/// without the lock.
pub(crate) fn refresh(
    checkout: &Checkout,
    force: bool,
    register: bool,
) -> Result<Refreshed, String> {
    let git = Git::open(&checkout.toplevel)?;
    let attempted = if register {
        // Registration is no part of the Survey, but its wait for the lock is bounded the
        // same way; the Survey's clock starts once it is done.
        let max_wall_ms = Policy::load(&checkout.toplevel)
            .map_or(Policy::default().max_wall_ms, |policy| policy.max_wall_ms);
        let deadline = Instant::now() + std::time::Duration::from_millis(max_wall_ms);
        // Released before the head is measured: the Surveyor opens the Store again.
        let registered = super::adopt_waiting(checkout, None, Wait::Until(deadline)).map(drop);
        match registered {
            Ok(()) => attempt(checkout, &git, Instant::now(), force),
            Err(Unopened::Locked) => Err(Unrecorded::new(None, STORE_LOCKED, STORE_LOCKED)),
            Err(Unopened::Failed(error)) => return Err(error),
        }
    } else {
        attempt(checkout, &git, Instant::now(), force)
    };
    let unrecorded = match attempted {
        Ok(refreshed) => return Ok(refreshed),
        Err(unrecorded) => unrecorded,
    };
    let head = unrecorded.head.clone().or_else(|| {
        let clock = measure::Clock::new(Instant::now(), FAILURE_HEAD_MS).section("key");
        measure::head(&git, &clock).ok().flatten()
    });
    // Only a registered Project's log holds it: a Project Store is never created for it.
    let logged = crate::xdg_state_root().is_ok_and(|state_home| {
        super::store_directory(&state_home, &checkout.project_id)
            .join("events.sqlite")
            .is_file()
    });
    // One try for the lock: a holder that keeps it gets the failure through the log alone.
    let recorded = match (head, logged) {
        (Some(head), true) => match open(checkout, Instant::now()) {
            Ok(mut store) => store.record_failure(&head, &unrecorded.reason),
            Err(Unopened::Locked) => crate::xdg_state_root().and_then(|state_home| {
                record_failure_unlocked(
                    &super::store_directory(&state_home, &checkout.project_id),
                    &head,
                    &unrecorded.reason,
                )
            }),
            Err(Unopened::Failed(_)) => Ok(()),
        },
        _ => Ok(()),
    };
    if let Err(error) = recorded {
        return Err(format!(
            "{}; recording SurveyFailed@1 failed too: {error}",
            unrecorded.error
        ));
    }
    Err(unrecorded.error)
}

/// One refresh under the Survey's clock from `started`: the recorded or reused Survey, or why
/// none was recorded, which [`refresh`] records.
fn attempt(
    checkout: &Checkout,
    git: &Git,
    started: Instant,
    force: bool,
) -> Result<Refreshed, Unrecorded> {
    let policy = Policy::load(&checkout.toplevel).and_then(within_pattern_limits);
    let clock = measure::Clock::new(
        started,
        policy
            .as_ref()
            .map_or(Policy::default().max_wall_ms, |policy| policy.max_wall_ms),
    );
    // The spills of a refresh killed after it spilled, and only those whose process is gone:
    // swept by every refresh before it decides whether to reuse a Survey, within its deadline,
    // and again after its Tree Digest (`measure::tree_digest`).
    review_sandbox::stale::sweep_stale_tree_digests_before(&std::env::temp_dir(), clock.deadline());
    // The key's reads are no section's: a fixture that slows one section leaves them alone.
    let key_clock = clock.section("key");
    let head = match measure::head(git, &key_clock) {
        Ok(Some(head)) => head,
        Ok(None) => {
            return Err(Unrecorded::new(
                None,
                KEY_DEADLINE,
                format!("{KEY_DEADLINE}; no head was read in time"),
            ));
        }
        Err(failure) => {
            return Err(Unrecorded::new(
                None,
                failure.reason.clone(),
                format!("{}: {}", failure.reason, failure.detail),
            ));
        }
    };
    let policy = policy.map_err(|reason| {
        Unrecorded::new(
            Some(&head),
            reason.clone(),
            format!("{reason}; fix the [survey] table and run `af project refresh` again"),
        )
    })?;
    // One ref observation: the key's `refs_digest` and `history`'s tags and branches.
    let observed = match measure::observe_refs(git, &key_clock, &policy) {
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
            return Err(Unrecorded::said(&head, reason));
        }
        Err(failure) => return Err(Unrecorded::of(&head, &failure)),
    };
    let refs_digest = observed.digest.clone();
    // A Project Store that cannot be opened (an unregistered Project) holds no failure either;
    // one whose lock another process holds until the deadline records `store_locked`.
    let unopened = |unopened: Unopened| match unopened {
        Unopened::Locked => Unrecorded::said(&head, STORE_LOCKED),
        Unopened::Failed(error) => Unrecorded::new(None, error.clone(), error),
    };
    let replayed = |error: String| Unrecorded::new(Some(&head), REPLAY_REFUSED, error);
    let store = open(checkout, clock.deadline()).map_err(unopened)?;
    let key = Key {
        head: head.clone(),
        refs_digest: refs_digest.clone(),
        policy_digest: policy
            .digest()
            .map_err(|error| Unrecorded::said(&head, error))?,
        surveyor_version: SURVEYOR_VERSION,
    };
    if !force
        && let Some(newest) = reusable(&store, &store.log().map_err(replayed)?).map_err(replayed)?
        && key.reuses(&newest)
    {
        return Ok(Refreshed::Unchanged(newest));
    }
    // The Store is not held while the head is measured.
    drop(store);
    let git_version = match measure::git_version(git, &key_clock) {
        Ok(Some(version)) => version,
        Ok(None) => return Err(Unrecorded::said(&head, KEY_DEADLINE)),
        Err(failure) => {
            let reason = "git --version failed; run `af project refresh` again";
            return Err(Unrecorded::new(
                Some(&head),
                reason,
                format!("{reason}: {}", failure.detail),
            ));
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
    let sections = collect::measure(git, &basis, started, clock.clone())
        .map_err(|failure| Unrecorded::of(&head, &failure))?;
    let mut store = open(checkout, clock.deadline()).map_err(unopened)?;
    let log = store.log().map_err(replayed)?;
    if !force
        && let Some(newest) = reusable(&store, &log).map_err(replayed)?
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
    let mut survey = ProjectSurveyV2 {
        schema: PROJECT_SURVEY_V2.into(),
        completeness,
        previous_survey_id: log.newest_survey().map(|newest| newest.survey_id.clone()),
        stopped: None,
        identity: sections.identity,
        tree: sections.tree,
        history: sections.history,
        contributors: sections.contributors,
        decisions: sections.decisions,
        vocabulary: sections.vocabulary,
        checks: sections.checks,
    };
    survey.stopped = stopped(&survey, &clock);
    let bytes = record_bytes(&survey).map_err(|error| {
        Unrecorded::new(
            Some(&head),
            "the Survey measured is no valid record; run `af project refresh` again",
            format!("the Survey measured is no valid record: {error}"),
        )
    })?;
    if bytes.len() as u64 > policy.max_survey_bytes.min(MAX_SURVEY_BYTES) {
        let reason = format!(
            "the Survey needs {} bytes, more than [survey] max_survey_bytes = {}; raise it to at \
             most {}, exclude paths or lower max_entries, and run `af project refresh` again",
            bytes.len(),
            policy.max_survey_bytes,
            review_core::project::MAX_SURVEY_BYTES
        );
        return Err(Unrecorded::said(&head, reason));
    }
    store
        .record_survey(&survey, bytes)
        .map(Refreshed::Recorded)
        .map_err(|error| Unrecorded::new(Some(&head), NOT_PUBLISHED, error))
}

/// Where `survey` stopped when it is not complete (ADR-0158 clause 1): the stopping point its
/// sections determine, with the items the Clock charged in that section.
fn stopped(
    survey: &ProjectSurveyV2,
    clock: &measure::Clock,
) -> Option<review_core::project::SurveyStopV1> {
    let mut stop = survey.stopping_point()?;
    let section = SECTION_NAMES
        .into_iter()
        .find(|name| *name == stop.section)
        .unwrap_or_default();
    stop.item = clock.charged(section).into();
    Some(stop)
}

/// The newest recorded Survey of the Project while it is uncollected (ADR-0158 clause 13): a
/// newest Survey a tombstone names is none, once its event is bound to that tombstone, and an
/// uncollected one is read through [`ProjectStore::survey`], so no field of the newest event is
/// used before it is verified.
pub(crate) fn newest(store: &ProjectStore, log: &Log) -> Result<Option<Recorded>, String> {
    let Some(newest) = log.newest_survey() else {
        return Ok(None);
    };
    if store.collected(newest)?.is_some() {
        return Ok(None);
    }
    store.survey(newest).map(Some)
}

/// The newest Survey a refresh may reuse: [`newest`], but none when its record is of a
/// superseded version, which is kept and measured again, never reused.
fn reusable(store: &ProjectStore, log: &Log) -> Result<Option<Recorded>, String> {
    if let Some(newest) = log.newest_survey()
        && store.collected(newest)?.is_none()
        && store.superseded(newest)
    {
        return Ok(None);
    }
    newest(store, log)
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

/// `  stopped       <section> at item <n>[, commit <12 hex>][, <path>] (<reason>)`.
fn stop_line(stop: &review_core::project::SurveyStopV1) -> String {
    let mut at = format!("{} at item {}", stop.section, stop.item.get());
    if let Some(commit) = &stop.commit {
        at.push_str(&format!(", commit {}", short(commit)));
    }
    if let Some(path) = &stop.path {
        at.push_str(&format!(", {path}"));
    }
    format!("  stopped       {at} ({})", stop.reason.as_str())
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
    if let Some(stop) = &survey.stopped {
        lines.push(stop_line(stop));
    }
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
            let mut measured = format!(
                "{} commits ({} merges), {} in 30 days, {} in 90 days",
                history.commits.get(),
                history.merges.get(),
                history.windows.days_30.commits.get(),
                history.windows.days_90.commits.get()
            );
            if let (Some(commit), Some(record)) = (&history.stopped_in, history.stopped_record) {
                measured.push_str(&format!(
                    ", stopped in record {} (commit {})",
                    record.get(),
                    short(commit)
                ));
            }
            measured
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
    match refresh(&checkout, false, false) {
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
        ProjectCommand::History { json } => history(json),
        ProjectCommand::Survey { json: false } if !sections.is_empty() => survey_sections(sections),
        ProjectCommand::Survey { json } => {
            // A collected newest Survey's event is bound to its tombstone before `collect::run`
            // names its head.
            if let Some(store) = existing_store_here()?
                && let Some(newest) = store.log()?.newest_survey()
            {
                store.collected(newest)?;
            }
            collect::run(ProjectCommand::Survey { json })
        }
        other => collect::run(other),
    }
}

/// `af project history`: every Survey event of the chain verified against its record or its
/// tombstone before any of it is printed, a Survey whose record is of a superseded version
/// listed as [`SUPERSEDED_LINE`] with none of its event's other fields.
pub(crate) fn history(json: bool) -> Result<i32, String> {
    let superseded = match existing_store_here()? {
        Some(store) => store.verify_chain()?,
        None => std::collections::BTreeSet::new(),
    };
    collect::history(json, &superseded)
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
        && let Some(tombstone) = store.collected(newest)?
    {
        return Err(format!(
            "the newest Survey {} of {} was collected at {}; run `af project refresh` to \
             record one",
            tombstone.survey_id,
            short(&tombstone.head),
            review_core::task::collection::collected_time(tombstone.collected_unix_ms.get())
        ));
    }
    let recorded = newest(&store, &log)?.ok_or(none)?;
    for line in summary_of(&recorded, sections) {
        println!("{line}");
    }
    Ok(0)
}

/// The pattern limits of ADR-0158 clause 14, where the Surveyor loads its patterns: every
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
