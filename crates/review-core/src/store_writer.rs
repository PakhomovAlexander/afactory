//! The af release that wrote a Store (`af/store-writer@1`).
//!
//! A release replays only the Stores it writes (ADR-0145), so a Store another release wrote is
//! refused. The refusal can name the fix only if the Store names its writer: every release
//! records one marker per release that opened the Store for writing, beside the event log and
//! outside it, so no replay ever reads it and no release has to know another's events to find
//! it. The marker's release is a column of its own; the record beside it is this versioned
//! shape, which a later release may change under a new version without hiding its release.

use serde::{Deserialize, Serialize};

pub const STORE_WRITER_V1: &str = "af/store-writer@1";

/// The most bytes a release spelling may have.
pub const MAX_RELEASE_BYTES: usize = 64;

/// One release that opened a Store for writing, and when it first did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreWriterV1 {
    pub schema: String,
    /// The release, as `af --version` prints it without the `af ` prefix.
    pub af_version: String,
    /// When this release first opened the Store for writing.
    pub first_unix_ms: u64,
}

impl StoreWriterV1 {
    pub fn new(af_version: &str, first_unix_ms: u64) -> Self {
        Self {
            schema: STORE_WRITER_V1.into(),
            af_version: af_version.into(),
            first_unix_ms,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != STORE_WRITER_V1 {
            return Err(format!("a Store writer marker is {STORE_WRITER_V1}"));
        }
        if !is_release(&self.af_version) {
            return Err("a Store writer marker names a release as af --version prints it".into());
        }
        if self.first_unix_ms > crate::json::SAFE_INTEGER_MAX as u64 {
            return Err("a Store writer marker's time is past the JSON integer range".into());
        }
        Ok(())
    }
}

/// Whether `value` spells a release as `af --version` prints one: 1 to 64 bytes of ASCII
/// letters, digits, `.`, `-` and `+`, starting with a digit. A marker another release wrote is
/// shown only when it passes, so nothing a Store holds reaches a terminal unchecked.
pub fn is_release(value: &str) -> bool {
    (1..=MAX_RELEASE_BYTES).contains(&value.len())
        && value.starts_with(|c: char| c.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

/// Who wrote a Store, judged by the release reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreProvenance {
    /// Only this release's marker: the Store is this release's to replay.
    ThisRelease,
    /// Other releases opened it for writing, newest marker first.
    OtherReleases(Vec<String>),
    /// No marker at all: a release from before markers wrote it.
    Unmarked,
}

impl StoreProvenance {
    /// The provenance of a Store whose markers name `releases`, oldest first, as read by
    /// release `this`.
    pub fn of(releases: &[String], this: &str) -> Self {
        if releases.is_empty() {
            return Self::Unmarked;
        }
        let mut others: Vec<String> = Vec::new();
        for release in releases.iter().rev() {
            if release != this && !others.contains(release) {
                others.push(release.clone());
            }
        }
        if others.is_empty() {
            Self::ThisRelease
        } else {
            Self::OtherReleases(others)
        }
    }

    /// The release the fix names: the newest other release that wrote the Store.
    pub fn writer(&self) -> Option<&str> {
        match self {
            Self::OtherReleases(releases) => releases.first().map(String::as_str),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_names_a_release_and_nothing_else() {
        let marker = StoreWriterV1::new("0.12.0-rc.5", 1_700_000_000_000);
        marker.validate().unwrap();
        for bad in ["", "v1.0", "0.1 0", "0.1\u{1b}[2J", &"1".repeat(65)] {
            assert!(StoreWriterV1::new(bad, 0).validate().is_err(), "{bad:?}");
        }
        let mut other = marker.clone();
        other.schema = "af/store-writer@2".into();
        assert!(other.validate().is_err());
    }

    #[test]
    fn provenance_names_the_newest_other_writer() {
        let this = "0.12.0";
        assert_eq!(StoreProvenance::of(&[], this), StoreProvenance::Unmarked);
        assert_eq!(
            StoreProvenance::of(&[this.into()], this),
            StoreProvenance::ThisRelease
        );
        let read = StoreProvenance::of(&["0.9.0".into(), "0.11.0".into(), this.into()], this);
        assert_eq!(
            read,
            StoreProvenance::OtherReleases(vec!["0.11.0".into(), "0.9.0".into()])
        );
        assert_eq!(read.writer(), Some("0.11.0"));
    }
}
