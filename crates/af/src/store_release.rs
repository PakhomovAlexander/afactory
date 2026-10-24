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

/// The widest line [`summary`] writes, so a caller that indents it by two still fits 100
/// columns.
const SUMMARY_WIDTH: usize = 96;

/// One row for every Store another release wrote: how many, by which releases, and how to run
/// them. `command` is the command to rerun under the release, as `AF_VERSION=… {command}`.
/// Stores of one release are one line with that release's command; Stores of several releases
/// (an unmarked one counts as its own) are a line each, so no release is recommended for a
/// Store it did not write. Lines are separated by `\n`; a mixed summary's lines fit in
/// [`SUMMARY_WIDTH`] columns.
pub(crate) fn summary<'a>(
    provenances: impl IntoIterator<Item = &'a StoreProvenance>,
    command: &str,
) -> Option<String> {
    let mut by_release: BTreeMap<String, usize> = BTreeMap::new();
    let mut unmarked = 0usize;
    let mut total = 0usize;
    for provenance in provenances {
        total += 1;
        match provenance.writer() {
            Some(release) => *by_release.entry(release.to_string()).or_default() += 1,
            None => unmarked += 1,
        }
    }
    if total == 0 {
        return None;
    }
    let mut releases: Vec<(String, usize)> = by_release.into_iter().collect();
    releases.sort_by(|(left, _), (right, _)| semver_key(right).cmp(&semver_key(left)));
    let groups = releases.len() + usize::from(unmarked > 0);
    let (noun, them) = if total == 1 {
        ("Store written by another release", "it")
    } else {
        ("Stores written by other releases", "them")
    };
    let finish = |them: &str| {
        format!(
            "finish {them} under the release that started {them}, or remove {them} with af \
             storage prune --apply once idle"
        )
    };
    if groups == 1 {
        let (what, fix) = match releases.first() {
            Some((release, count)) => (
                format!("af {release} ({count})"),
                format!("run {them} with AF_VERSION={release} {command}"),
            ),
            None => (
                format!("an older af release with no version marker ({unmarked})"),
                finish(them),
            ),
        };
        return Some(format!("{total} {noun}: {what}; {fix}"));
    }
    let stores = |count: usize| {
        if count == 1 {
            "1 Store".to_owned()
        } else {
            format!("{count} Stores")
        }
    };
    let mut lines = vec![format!(
        "{total} {noun}; run each under the release that wrote it:"
    )];
    for (release, count) in &releases {
        lines.extend(wrap(
            &format!("AF_VERSION={release} {command} ({})", stores(*count)),
            "  ",
        ));
    }
    if unmarked > 0 {
        let them = if unmarked == 1 { "it" } else { "them" };
        lines.extend(wrap(
            &format!(
                "{} by an older af release with no version marker: finish {them} under {} own \
                 release, or remove {them} with af storage prune --apply once idle",
                stores(unmarked),
                if unmarked == 1 { "its" } else { "their" }
            ),
            "  ",
        ));
    }
    Some(lines.join("\n"))
}

/// `text` in lines of at most [`SUMMARY_WIDTH`] columns, broken between words, the first
/// indented by `indent` and the rest by two more.
fn wrap(text: &str, indent: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = indent.to_owned();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let fresh = line.trim().is_empty();
        if !fresh && line.chars().count() + 1 + word.chars().count() > SUMMARY_WIDTH {
            lines.push(std::mem::replace(&mut line, format!("{indent}  ")));
        }
        if !line.trim().is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines
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
    fn stores_of_one_release_are_one_row_and_of_several_a_command_each() {
        let one = [
            StoreProvenance::OtherReleases(vec!["0.12.0-rc.3".into()]),
            StoreProvenance::OtherReleases(vec!["0.12.0-rc.3".into()]),
        ];
        assert_eq!(
            summary(&one, "af task list").unwrap(),
            "2 Stores written by other releases: af 0.12.0-rc.3 (2); run them with \
             AF_VERSION=0.12.0-rc.3 af task list"
        );
        let unmarked = [StoreProvenance::Unmarked];
        assert_eq!(
            summary(&unmarked, "af storage").unwrap(),
            "1 Store written by another release: an older af release with no version marker \
             (1); finish it under the release that started it, or remove it with af storage \
             prune --apply once idle"
        );
        assert!(summary(&[], "af storage").is_none());
        // Two releases: one command per release, never the newest one for both.
        let mixed = [
            StoreProvenance::OtherReleases(vec!["0.9.0-rc.8".into()]),
            StoreProvenance::OtherReleases(vec!["0.12.0-rc.3".into()]),
            StoreProvenance::OtherReleases(vec!["0.12.0-rc.3".into()]),
            StoreProvenance::Unmarked,
        ];
        let text = summary(&mixed, "af task list --state DIR").unwrap();
        assert_eq!(
            text,
            "4 Stores written by other releases; run each under the release that wrote it:\n  \
             AF_VERSION=0.12.0-rc.3 af task list --state DIR (2 Stores)\n  \
             AF_VERSION=0.9.0-rc.8 af task list --state DIR (1 Store)\n  \
             1 Store by an older af release with no version marker: finish it under its own \
             release, or\n    remove it with af storage prune --apply once idle"
        );
        for line in text.lines() {
            assert!(line.chars().count() <= SUMMARY_WIDTH, "{line}");
        }
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
