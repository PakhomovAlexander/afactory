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
//! Survey is reused while its survey key is unchanged: the head commit, the digest of the refs
//! (`refs/heads` and `refs/tags`, names and targets), the policy digest and the Surveyor version.
//! Then nothing past `rev-parse HEAD`, `for-each-ref`, the policy and the log is read, and
//! nothing is recorded; a `partial` or `unavailable` newest Survey is measured again, and
//! `--force` records regardless of the key.

mod measure;
pub(crate) mod policy;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::time::Instant;

use review_core::EventType;
use review_core::project::{
    PROJECT_SURVEY_V1, ProjectSurveyV1, SurveyCompletenessV1, SurveyFailedPayloadV1,
    SurveyRecordedPayloadV1,
};
use review_store::NewEvent;
use serde_json::Value;

use policy::Policy;

use super::{Checkout, Git, Log, ProjectStore};

/// The Surveyor's measurement rules. Bumped whenever a rule changes, so a head measured under
/// the old rules is surveyed again. 2: the survey key's refs, the fixed maxima, one pattern
/// grammar, the stated measurement policy and the agent-assisted partition.
pub(crate) const SURVEYOR_VERSION: u64 = 2;

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
    /// Whether `recorded` is complete and was measured under this key.
    fn reuses(&self, recorded: &SurveyRecordedPayloadV1) -> bool {
        recorded.completeness == SurveyCompletenessV1::Complete
            && recorded.head == self.head
            && recorded.refs_digest == self.refs_digest
            && recorded.policy_digest == self.policy_digest
            && recorded.surveyor_version.get() == self.surveyor_version
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

impl ProjectStore {
    /// The record `recorded` names, read back from the CAS and checked against the event.
    pub(crate) fn survey(&self, recorded: &SurveyRecordedPayloadV1) -> Result<Recorded, String> {
        let bytes = self.cas.get(&recorded.survey_id).map_err(|error| {
            format!(
                "the Project Store does not hold Survey {}: {error}",
                recorded.survey_id
            )
        })?;
        let survey: ProjectSurveyV1 = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Survey {}: {error}", recorded.survey_id))?;
        survey.validate()?;
        Ok(Recorded {
            event: recorded.clone(),
            survey,
            bytes,
        })
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

/// The head commit of the checkout.
fn head(git: &Git) -> Result<String, String> {
    git.require(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"], None)
}

fn open(checkout: &Checkout) -> Result<ProjectStore, String> {
    let store = ProjectStore::open(&crate::xdg_state_root()?, &checkout.project_id)?;
    if store.log()?.project.is_none() {
        return Err("the Project is not registered; run `af project show` first".into());
    }
    Ok(store)
}

/// Record a Survey of the checkout's head now, synchronously, unless the newest Survey is
/// complete under the current survey key and `force` is not asked. A Survey that cannot be
/// taken, or whose record is over `max_survey_bytes`, records `SurveyFailed@1` and is the error.
pub(crate) fn refresh(checkout: &Checkout, force: bool) -> Result<Refreshed, String> {
    let git = Git::open(&checkout.toplevel)?;
    let head = head(&git)?;
    let refs_digest = measure::refs_digest(&git)?;
    let policy = Policy::load(&checkout.toplevel);
    let mut store = open(checkout)?;
    let policy = match policy {
        Ok(policy) => policy,
        Err(reason) => {
            store.record_failure(&head, &reason)?;
            return Err(format!(
                "{reason}; fix the [survey] table and run `af project refresh` again"
            ));
        }
    };
    let key = Key {
        head: head.clone(),
        refs_digest: refs_digest.clone(),
        policy_digest: policy.digest()?,
        surveyor_version: SURVEYOR_VERSION,
    };
    if !force
        && let Some(newest) = store.log()?.newest_survey()
        && key.reuses(newest)
    {
        return Ok(Refreshed::Unchanged(store.survey(newest)?));
    }
    // The Store is not held while the head is measured.
    drop(store);
    let started = Instant::now();
    let git_version = match git.require(["--version"], None) {
        Ok(version) => version,
        Err(detail) => {
            let reason = "git --version failed; run `af project refresh` again";
            open(checkout)?.record_failure(&head, reason)?;
            return Err(format!("{reason}: {detail}"));
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
    let measured = measure::measure(&git, &basis, started);
    let mut store = open(checkout)?;
    let sections = match measured {
        Ok(sections) => sections,
        Err(failure) => {
            store.record_failure(&head, &failure.reason)?;
            return Err(format!("{}: {}", failure.reason, failure.detail));
        }
    };
    let log = store.log()?;
    let newest = log.newest_survey();
    if !force
        && let Some(newest) = newest
        && key.reuses(newest)
    {
        // Another refresh recorded this head while this one measured it.
        return Ok(Refreshed::Unchanged(store.survey(newest)?));
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
        previous_survey_id: newest.map(|newest| newest.survey_id.clone()),
        identity: sections.identity,
        tree: sections.tree,
        history: sections.history,
        contributors: sections.contributors,
        decisions: sections.decisions,
        vocabulary: sections.vocabulary,
        checks: sections.checks,
    };
    let bytes = record_bytes(&survey)?;
    if bytes.len() as u64 > policy.max_survey_bytes {
        let reason = format!(
            "the Survey needs {} bytes, more than [survey] max_survey_bytes = {}; raise it or \
             exclude paths, and run `af project refresh` again",
            bytes.len(),
            policy.max_survey_bytes
        );
        store.record_failure(&head, &reason)?;
        return Err(reason);
    }
    store.record_survey(&survey, bytes).map(Refreshed::Recorded)
}

/// The newest recorded Survey of the Project, if any.
pub(crate) fn newest(store: &ProjectStore, log: &Log) -> Result<Option<Recorded>, String> {
    log.newest_survey()
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

/// The Survey in a few lines, one per section, every number the record's.
pub(crate) fn summary(recorded: &Recorded) -> Vec<String> {
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
                        .map_or("-", review_core::project::SurveyReasonV1::as_str)
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
