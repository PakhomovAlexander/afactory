//! A Store another af release wrote, named by the release that wrote it.
//!
//! A release replays only what it writes (ADR-0145), and that stays: such a Store is refused.
//! What changes is the refusal. Every Store keeps a marker per release that opened it for
//! writing (`af/store-writer@1`, beside the log), so the refusal names that release and the
//! commands that run it, and a listing that meets many such Stores says so in one row rather
//! than one error each. A Store from before markers is named as an earlier release's.

use std::collections::BTreeMap;
use std::path::Path;

use review_core::store_writer::StoreProvenance;

/// This release, as `af --version` prints it.
pub(crate) const THIS_RELEASE: &str = env!("CARGO_PKG_VERSION");

/// Whether `error` is a replay refusal: the log holds an event type this release does not know,
/// or an event whose payload has another shape than the one this release writes.
pub(crate) fn is_replay_refusal(error: &str) -> bool {
    error.contains(review_core::event::ANOTHER_RELEASE)
        || error.contains("invalid replayed event payload")
}

/// Who wrote the Store at `state` when this release refused to replay it with `error`: another
/// release by its marker, or an earlier one without a marker. `None` when the error is no
/// replay refusal, or this release's own marker is the only one.
pub(crate) fn foreign(state: &Path, error: &str) -> Option<StoreProvenance> {
    if !is_replay_refusal(error) {
        return None;
    }
    match review_store::store::writers::provenance_at(&state.join("events.sqlite")) {
        Ok(StoreProvenance::ThisRelease) => None,
        Ok(provenance) => Some(provenance),
        // A log this release cannot open far enough to read its markers predates them.
        Err(_) => Some(StoreProvenance::Unmarked),
    }
}

/// The command this process runs, as a person retypes it: `af` and its arguments.
pub(crate) fn current_command() -> String {
    let words: Vec<String> = std::env::args().skip(1).collect();
    if words.is_empty() {
        "af".into()
    } else {
        format!("af {}", shell_words::join(&words))
    }
}

/// The writer as a person reads it.
fn writer_words(provenance: &StoreProvenance) -> String {
    match provenance {
        StoreProvenance::OtherReleases(releases) => releases
            .iter()
            .map(|release| format!("af {release}"))
            .collect::<Vec<_>>()
            .join(" and "),
        _ => "an older af release (no version marker)".into(),
    }
}

/// The refusal of one Store another release wrote: what happened, who wrote it, and the fix.
pub(crate) fn refusal(state: &Path, provenance: &StoreProvenance, command: &str) -> String {
    match provenance.writer() {
        Some(release) => format!(
            "Store {} was written by {} (this is af {THIS_RELEASE}); fix: AF_VERSION={release} \
             {command}, or af self install {release}, or af onboard --refresh-lock --af {release}",
            state.display(),
            writer_words(provenance),
        ),
        None => format!(
            "Store {} was written by an older af release (no version marker; this is af \
             {THIS_RELEASE}); fix: finish its work under the release that started it, or remove \
             it with af storage prune --apply once it is idle",
            state.display(),
        ),
    }
}

/// `error` with its refusal named when another release wrote the Store at `state`; otherwise
/// `error` unchanged.
pub(crate) fn explain(state: &Path, error: String) -> String {
    match foreign(state, &error) {
        Some(provenance) => refusal(state, &provenance, &current_command()),
        None => error,
    }
}

/// One row for every Store another release wrote: how many, by which releases, and how to run
/// them. `command` is the command to rerun under the release, as `AF_VERSION=… {command}`.
pub(crate) fn summary<'a>(
    provenances: impl IntoIterator<Item = &'a StoreProvenance>,
    command: &str,
) -> Option<String> {
    let mut by_release: BTreeMap<String, usize> = BTreeMap::new();
    let mut unmarked = 0usize;
    let mut total = 0usize;
    let mut newest: Option<String> = None;
    for provenance in provenances {
        total += 1;
        match provenance.writer() {
            Some(release) => {
                *by_release.entry(release.to_string()).or_default() += 1;
                if newest
                    .as_deref()
                    .is_none_or(|current| semver_key(release) > semver_key(current))
                {
                    newest = Some(release.to_string());
                }
            }
            None => unmarked += 1,
        }
    }
    if total == 0 {
        return None;
    }
    let mut releases: Vec<(String, usize)> = by_release.into_iter().collect();
    releases.sort_by(|(left, _), (right, _)| semver_key(right).cmp(&semver_key(left)));
    let mut parts: Vec<String> = releases
        .iter()
        .map(|(release, count)| format!("af {release} ({count})"))
        .collect();
    if unmarked > 0 {
        parts.push(format!(
            "an older af release with no version marker ({unmarked})"
        ));
    }
    let (noun, them) = if total == 1 {
        ("Store written by another release", "it")
    } else {
        ("Stores written by other releases", "them")
    };
    let fix = match newest {
        Some(release) => format!("run {them} with AF_VERSION={release} {command}"),
        None => format!(
            "finish {them} under the release that started {them}, or remove {them} with af \
             storage prune --apply once idle"
        ),
    };
    Some(format!("{total} {noun}: {}; {fix}", parts.join(", ")))
}

/// A release spelling ordered as a version where it parses as one, after every one that does
/// not.
fn semver_key(release: &str) -> Option<semver::Version> {
    semver::Version::parse(release).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_refusal_names_the_writer_this_release_and_three_fixes() {
        let state = PathBuf::from("/state/task");
        let text = refusal(
            &state,
            &StoreProvenance::OtherReleases(vec!["0.12.0-rc.3".into()]),
            "af task list",
        );
        assert_eq!(
            text,
            format!(
                "Store /state/task was written by af 0.12.0-rc.3 (this is af {THIS_RELEASE}); \
                 fix: AF_VERSION=0.12.0-rc.3 af task list, or af self install 0.12.0-rc.3, or \
                 af onboard --refresh-lock --af 0.12.0-rc.3"
            )
        );
        let older = refusal(&state, &StoreProvenance::Unmarked, "af task list");
        assert!(
            older.contains("was written by an older af release (no version marker"),
            "{older}"
        );
    }

    #[test]
    fn many_stores_are_one_row_by_release() {
        let stores = [
            StoreProvenance::OtherReleases(vec!["0.9.0-rc.8".into()]),
            StoreProvenance::OtherReleases(vec!["0.12.0-rc.3".into()]),
            StoreProvenance::OtherReleases(vec!["0.12.0-rc.3".into()]),
        ];
        assert_eq!(
            summary(&stores, "af task list").unwrap(),
            "3 Stores written by other releases: af 0.12.0-rc.3 (2), af 0.9.0-rc.8 (1); run \
             them with AF_VERSION=0.12.0-rc.3 af task list"
        );
        let one = [StoreProvenance::Unmarked];
        assert_eq!(
            summary(&one, "af storage").unwrap(),
            "1 Store written by another release: an older af release with no version marker \
             (1); finish it under the release that started it, or remove it with af storage \
             prune --apply once idle"
        );
        assert!(summary(&[], "af storage").is_none());
    }

    #[test]
    fn only_replay_refusals_are_explained() {
        assert!(is_replay_refusal(
            "event store conflict: invalid replayed event payload: unknown variant `opened`"
        ));
        assert!(is_replay_refusal(
            "unknown review-kernel event type: TaskTransition@1; this log was written by \
             another af release; start a new Campaign or Task"
        ));
        assert!(!is_replay_refusal("cas io: No such file or directory"));
    }
}
