//! The seven sections of a Survey, measured from the head through git plumbing (ADR-0152).
//!
//! Every git process runs through `projects/git.rs` with what remains of the Survey's one
//! `max_wall_ms` deadline: `cat-file --batch`, `ls-tree`, `rev-list`, `log` and `for-each-ref`,
//! and the committed capture for the Tree Digest. None checks anything out, runs a hook, a
//! filter, a textconv or other project code, or writes inside the checkout. Every stream is read
//! as it arrives, never buffered whole and never a record over [`MAX_SURVEY_RECORD_BYTES`], and
//! every loop over its items (tree entries, blobs, commits, numstat lines, decision files, tags
//! and refs) and every loop after its streams (the tree's aggregation, the ranking of
//! identities, the reading of decisions, terms and checks) checks the Clock once per item: a
//! section that reaches the deadline stops at that item and is `partial` with what it reached
//! (`unavailable` when it reached nothing), every section after it is `unavailable`, and the
//! Survey is still recorded. Any other git failure fails the Survey.
//!
//! The readers and the folds are pure functions of git's bytes, the policy and the cutoff, so a
//! Survey is byte-identical for the same head, refs, policy, cutoff and Surveyor version.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::time::{Duration, Instant};

use review_core::project::{
    MAX_SURVEY_RECORD_BYTES, MAX_SURVEY_TEXT_BYTES, ProjectRegisteredPayloadV1,
    SURVEY_CODE_POLICY_SOURCE, SURVEY_DECISION_STATUSES, SURVEY_EMAIL_HASH_HEX_LEN,
    SURVEY_NAME_HASH_PREFIX, SURVEY_RELEASE_TAGS, SURVEY_UNREADABLE, SurveyAfPinV1, SurveyCheckV1,
    SurveyChecksV1, SurveyChurnV1, SurveyContributorV1, SurveyContributorsV1, SurveyDecisionV1,
    SurveyDecisionsV1, SurveyDirectoryV1, SurveyHistoryV1, SurveyHotPathV1, SurveyIdentityPolicyV1,
    SurveyIdentityV1, SurveyLanguageV1, SurveyNameV1, SurveyReasonV1, SurveySectionV1,
    SurveySeenV1, SurveyTagsV1, SurveyTestsV1, SurveyTreeV1, SurveyVocabularyV1, SurveyWindowV1,
    SurveyWindowsV1, is_email_shaped, is_survey_text,
};
use review_core::task::usage::DecimalU64;
use sha2::{Digest, Sha256};

use super::policy::Policy;
use crate::project::glob_matches;
use crate::projects::Git;

const DAY_S: u64 = 86_400;
/// A blob with a NUL in its first 8 KiB is binary and has no line count.
const BINARY_PROBE_BYTES: usize = 8 * 1024;
/// How much of a git stream is held at once.
const STREAM_BUFFER: usize = 64 * 1024;
/// The toolchain declarations `checks` looks for at the root.
pub(crate) const TOOLCHAIN_FILES: [&str; 8] = [
    "rust-toolchain.toml",
    "rust-toolchain",
    ".tool-versions",
    ".nvmrc",
    ".node-version",
    ".python-version",
    ".ruby-version",
    ".java-version",
];
/// Where a project's code policy declares its checks.
pub(crate) const CODE_POLICY: &str = ".af/code-policy.toml";
/// The pipelines whose `[[checks]]` `checks` lists: every file of the `.af/` layout's
/// `pipelines/` in the head tree, routed or not.
pub(crate) const PIPELINES: &str = ".af/pipelines/*.toml";
/// The CI workflow files `checks` counts.
const WORKFLOWS: [&str; 2] = [".github/workflows/*.yml", ".github/workflows/*.yaml"];
/// A debug build's fixture may make every item of every stream and of every loop after them cost
/// this many microseconds, so a test whose subject is the deadline reaches it inside a section;
/// [`TEST_SLOW_SECTION`] names the one section whose items cost it. Release binaries read
/// neither.
#[cfg(debug_assertions)]
const TEST_SLOW_ITEM_US: &str = "AF_TEST_SURVEY_SLOW_ITEM_US";
#[cfg(debug_assertions)]
const TEST_SLOW_SECTION: &str = "AF_TEST_SURVEY_SLOW_SECTION";
/// The sections, by the names the Clock and [`TEST_SLOW_SECTION`] know them by.
#[cfg(debug_assertions)]
const SECTIONS: [&str; 7] = [
    "identity",
    "tree",
    "history",
    "contributors",
    "decisions",
    "vocabulary",
    "checks",
];

fn decimal(value: u64) -> DecimalU64 {
    value.into()
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// A Survey that could not be taken: af's own sentence, which names no path and no email and
/// goes into `SurveyFailed@1`, and the detail, which only stderr shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub(crate) reason: String,
    pub(crate) detail: String,
}

impl Failure {
    pub(crate) fn new(reason: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            detail: detail.into(),
        }
    }
}

fn unreadable(detail: String) -> Failure {
    Failure::new(
        "git answered in a shape af does not read; run `af project refresh` again",
        detail,
    )
}

/// The Survey's one deadline, from before `rev-parse` to the last section.
#[derive(Debug, Clone, Copy)]
pub(super) struct Clock {
    deadline: Instant,
    slow: Duration,
    /// The one section whose items cost `slow`, when a fixture names one.
    only: Option<&'static str>,
}

impl Clock {
    pub(super) fn new(started: Instant, max_wall_ms: u64) -> Self {
        Self {
            deadline: started + Duration::from_millis(max_wall_ms),
            slow: slow_item(),
            only: slow_section(),
        }
    }

    /// The same deadline, for the items of `section`.
    pub(super) fn section(&self, section: &'static str) -> Self {
        let slow = match self.only {
            Some(only) if only != section => Duration::ZERO,
            _ => self.slow,
        };
        Self { slow, ..*self }
    }

    fn remaining(&self) -> Option<Duration> {
        let now = Instant::now();
        (now < self.deadline).then(|| self.deadline - now)
    }

    fn passed(&self) -> bool {
        self.remaining().is_none()
    }

    /// One item of a stream or of a loop over what it read: whether the deadline passed before
    /// it, where the loop stops.
    pub(super) fn item(&self) -> bool {
        if !self.slow.is_zero() {
            std::thread::sleep(self.slow);
        }
        self.passed()
    }
}

/// What each item costs in a debug build's fixture; nothing otherwise.
fn slow_item() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(micros) = std::env::var(TEST_SLOW_ITEM_US)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Duration::from_micros(micros);
    }
    Duration::ZERO
}

/// The section a debug build's fixture slows alone; none otherwise.
fn slow_section() -> Option<&'static str> {
    #[cfg(debug_assertions)]
    if let Ok(named) = std::env::var(TEST_SLOW_SECTION) {
        return SECTIONS.iter().copied().find(|section| *section == named);
    }
    None
}

/// How a reader of one git stream ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ended {
    /// The stream ended.
    Done,
    /// The deadline passed before an item; the stream stopped there.
    Deadline,
    /// A bound stopped the stream: `max_record_bytes`, or `max_paths` in the history walk.
    Bound(SurveyReasonV1),
    /// git wrote what af does not read.
    Bad(String),
}

/// One record of a stream, ended by `end`, read into `record` without its end.
#[derive(Debug, PartialEq, Eq)]
enum Record {
    /// The stream ended before another record.
    End,
    /// A record; `ended` when its end followed it.
    Read { ended: bool },
    /// The record is longer than [`MAX_SURVEY_RECORD_BYTES`]; nothing more was read.
    TooLong,
}

/// Read one record of at most [`MAX_SURVEY_RECORD_BYTES`] ended by `end`; the last record of a
/// stream may lack its end.
fn read_record(input: &mut dyn BufRead, end: u8, record: &mut Vec<u8>) -> Result<Record, String> {
    record.clear();
    let limit = MAX_SURVEY_RECORD_BYTES as u64 + 1;
    let read = (&mut *input)
        .take(limit)
        .read_until(end, record)
        .map_err(|error| error.to_string())?;
    if read == 0 {
        return Ok(Record::End);
    }
    if record.last() == Some(&end) {
        record.pop();
        Ok(Record::Read { ended: true })
    } else if record.len() as u64 >= limit {
        Ok(Record::TooLong)
    } else {
        Ok(Record::Read { ended: false })
    }
}

/// Plumbing under the clock.
struct Reader<'a> {
    git: &'a Git,
    clock: Clock,
}

impl Reader<'_> {
    fn failed(what: &str, detail: String) -> Failure {
        Failure::new(
            format!("git {what} failed while surveying the head; run `af project refresh` again"),
            detail,
        )
    }

    /// git's one-line stdout, requiring success, under what remains of the deadline; `None`
    /// when the deadline passed.
    fn answer(&self, what: &str, args: &[&str]) -> Result<Option<String>, Failure> {
        let Some(remaining) = self.clock.remaining() else {
            return Ok(None);
        };
        match self.git.run_within(args, None, remaining) {
            Ok(output) if output.status.success() => Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
            )),
            Ok(output) => Err(Self::failed(
                what,
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            )),
            Err(stopped) if stopped.deadline => Ok(None),
            Err(stopped) => Err(Self::failed(what, stopped.message)),
        }
    }

    /// Stream git's stdout through `parse`, with `lines` written to its stdin, under what
    /// remains of the deadline: `None` when the stream ended in time, `max_wall_ms` when the
    /// deadline passed before it ended or right after, or the bound that stopped it.
    fn stream<P>(
        &self,
        what: &str,
        args: &[OsString],
        lines: &[String],
        parse: P,
    ) -> Result<Option<SurveyReasonV1>, Failure>
    where
        P: FnOnce(&mut dyn BufRead) -> Ended + Send,
    {
        let Some(remaining) = self.clock.remaining() else {
            return Ok(Some(SurveyReasonV1::MaxWallMs));
        };
        let streamed = self.git.stream_within(
            args,
            remaining,
            |stdin| {
                for line in lines {
                    stdin.write_all(line.as_bytes())?;
                    stdin.write_all(b"\n")?;
                }
                stdin.flush()
            },
            |stdout| parse(&mut BufReader::with_capacity(STREAM_BUFFER, stdout)),
        );
        match streamed {
            Ok(output) => match output.output {
                // A reader that stopped closed the pipe; how git then exited says nothing.
                Ended::Deadline => Ok(Some(SurveyReasonV1::MaxWallMs)),
                Ended::Bound(reason) => Ok(Some(reason)),
                Ended::Bad(detail) => Err(unreadable(format!("git {what}: {detail}"))),
                // A stream that ended after the deadline ended late.
                Ended::Done if output.status.success() => {
                    Ok(self.clock.passed().then_some(SurveyReasonV1::MaxWallMs))
                }
                Ended::Done => Err(Self::failed(
                    what,
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                )),
            },
            Err(stopped) if stopped.deadline => Ok(Some(SurveyReasonV1::MaxWallMs)),
            Err(stopped) => Err(Self::failed(what, stopped.message)),
        }
    }

    /// The objects `names` name, in order, through one `cat-file --batch`: a missing one is
    /// `None`; a content over `keep` bytes is not kept. Also whether the deadline stopped it,
    /// with the objects before it.
    fn objects(&self, names: &[String], keep: u64) -> Result<(Vec<Option<Object>>, bool), Failure> {
        let mut objects = Vec::with_capacity(names.len());
        if names.is_empty() {
            return Ok((objects, self.clock.passed()));
        }
        let clock = self.clock;
        let stopped = self.stream("cat-file", &os(&["cat-file", "--batch"]), names, |input| {
            read_batch(input, &clock, Some(keep), |_, object| {
                objects.push(object);
                true
            })
        })?;
        // A cat-file header is a record too; one over the bound reads as the deadline would.
        Ok((objects, stopped.is_some()))
    }
}

/// One object of `cat-file --batch`, as the stream yields it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Object {
    pub(super) oid: String,
    pub(super) blob: bool,
    pub(super) size: u64,
    /// Newlines, or `None` for a binary object (a NUL in its first 8 KiB).
    pub(super) lines: Option<u64>,
    /// The content, when it was kept.
    pub(super) content: Option<Vec<u8>>,
}

/// Read `cat-file --batch`: `<oid> <type> <size>\n<content>\n`, or `<name> missing\n`, for each
/// object requested, in order. `each` receives the request's index and the object, and returns
/// `false` when the deadline passed while it took it, which stops the stream; contents of at
/// most `keep` bytes are kept, and every object's newlines are counted as it streams.
pub(super) fn read_batch(
    input: &mut dyn BufRead,
    clock: &Clock,
    keep: Option<u64>,
    mut each: impl FnMut(usize, Option<Object>) -> bool,
) -> Ended {
    let mut header = Vec::new();
    let mut index = 0;
    loop {
        match read_record(input, b'\n', &mut header) {
            Ok(Record::End) => return Ended::Done,
            Ok(Record::Read { .. }) => {}
            Ok(Record::TooLong) => return Ended::Bound(SurveyReasonV1::MaxRecordBytes),
            Err(error) => return Ended::Bad(error),
        }
        if clock.item() {
            return Ended::Deadline;
        }
        let line = String::from_utf8_lossy(&header);
        if line.ends_with(" missing") || line.ends_with(" ambiguous") {
            if !each(index, None) {
                return Ended::Deadline;
            }
            index += 1;
            continue;
        }
        let fields: Vec<&str> = line.split(' ').collect();
        let [oid, kind, size] = fields.as_slice() else {
            return Ended::Bad(format!("the cat-file header `{line}`"));
        };
        let Ok(size) = size.parse::<u64>() else {
            return Ended::Bad(format!("the cat-file header `{line}` has no size"));
        };
        let kept = keep.is_some_and(|keep| size <= keep);
        match read_content(input, size, kept) {
            Ok((lines, content)) => {
                let object = Object {
                    oid: oid.to_string(),
                    blob: *kind == "blob",
                    size,
                    lines,
                    content,
                };
                if !each(index, Some(object)) {
                    return Ended::Deadline;
                }
            }
            Err(detail) => return Ended::Bad(format!("{oid}: {detail}")),
        }
        index += 1;
    }
}

/// `size` bytes of content and the newline after them, read in the stream's own chunks: the
/// newline count (`None` when binary) and, when `keep`, the bytes.
fn read_content(
    input: &mut dyn BufRead,
    size: u64,
    keep: bool,
) -> Result<(Option<u64>, Option<Vec<u8>>), String> {
    let mut remaining = size;
    let (mut newlines, mut probed, mut binary) = (0_u64, 0_usize, false);
    let mut kept = keep.then(Vec::new);
    while remaining > 0 {
        let chunk = input.fill_buf().map_err(|error| error.to_string())?;
        if chunk.is_empty() {
            return Err(format!("the content is shorter than its {size} bytes"));
        }
        let taken = chunk
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let chunk = &chunk[..taken];
        if probed < BINARY_PROBE_BYTES {
            let probe = (BINARY_PROBE_BYTES - probed).min(taken);
            binary |= chunk[..probe].contains(&0);
            probed += probe;
        }
        newlines += chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
        if let Some(kept) = kept.as_mut() {
            kept.extend_from_slice(chunk);
        }
        input.consume(taken);
        remaining -= taken as u64;
    }
    let mut end = [0_u8; 1];
    input
        .read_exact(&mut end)
        .map_err(|error| error.to_string())?;
    if end != *b"\n" {
        return Err("the content does not end where its size says".into());
    }
    Ok(((!binary).then_some(newlines), kept))
}

/// What an entry of the head tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EntryKind {
    File,
    Symlink,
    Submodule,
}

/// One entry of `ls-tree -r -l -z`, its path in the one encoded spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) kind: EntryKind,
    pub(super) oid: String,
    pub(super) size: u64,
    pub(super) path: String,
}

/// One record of `ls-tree -r -l -z`: `<mode> SP <type> SP <oid> SP+ <size> TAB <path>`.
pub(super) fn parse_entry(record: &[u8]) -> Result<Entry, String> {
    let tab = record
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or("an ls-tree record without a path")?;
    let meta = String::from_utf8_lossy(&record[..tab]);
    let fields: Vec<&str> = meta.split_whitespace().collect();
    let [mode, kind, oid, size] = fields.as_slice() else {
        return Err(format!("the ls-tree record `{meta}`"));
    };
    let kind = match (*mode, *kind) {
        ("120000", "blob") => EntryKind::Symlink,
        (_, "blob") => EntryKind::File,
        (_, "commit") => EntryKind::Submodule,
        _ => return Err(format!("the ls-tree entry type `{kind}`")),
    };
    Ok(Entry {
        kind,
        oid: oid.to_string(),
        size: if kind == EntryKind::Submodule {
            0
        } else {
            size.parse()
                .map_err(|_| format!("the ls-tree size `{size}`"))?
        },
        path: review_core::encode_path(&record[tab + 1..]),
    })
}

/// Read NUL-ended records, one item each, as `ls-tree -z` and its `--name-only` form print them.
fn read_records(
    input: &mut dyn BufRead,
    clock: &Clock,
    mut each: impl FnMut(&[u8]) -> Result<(), String>,
) -> Ended {
    let mut record = Vec::new();
    loop {
        match read_record(input, 0, &mut record) {
            Ok(Record::End) => return Ended::Done,
            Ok(Record::Read { .. }) => {}
            Ok(Record::TooLong) => return Ended::Bound(SurveyReasonV1::MaxRecordBytes),
            Err(error) => return Ended::Bad(error),
        }
        if clock.item() {
            return Ended::Deadline;
        }
        if record.is_empty() {
            continue;
        }
        if let Err(detail) = each(&record) {
            return Ended::Bad(detail);
        }
    }
}

/// Read `ls-tree -r -l -z`, one entry at a time, in tree order.
pub(super) fn read_listing(
    input: &mut dyn BufRead,
    clock: &Clock,
    mut each: impl FnMut(Entry),
) -> Ended {
    read_records(input, clock, |record| {
        each(parse_entry(record)?);
        Ok(())
    })
}

/// Read newline-ended lines, one item each, as `for-each-ref` prints them: `each` receives the
/// line and whether its newline followed it.
fn read_lines(input: &mut dyn BufRead, clock: &Clock, mut each: impl FnMut(&[u8], bool)) -> Ended {
    let mut line = Vec::new();
    loop {
        let ended = match read_record(input, b'\n', &mut line) {
            Ok(Record::End) => return Ended::Done,
            Ok(Record::Read { ended }) => ended,
            Ok(Record::TooLong) => return Ended::Bound(SurveyReasonV1::MaxRecordBytes),
            Err(error) => return Ended::Bad(error),
        };
        if clock.item() {
            return Ended::Deadline;
        }
        each(&line, ended);
    }
}

/// The top-level directory of a path; `.` for a root-level file.
pub(super) fn top_level(path: &str) -> &str {
    path.split_once('/').map_or(".", |(top, _)| top)
}

fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn any_match(patterns: &[String], text: &str) -> bool {
    super::collect::any_match(patterns, text)
}

/// A display string as a Survey holds it: trimmed, cut at a character boundary to
/// [`MAX_SURVEY_TEXT_BYTES`], and whether it was cut; `None` when what is left is blank or
/// holds a control character.
pub(super) fn display(text: &str) -> Option<(String, bool)> {
    let text = text.trim();
    let mut end = text.len().min(MAX_SURVEY_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let kept = text[..end].trim_end();
    is_survey_text(kept).then(|| (kept.to_string(), end < text.len()))
}

fn named(text: &str) -> Option<SurveyNameV1> {
    display(text).map(|(name, truncated)| SurveyNameV1 { name, truncated })
}

/// How many items a list keeps, and the `seen` entry when it holds more than `max_entries`.
fn cut<T>(list: &mut Vec<T>, name: &str, max_entries: u64, seen: &mut SurveySeenV1) {
    let held = usize::try_from(max_entries).unwrap_or(usize::MAX);
    if list.len() > held {
        seen.insert(name.into(), decimal(list.len() as u64));
        list.truncate(held);
    }
}

/// [`cut`] for a map: the first `max_entries` by key.
fn cut_map<T>(
    map: BTreeMap<String, T>,
    name: &str,
    max_entries: u64,
    seen: &mut SurveySeenV1,
) -> BTreeMap<String, T> {
    let mut entries: Vec<(String, T)> = map.into_iter().collect();
    cut(&mut entries, name, max_entries, seen);
    entries.into_iter().collect()
}

/// `tree`, folded as its two streams arrive: each entry of the listing as it is read (after
/// `exclude`, at most `max_paths`), then the newline count of each text blob as `cat-file`
/// yields it. No loop over the entries runs after the streams.
#[derive(Debug, Default)]
pub(super) struct TreeFold {
    files: u64,
    bytes: u64,
    lines: u64,
    /// Entries whose blob's lines were counted.
    with_lines: u64,
    directories: BTreeMap<String, (u64, u64, u64)>,
    languages: BTreeMap<String, (u64, u64)>,
    tests: (u64, u64, u64, u64),
    /// The blobs whose lines are still to count, and where each of their entries counts them:
    /// its directory, its language and whether `tests` matches it.
    pending: BTreeMap<String, Vec<(String, String, bool)>>,
}

impl TreeFold {
    /// One entry of the listing: a file, a symlink or a submodule.
    pub(super) fn add(&mut self, entry: &Entry, policy: &Policy) {
        let language = match entry.kind {
            EntryKind::File => policy.language(&entry.path),
            EntryKind::Symlink | EntryKind::Submodule => "other".into(),
        };
        let test = any_match(&policy.tests, &entry.path);
        let directory = top_level(&entry.path).to_string();
        self.files += 1;
        self.bytes += entry.size;
        let counts = self.directories.entry(directory.clone()).or_default();
        counts.0 += 1;
        counts.2 += entry.size;
        self.languages.entry(language.clone()).or_default().0 += 1;
        if test {
            self.tests.0 += 1;
        } else {
            self.tests.2 += 1;
        }
        if entry.kind == EntryKind::File && entry.size <= policy.max_file_bytes {
            self.pending
                .entry(entry.oid.clone())
                .or_default()
                .push((directory, language, test));
        }
    }

    /// The blobs whose lines to count, each once.
    pub(super) fn blobs(&self) -> Vec<String> {
        self.pending.keys().cloned().collect()
    }

    /// The newline count of one blob read, `None` for a binary one, into each of its entries.
    pub(super) fn lines(&mut self, oid: &str, lines: Option<u64>) {
        let (Some(entries), Some(lines)) = (self.pending.remove(oid), lines) else {
            return;
        };
        for (directory, language, test) in entries {
            self.lines += lines;
            self.with_lines += 1;
            if let Some(counts) = self.directories.get_mut(&directory) {
                counts.1 += lines;
            }
            if let Some(counts) = self.languages.get_mut(&language) {
                counts.1 += lines;
            }
            if test {
                self.tests.1 += lines;
            } else {
                self.tests.3 += lines;
            }
        }
    }

    /// The entries folded so far.
    pub(super) fn files(&self) -> u64 {
        self.files
    }

    /// `tree` over the entries folded, of the `seen_paths` the listing held, its lists cut at
    /// `max_entries` by name. An entry whose blob was not read before the deadline, binary or
    /// over `max_file_bytes`, a symlink and a submodule have no lines.
    pub(super) fn finish(self, seen_paths: u64, policy: &Policy) -> SurveyTreeV1 {
        let mut seen = SurveySeenV1::new();
        if seen_paths > self.files {
            seen.insert("paths".into(), decimal(seen_paths));
        }
        let directories = cut_map(
            self.directories,
            "directories",
            policy.max_entries,
            &mut seen,
        );
        let languages = cut_map(self.languages, "languages", policy.max_entries, &mut seen);
        let tests = self.tests;
        SurveyTreeV1 {
            files: decimal(self.files),
            bytes: decimal(self.bytes),
            lines: decimal(self.lines),
            files_without_lines: decimal(self.files - self.with_lines),
            directories: directories
                .into_iter()
                .map(|(name, (files, lines, bytes))| {
                    (
                        name,
                        SurveyDirectoryV1 {
                            files: decimal(files),
                            lines: decimal(lines),
                            bytes: decimal(bytes),
                        },
                    )
                })
                .collect(),
            languages: languages
                .into_iter()
                .map(|(name, (files, lines))| {
                    (
                        name,
                        SurveyLanguageV1 {
                            files: decimal(files),
                            lines: decimal(lines),
                        },
                    )
                })
                .collect(),
            tests: SurveyTestsV1 {
                files: decimal(tests.0),
                lines: decimal(tests.1),
                other_files: decimal(tests.2),
                other_lines: decimal(tests.3),
            },
            seen,
        }
    }
}

/// One `--numstat` line of a commit, after `exclude`; a binary change is one file touched with
/// no lines added or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Change {
    pub(super) added: u64,
    pub(super) removed: u64,
    pub(super) path: String,
}

/// One commit reached, as the Survey keeps it: never its author's email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Commit {
    pub(super) id: String,
    pub(super) merge: bool,
    /// Committer time, Unix seconds.
    pub(super) time: u64,
    /// The author's identity: what contributors are grouped and ranked by.
    pub(super) key: String,
    /// The author's display text, under `identity = "name"` only.
    pub(super) name: Option<String>,
    /// The name was cut to [`MAX_SURVEY_TEXT_BYTES`].
    pub(super) truncated: bool,
    /// Agent-assisted: the author or a `Co-Authored-By` name matches the agent patterns.
    pub(super) agent: bool,
    pub(super) changes: Vec<Change>,
}

/// The first 16 hex digits of SHA-256 of `text` lowercased and trimmed.
pub(crate) fn hash16(text: &str) -> String {
    hex16_of(text.trim().to_lowercase().as_bytes())
}

/// The first 16 hex digits of SHA-256 of `bytes`.
fn hex16_of(bytes: &[u8]) -> String {
    review_core::hex::encode(&Sha256::digest(bytes))[..SURVEY_EMAIL_HASH_HEX_LEN].to_string()
}

/// A contributor as a record holds one: its key, its display name under `identity = "name"`, and
/// whether the name was cut. An author name any whitespace-separated token of which is an email
/// address, under either policy, is `h:` and the 16-hex hash of the lowercased, trimmed name as
/// both key and name, decided before any cut and without the email; so is a name a record
/// cannot hold as text. Otherwise, under `name`, the key is the 16-hex hash of the exact
/// author-name bytes and the name is cut to [`MAX_SURVEY_TEXT_BYTES`]; under `email_hash`, the
/// key is the 16-hex hash of the email and there is no name. An email never enters a record.
pub(super) fn identity_of(
    name: &[u8],
    email: &str,
    policy: SurveyIdentityPolicyV1,
) -> (String, Option<String>, bool) {
    let text = String::from_utf8_lossy(name);
    let hashed = format!("{SURVEY_NAME_HASH_PREFIX}{}", hash16(&text));
    match policy {
        _ if is_email_shaped(&text) => match policy {
            SurveyIdentityPolicyV1::Name => (hashed.clone(), Some(hashed), false),
            SurveyIdentityPolicyV1::EmailHash => (hashed, None, false),
        },
        SurveyIdentityPolicyV1::Name => match display(&text) {
            Some((kept, truncated)) if !is_email_shaped(&kept) => {
                (hex16_of(name), Some(kept), truncated)
            }
            _ => (hashed.clone(), Some(hashed), false),
        },
        SurveyIdentityPolicyV1::EmailHash => (hash16(email), None, false),
    }
}

/// The name of a `Co-Authored-By` value: the text before its `<address>`.
fn trailer_name(value: &str) -> &str {
    value.split_once('<').map_or(value, |(name, _)| name).trim()
}

/// The `log` format: record separator, then id, parents, committer time, author name, author
/// email (only under `email_hash`) and the `Co-Authored-By` values, each ended by a unit
/// separator.
pub(super) fn log_format(policy: &Policy) -> String {
    let email = match policy.identity {
        SurveyIdentityPolicyV1::Name => "",
        SurveyIdentityPolicyV1::EmailHash => "%ae",
    };
    format!(
        "--format=%x1e%H%x1f%P%x1f%ct%x1f%an%x1f{email}%x1f\
         %(trailers:key=Co-authored-by,valueonly,separator=%x1d)%x1f"
    )
}

/// The commit a log record's six fields describe, its identity and partition derived as it is
/// read: an email is hashed, or never asked for, and is never kept.
fn commit_of(fields: &[Vec<u8>], policy: &Policy) -> Result<Commit, String> {
    let [id, parents, time, name, email, trailers] = fields else {
        return Err("a log record without its six fields".into());
    };
    let text = |field: &[u8]| String::from_utf8_lossy(field).into_owned();
    let id = text(id);
    if !review_core::project::is_commit_id(&id) {
        return Err("a log record without a commit ID".into());
    }
    let agent = any_match(&policy.agent_authors, text(name).trim())
        || text(trailers)
            .split('\u{1d}')
            .map(trailer_name)
            .filter(|value| !value.is_empty())
            .any(|value| any_match(&policy.agent_trailers, value));
    let (key, display_name, truncated) = identity_of(name, &text(email), policy.identity);
    Ok(Commit {
        id,
        merge: text(parents).split_whitespace().count() >= 2,
        time: text(time)
            .trim()
            .parse()
            .map_err(|_| "a committer time that is not a number")?,
        key,
        name: display_name,
        truncated,
        agent,
        changes: Vec::new(),
    })
}

/// One `added TAB removed TAB path` of `--numstat -z`, or nothing when `exclude` removes it.
fn change_of(line: &[u8], policy: &Policy) -> Result<Option<Change>, String> {
    let mut parts = line.splitn(3, |byte| *byte == b'\t');
    let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err("a numstat line without three fields".into());
    };
    let count = |field: &[u8]| -> Result<u64, String> {
        if field == b"-" {
            return Ok(0);
        }
        std::str::from_utf8(field)
            .ok()
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| "a numstat count that is not a number".to_string())
    };
    let path = review_core::encode_path(path);
    if any_match(&policy.exclude, &path) {
        return Ok(None);
    }
    Ok(Some(Change {
        added: count(added)?,
        removed: count(removed)?,
        path,
    }))
}

/// Read `log -z --numstat` in [`log_format`], one commit and one numstat line per item; `each`
/// receives every commit read to its end and may stop the walk with a bound. A record over
/// [`MAX_SURVEY_RECORD_BYTES`] stops it with `max_record_bytes`, and a commit touching more
/// than `max_paths` paths with `max_paths`; the commit it stopped in is not passed on.
pub(super) fn read_log(
    input: &mut dyn BufRead,
    clock: &Clock,
    policy: &Policy,
    mut each: impl FnMut(Commit) -> Result<(), SurveyReasonV1>,
) -> Ended {
    const RECORD: u8 = 0x1e;
    const UNIT: u8 = 0x1f;
    let mut current: Option<Commit> = None;
    let mut line = Vec::new();
    loop {
        let first = match input.fill_buf() {
            Ok([]) => None,
            Ok(buffer) => Some(buffer[0]),
            Err(error) => return Ended::Bad(error.to_string()),
        };
        match first {
            None => {
                return match each_commit(&mut current, &mut each) {
                    Ok(()) => Ended::Done,
                    Err(reason) => Ended::Bound(reason),
                };
            }
            Some(RECORD) => {
                input.consume(1);
                if let Err(reason) = each_commit(&mut current, &mut each) {
                    return Ended::Bound(reason);
                }
                if clock.item() {
                    return Ended::Deadline;
                }
                let mut fields = Vec::with_capacity(6);
                for _ in 0..6 {
                    match read_record(input, UNIT, &mut line) {
                        Ok(Record::Read { ended: true }) => fields.push(line.clone()),
                        Ok(Record::TooLong) => {
                            return Ended::Bound(SurveyReasonV1::MaxRecordBytes);
                        }
                        Ok(_) => return Ended::Bad("a log record ends early".into()),
                        Err(error) => return Ended::Bad(error),
                    }
                }
                match commit_of(&fields, policy) {
                    Ok(commit) => current = Some(commit),
                    Err(detail) => return Ended::Bad(detail),
                }
            }
            Some(0 | b'\n') => input.consume(1),
            Some(_) => {
                match read_record(input, 0, &mut line) {
                    Ok(Record::Read { .. }) => {}
                    Ok(Record::TooLong) => return Ended::Bound(SurveyReasonV1::MaxRecordBytes),
                    Ok(Record::End) => return Ended::Bad("a numstat line ends early".into()),
                    Err(error) => return Ended::Bad(error),
                }
                if clock.item() {
                    return Ended::Deadline;
                }
                let Some(commit) = current.as_mut() else {
                    return Ended::Bad("a numstat line before its commit".into());
                };
                match change_of(&line, policy) {
                    Ok(Some(_)) if commit.changes.len() as u64 >= policy.max_paths => {
                        return Ended::Bound(SurveyReasonV1::MaxPaths);
                    }
                    Ok(change) => commit.changes.extend(change),
                    Err(detail) => return Ended::Bad(detail),
                }
            }
        }
    }
}

fn each_commit(
    current: &mut Option<Commit>,
    each: &mut impl FnMut(Commit) -> Result<(), SurveyReasonV1>,
) -> Result<(), SurveyReasonV1> {
    match current.take() {
        Some(commit) => each(commit),
        None => Ok(()),
    }
}

/// Whether `time` falls in the window of `days` before `cutoff`:
/// `cutoff - window < time <= cutoff`.
fn in_window(time: u64, cutoff: u64, days: u64) -> bool {
    time <= cutoff && time > cutoff.saturating_sub(days * DAY_S)
}

/// One contributor's totals over the commits reached.
#[derive(Debug, Clone, Default)]
struct Totals {
    name: Option<String>,
    commits: u64,
    added: u64,
    removed: u64,
    first: u64,
    last: u64,
    active: bool,
    truncated: bool,
}

/// The commits reached, folded as they arrive: what `history` and `contributors` count.
#[derive(Debug, Clone, Default)]
pub(super) struct Walk {
    cutoff: u64,
    /// The distinct paths the walk may track: `max_paths`.
    max_paths: u64,
    pub(super) commits: u64,
    merges: u64,
    /// Commits and merges in the 30, 90 and 365-day windows.
    windows: [(u64, u64); 3],
    first: Option<u64>,
    last: u64,
    /// The last commit of the walk, the oldest reached.
    oldest: Option<String>,
    churn: BTreeMap<String, (u64, u64, u64)>,
    touched: BTreeMap<String, u64>,
    identities: BTreeMap<String, Totals>,
    /// The commits of the identity with the most, and the identities active in 90 days.
    top: u64,
    active: u64,
    agent: u64,
}

impl Walk {
    pub(super) fn new(cutoff: u64, max_paths: u64) -> Self {
        Self {
            cutoff,
            max_paths,
            ..Self::default()
        }
    }

    /// Fold one commit. A merge counts as a commit and a merge and contributes no churn. A
    /// commit that would make the walk track more than `max_paths` distinct paths is not folded
    /// and stops the walk with `max_paths`.
    pub(super) fn add(&mut self, commit: Commit) -> Result<(), SurveyReasonV1> {
        let cutoff = self.cutoff;
        let recent = in_window(commit.time, cutoff, 90);
        let changes: &[Change] = if commit.merge { &[] } else { &commit.changes };
        if recent {
            let new: BTreeSet<&str> = changes
                .iter()
                .map(|change| change.path.as_str())
                .filter(|path| !self.touched.contains_key(*path))
                .collect();
            if self.touched.len() as u64 + new.len() as u64 > self.max_paths {
                return Err(SurveyReasonV1::MaxPaths);
            }
        }
        self.commits += 1;
        self.merges += u64::from(commit.merge);
        for (window, days) in self.windows.iter_mut().zip([30, 90, 365]) {
            if in_window(commit.time, cutoff, days) {
                window.0 += 1;
                window.1 += u64::from(commit.merge);
            }
        }
        self.first = Some(
            self.first
                .map_or(commit.time, |first| first.min(commit.time)),
        );
        self.last = self.last.max(commit.time);
        let (added, removed) = changes.iter().fold((0, 0), |(added, removed), change| {
            (added + change.added, removed + change.removed)
        });
        if recent {
            let mut directories = BTreeSet::new();
            let mut paths = BTreeSet::new();
            for change in changes {
                let directory = self
                    .churn
                    .entry(top_level(&change.path).to_string())
                    .or_default();
                directory.1 += change.added;
                directory.2 += change.removed;
                directories.insert(top_level(&change.path));
                paths.insert(change.path.as_str());
            }
            for directory in directories {
                self.churn.get_mut(directory).expect("entered above").0 += 1;
            }
            for path in paths {
                *self.touched.entry(path.to_string()).or_default() += 1;
            }
        }
        let totals = self.identities.entry(commit.key).or_insert_with(|| Totals {
            name: commit.name,
            first: u64::MAX,
            ..Totals::default()
        });
        totals.commits += 1;
        totals.added += added;
        totals.removed += removed;
        totals.first = totals.first.min(commit.time);
        totals.last = totals.last.max(commit.time);
        self.active += u64::from(recent && !totals.active);
        totals.active |= recent;
        totals.truncated |= commit.truncated;
        self.top = self.top.max(totals.commits);
        self.agent += u64::from(commit.agent);
        self.oldest = Some(commit.id);
        Ok(())
    }

    /// The paths with the most commits in 90 days, most first, ties by path: at most `limit`,
    /// with how many distinct paths there were.
    pub(super) fn hot_paths(&self, limit: u64) -> (Vec<(String, u64)>, u64) {
        let mut hot: Vec<(&String, u64)> = self
            .touched
            .iter()
            .map(|(path, commits)| (path, *commits))
            .collect();
        hot.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        hot.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        (
            hot.into_iter()
                .map(|(path, commits)| (path.clone(), commits))
                .collect(),
            self.touched.len() as u64,
        )
    }
}

/// `history` from the walk, without its tags and branches: `hot` are the hot paths, `distinct`
/// how many paths were touched in 90 days, `absent` the hot paths the head no longer holds.
pub(super) fn history_section(
    walk: &Walk,
    reachable: u64,
    hot: Vec<(String, u64)>,
    distinct: u64,
    absent: &BTreeSet<String>,
    policy: &Policy,
) -> SurveyHistoryV1 {
    let window = |index: usize| SurveyWindowV1 {
        commits: decimal(walk.windows[index].0),
        merges: decimal(walk.windows[index].1),
    };
    let mut seen = SurveySeenV1::new();
    let churn = cut_map(
        walk.churn.clone(),
        "churn_90d",
        policy.max_entries,
        &mut seen,
    );
    if policy.hot_paths > policy.max_entries && distinct > policy.max_entries {
        seen.insert("hot_paths".into(), decimal(distinct));
    }
    SurveyHistoryV1 {
        reachable_commits: decimal(reachable),
        commits: decimal(walk.commits),
        merges: decimal(walk.merges),
        windows: SurveyWindowsV1 {
            days_30: window(0),
            days_90: window(1),
            days_365: window(2),
        },
        first_commit_unix_s: decimal(walk.first.unwrap_or(0)),
        last_commit_unix_s: decimal(walk.last),
        oldest_reached: (walk.commits < reachable)
            .then(|| walk.oldest.clone())
            .flatten(),
        churn_90d: churn
            .into_iter()
            .map(|(name, (commits, added, removed))| {
                (
                    name,
                    SurveyChurnV1 {
                        commits: decimal(commits),
                        added: decimal(added),
                        removed: decimal(removed),
                    },
                )
            })
            .collect(),
        hot_paths: hot
            .into_iter()
            .map(|(path, commits)| SurveyHotPathV1 {
                removed: absent.contains(&path).then_some(true),
                path,
                commits: decimal(commits),
            })
            .collect(),
        tags: None,
        local_branches: None,
        seen,
    }
}

/// How a release tag ranks: by its date (the tagger time of an annotated tag, the target's
/// committer time of a lightweight one) newest first, undated last, then by name.
type TagRank = (bool, std::cmp::Reverse<u64>, String);

/// The refs `history` counts, read from one `for-each-ref` of `refs/heads` and `refs/tags`.
#[derive(Debug, Default)]
pub(super) struct Refs {
    branches: u64,
    tags: u64,
    /// The newest release tags so far, ranked, at most [`SURVEY_RELEASE_TAGS`].
    release: Vec<(TagRank, SurveyNameV1)>,
    release_seen: u64,
}

impl Refs {
    /// One ref, and its creator date when it has one.
    pub(super) fn add(&mut self, refname: &str, date: Option<u64>, policy: &Policy) {
        if refname.starts_with("refs/heads/") {
            self.branches += 1;
        } else if let Some(tag) = refname.strip_prefix("refs/tags/") {
            self.tags += 1;
            if glob_matches(&policy.release_tags, tag)
                && let Some(name) = named(tag)
            {
                self.release_seen += 1;
                let rank = (
                    date.is_none(),
                    std::cmp::Reverse(date.unwrap_or(0)),
                    tag.to_string(),
                );
                let at = self.release.partition_point(|(held, _)| *held < rank);
                if at < SURVEY_RELEASE_TAGS {
                    self.release.insert(at, (rank, name));
                    self.release.truncate(SURVEY_RELEASE_TAGS);
                }
            }
        }
    }

    /// One line of `for-each-ref --format=%(creatordate:unix)%09%(refname)`.
    fn line(&mut self, line: &[u8], policy: &Policy) {
        let line = String::from_utf8_lossy(line);
        let (date, refname) = line.split_once('\t').unwrap_or(("", &line));
        self.add(refname.trim_end(), date.trim().parse().ok(), policy);
    }

    /// The tags and the local branch count, and the release tags cut at `max_entries`.
    pub(super) fn fill(self, history: &mut SurveyHistoryV1, policy: &Policy) {
        let held = usize::try_from(policy.max_entries).unwrap_or(usize::MAX);
        let mut release: Vec<SurveyNameV1> =
            self.release.into_iter().map(|(_, name)| name).collect();
        if release.len() > held {
            history
                .seen
                .insert("tags".into(), decimal(self.release_seen));
            release.truncate(held);
        }
        history.tags = Some(SurveyTagsV1 {
            count: decimal(self.tags),
            release,
        });
        history.local_branches = Some(decimal(self.branches));
    }
}

/// `contributors` from the walk: every identity with its totals, ranked by commits, then lines
/// added, both descending, then key, and kept at `max_entries`, and the human and agent-assisted
/// partition, each commit once. The ranking reads one identity per item of `clock`; when the
/// deadline passes it ranks those it read and says so with `true`, the list then recording
/// under `seen` how many identities there were.
pub(super) fn contributors_section(
    walk: &Walk,
    policy: &Policy,
    clock: &Clock,
) -> (SurveyContributorsV1, bool) {
    let total = walk.commits;
    let share = |part: u64| decimal((part * 10_000).checked_div(total).unwrap_or(0));
    let mut ranked: Vec<(&String, &Totals)> = Vec::new();
    let mut deadline = false;
    for identity in &walk.identities {
        if clock.item() {
            deadline = true;
            break;
        }
        ranked.push(identity);
    }
    ranked.sort_by(|a, b| {
        b.1.commits
            .cmp(&a.1.commits)
            .then(b.1.added.cmp(&a.1.added))
            .then(a.0.cmp(b.0))
    });
    let mut seen = SurveySeenV1::new();
    cut(&mut ranked, "by_identity", policy.max_entries, &mut seen);
    let identities = walk.identities.len() as u64;
    if (ranked.len() as u64) < identities {
        seen.insert("by_identity".into(), decimal(identities));
    }
    let body = SurveyContributorsV1 {
        identity: policy.identity,
        identities: decimal(identities),
        active_90d: decimal(walk.active),
        human_commits: decimal(total - walk.agent),
        agent_commits: decimal(walk.agent),
        human_share_bp: share(total - walk.agent),
        agent_share_bp: share(walk.agent),
        top_share_bp: share(walk.top),
        by_identity: ranked
            .into_iter()
            .map(|(key, totals)| SurveyContributorV1 {
                key: key.clone(),
                name: totals.name.clone(),
                commits: decimal(totals.commits),
                added: decimal(totals.added),
                removed: decimal(totals.removed),
                first_unix_s: decimal(totals.first),
                last_unix_s: decimal(totals.last),
                active_90d: totals.active,
                truncated: totals.truncated,
            })
            .collect(),
        seen,
    };
    (body, deadline)
}

/// The first `YYYY-MM-DD` of `line` that no digit touches, when it is a calendar date: a month
/// of `01` to `12` and a day of `01` to `31`. A malformed first date is no date.
fn first_date(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let first = (0..bytes.len().saturating_sub(9)).find(|&start| {
        let candidate = &bytes[start..start + 10];
        let shaped = candidate.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        });
        let touched = (start > 0 && bytes[start - 1].is_ascii_digit())
            || bytes.get(start + 10).is_some_and(u8::is_ascii_digit);
        shaped && !touched
    })?;
    let date = &line[first..first + 10];
    let month: u8 = date[5..7].parse().ok()?;
    let day: u8 = date[8..10].parse().ok()?;
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then(|| date.to_string())
}

/// The status a status line names: the first, by position, of the closed statuses, each a whole
/// word in any case.
fn named_status(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    SURVEY_DECISION_STATUSES
        .iter()
        .filter_map(|status| {
            lower.match_indices(status).find_map(|(start, _)| {
                let end = start + status.len();
                let before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
                let after = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
                (before && after).then_some((start, *status))
            })
        })
        .min()
        .map(|(_, status)| status)
}

/// Whether `line` is a decision's status line: it starts with `**Status:**` or `Status:`.
fn is_status_line(line: &str) -> bool {
    line.starts_with("**Status:**") || line.starts_with("Status:")
}

/// The number of a decision file: its name's four leading ASCII digits, when a dash follows.
pub(super) fn decision_number(path: &str) -> Option<&str> {
    let name = basename(path).as_bytes();
    (name.len() > 5 && name[..4].iter().all(u8::is_ascii_digit) && name[4] == b'-')
        .then(|| &basename(path)[..4])
}

/// Whether `path` is a decision record: the `decisions` pattern matches it and its name starts
/// with four ASCII digits and a dash.
pub(super) fn is_decision(path: &str, policy: &Policy) -> bool {
    glob_matches(&policy.decisions, path) && decision_number(path).is_some()
}

/// One decision record: number from the file name, title from the first H1, and status and
/// date from the first line that starts with `**Status:**` or `Status:`: the first closed
/// status on it, `amended` when it says so, and its first date; each absent, counted `unknown`,
/// without one. Without a text content (over `max_file_bytes`, binary or not UTF-8) it is
/// `unreadable` and has no title.
pub(super) fn decision(path: &str, content: Option<&str>) -> SurveyDecisionV1 {
    let number = decision_number(path).unwrap_or_default().to_string();
    let Some(text) = content else {
        return SurveyDecisionV1 {
            number,
            path: path.to_string(),
            title: None,
            truncated: false,
            status: Some(SURVEY_UNREADABLE.into()),
            date: None,
            amended: false,
        };
    };
    let (title, truncated) = text
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .and_then(display)
        .map_or((None, false), |(title, cut)| (Some(title), cut));
    let status_line = text.lines().find(|line| is_status_line(line));
    SurveyDecisionV1 {
        number,
        path: path.to_string(),
        title,
        truncated,
        status: status_line.and_then(named_status).map(str::to_string),
        date: status_line.and_then(first_date),
        amended: status_line.is_some_and(|line| line.to_ascii_lowercase().contains("amended")),
    }
}

/// `decisions` over every record read, sorted by number then path, the list cut at
/// `max_entries`.
pub(super) fn decisions_section(
    mut records: Vec<SurveyDecisionV1>,
    policy: &Policy,
) -> SurveyDecisionsV1 {
    records.sort_by(|a, b| (&a.number, &a.path).cmp(&(&b.number, &b.path)));
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    for record in &records {
        *by_status
            .entry(record.status.clone().unwrap_or_else(|| "unknown".into()))
            .or_default() += 1;
    }
    let count = records.len() as u64;
    let mut seen = SurveySeenV1::new();
    cut(&mut records, "records", policy.max_entries, &mut seen);
    SurveyDecisionsV1 {
        count: decimal(count),
        by_status: by_status
            .into_iter()
            .map(|(status, count)| (status, decimal(count)))
            .collect(),
        records,
        seen,
    }
}

/// The names of the bold-colon headings, `**Term**:` alone on a line, by name, each once: one
/// line per item of `clock`, and whether the deadline stopped the reading.
pub(super) fn terms(content: &str, clock: &Clock) -> (Vec<SurveyNameV1>, bool) {
    let mut terms: BTreeMap<String, bool> = BTreeMap::new();
    for line in content.lines() {
        if clock.item() {
            return (named_terms(terms), true);
        }
        let Some(inner) = line
            .trim_end()
            .strip_prefix("**")
            .and_then(|rest| rest.strip_suffix("**:"))
        else {
            continue;
        };
        if inner.contains("**") {
            continue;
        }
        if let Some(term) = named(inner) {
            terms.entry(term.name).or_insert(term.truncated);
        }
    }
    (named_terms(terms), false)
}

fn named_terms(terms: BTreeMap<String, bool>) -> Vec<SurveyNameV1> {
    terms
        .into_iter()
        .map(|(name, truncated)| SurveyNameV1 { name, truncated })
        .collect()
}

/// `vocabulary`: the file, whether the head holds it, and its terms; `content` is `None` when
/// the file is over `max_file_bytes`, not text or not UTF-8, which leaves the terms
/// unmeasured. Also whether the deadline stopped the reading of the terms.
pub(super) fn vocabulary_section(
    file: &str,
    present: bool,
    content: Option<&str>,
    policy: &Policy,
    clock: &Clock,
) -> (SurveyVocabularyV1, bool) {
    let mut seen = SurveySeenV1::new();
    let mut deadline = false;
    let listed = match (present, content) {
        (false, _) => Some(Vec::new()),
        (true, Some(content)) => {
            let (mut found, stopped) = terms(content, clock);
            deadline = stopped;
            cut(&mut found, "terms", policy.max_entries, &mut seen);
            Some(found)
        }
        (true, None) => None,
    };
    let count = listed.as_ref().map(|listed| {
        decimal(
            seen.get("terms")
                .map_or(listed.len() as u64, |seen| seen.get()),
        )
    });
    let body = SurveyVocabularyV1 {
        file: file.to_string(),
        present,
        count,
        terms: listed,
        seen,
    };
    (body, deadline)
}

/// A check's name and program as a record may hold them: an absolute program becomes its file
/// name; either is cut to [`MAX_SURVEY_TEXT_BYTES`], and the check marked.
fn check(source: &str, name: &str, program: &str) -> Option<SurveyCheckV1> {
    let program = if program.starts_with('/') {
        basename(program)
    } else {
        program
    };
    let (name, name_cut) = display(name)?;
    let (program, program_cut) = display(program)?;
    Some(SurveyCheckV1 {
        source: source.to_string(),
        name,
        program,
        truncated: name_cut || program_cut,
    })
}

/// The checks `.af/code-policy.toml` declares: `[checks.<key>]` with `name` (else the key) and
/// `command.program`, each with its exact name. Read leniently; a file that does not parse
/// declares nothing here.
pub(super) fn code_policy_checks(content: &str) -> Vec<(String, SurveyCheckV1)> {
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return Vec::new();
    };
    let Some(checks) = value.get("checks").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    checks
        .iter()
        .filter_map(|(key, declared)| {
            let name = declared
                .get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            let program = declared
                .get("command")
                .and_then(|command| command.get("program"))
                .and_then(toml::Value::as_str)?;
            Some((
                name.to_string(),
                check(SURVEY_CODE_POLICY_SOURCE, name, program)?,
            ))
        })
        .collect()
}

/// The `[[checks]]` of one pipeline file: `name` and `program`, each with its exact name, the
/// source the file's basename.
pub(super) fn pipeline_checks(path: &str, content: &str) -> Vec<(String, SurveyCheckV1)> {
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return Vec::new();
    };
    let Some(checks) = value.get("checks").and_then(toml::Value::as_array) else {
        return Vec::new();
    };
    checks
        .iter()
        .filter_map(|declared| {
            let name = declared.get("name")?.as_str()?;
            Some((
                name.to_string(),
                check(basename(path), name, declared.get("program")?.as_str()?)?,
            ))
        })
        .collect()
}

/// What the tree listing found for `checks` at the root and under `.github/workflows/`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Found {
    pub(super) toolchain: BTreeSet<String>,
    pub(super) workflows: u64,
}

impl Found {
    /// One file of the tree, after `exclude`.
    pub(super) fn add(&mut self, path: &str) {
        if TOOLCHAIN_FILES.contains(&path) {
            self.toolchain.insert(path.to_string());
        }
        if WORKFLOWS.iter().any(|pattern| glob_matches(pattern, path)) {
            self.workflows += 1;
        }
    }
}

/// `checks`, folded one declaration at a time as `cat-file` yields them. A check's identity is
/// `<source>:<name>`; the first declared of an identity is kept.
#[derive(Debug, Default)]
pub(super) struct ChecksFold {
    declared: BTreeMap<(String, String), SurveyCheckV1>,
    unreadable: Vec<String>,
}

impl ChecksFold {
    /// The declaration at `path`, `None` when it is over `max_file_bytes`, not text or not
    /// UTF-8 (listed under `unreadable`): one declared check per item of `clock`. `false` when
    /// the deadline stopped it.
    pub(super) fn add(&mut self, path: &str, content: Option<&str>, clock: &Clock) -> bool {
        let checks = match content {
            None => {
                self.unreadable.push(path.to_string());
                return true;
            }
            Some(content) if path == CODE_POLICY => code_policy_checks(content),
            Some(content) => pipeline_checks(path, content),
        };
        for (name, check) in checks {
            if clock.item() {
                return false;
            }
            self.declared
                .entry((check.source.clone(), name))
                .or_insert(check);
        }
        true
    }

    /// `checks` from the declarations folded and what the listing found, every list ranked by
    /// name, then cut at `max_entries` with `seen`.
    pub(super) fn finish(self, found: &Found, policy: &Policy) -> SurveyChecksV1 {
        let mut declared: Vec<SurveyCheckV1> = self.declared.into_values().collect();
        declared.sort_by(|a, b| (a.identity(), &a.program).cmp(&(b.identity(), &b.program)));
        let mut unreadable = self.unreadable;
        unreadable.sort();
        let mut toolchain: Vec<String> = found.toolchain.iter().cloned().collect();
        let mut seen = SurveySeenV1::new();
        cut(&mut declared, "declared", policy.max_entries, &mut seen);
        cut(&mut toolchain, "toolchain", policy.max_entries, &mut seen);
        cut(&mut unreadable, "unreadable", policy.max_entries, &mut seen);
        SurveyChecksV1 {
            declared,
            toolchain,
            ci_workflows: decimal(found.workflows),
            unreadable,
            seen,
        }
    }
}

/// Hand the declarations `entries` to `each`, in order, as one `cat-file --batch` streams them:
/// each with its content when it is a text blob, valid UTF-8, at or under `max_file_bytes`, and
/// `None` otherwise, never decoded lossily. One declaration per item of the reader's Clock;
/// `each` returns `false` when the deadline passed while it took one. `true` when the deadline
/// stopped the reading, the declarations before it handed on.
fn each_declaration(
    reader: &Reader,
    entries: &[&Entry],
    policy: &Policy,
    mut each: impl FnMut(&Entry, Option<String>) -> bool + Send,
) -> Result<bool, Failure> {
    let readable = |entry: &Entry| entry.size <= policy.max_file_bytes;
    let names: Vec<String> = entries
        .iter()
        .filter(|entry| readable(entry))
        .map(|entry| entry.oid.clone())
        .collect();
    let clock = reader.clock;
    let mut position = 0;
    let mut deadline = false;
    if !names.is_empty() {
        let stopped =
            reader.stream("cat-file", &os(&["cat-file", "--batch"]), &names, |input| {
                read_batch(input, &clock, Some(policy.max_file_bytes), |_, object| {
                    // The oversized declarations before this one, then this one.
                    while let Some(entry) = entries.get(position).filter(|entry| !readable(entry)) {
                        position += 1;
                        if !each(entry, None) {
                            return false;
                        }
                    }
                    let Some(entry) = entries.get(position) else {
                        return true;
                    };
                    position += 1;
                    let content = object
                        .filter(|object| object.lines.is_some())
                        .and_then(|object| object.content)
                        .and_then(|content| String::from_utf8(content).ok());
                    each(entry, content)
                })
            })?;
        deadline = stopped.is_some();
    }
    while !deadline && let Some(entry) = entries.get(position) {
        position += 1;
        deadline = clock.item() || !each(entry, None);
    }
    Ok(deadline)
}

/// What the Surveyor knows before it reads the head: the Project, the survey key, the policy,
/// the cutoff and git's version.
pub(super) struct Basis<'a> {
    pub(super) project_id: &'a str,
    pub(super) repository_id: &'a str,
    pub(super) head: &'a str,
    pub(super) refs_digest: &'a str,
    pub(super) policy: &'a Policy,
    pub(super) policy_digest: &'a str,
    pub(super) cutoff: u64,
    pub(super) surveyor_version: u64,
    pub(super) git_version: &'a str,
}

/// The seven sections, in record order.
pub(super) struct Sections {
    pub(super) identity: SurveySectionV1<SurveyIdentityV1>,
    pub(super) tree: SurveySectionV1<SurveyTreeV1>,
    pub(super) history: SurveySectionV1<SurveyHistoryV1>,
    pub(super) contributors: SurveySectionV1<SurveyContributorsV1>,
    pub(super) decisions: SurveySectionV1<SurveyDecisionsV1>,
    pub(super) vocabulary: SurveySectionV1<SurveyVocabularyV1>,
    pub(super) checks: SurveySectionV1<SurveyChecksV1>,
}

/// The `[project] name`, `min_af` and af pin the head declares, each only when a record may
/// hold it.
fn declarations(
    basis: &Basis,
    af_toml: Option<&[u8]>,
    af_lock: Option<&[u8]>,
) -> (Option<String>, Option<String>, Option<SurveyAfPinV1>) {
    let admissible = |name: Option<String>, min_af: Option<String>| {
        ProjectRegisteredPayloadV1 {
            project_id: basis.project_id.to_string(),
            repository_id: basis.repository_id.to_string(),
            name,
            min_af,
            registered_unix_ms: 0.into(),
        }
        .validate()
        .is_ok()
    };
    let project = af_toml
        .and_then(|content| toml::from_str::<toml::Value>(&String::from_utf8_lossy(content)).ok())
        .and_then(|value| value.get("project").cloned());
    let field = |key: &str| {
        project
            .as_ref()
            .and_then(|project| project.get(key))
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    let name = field("name").filter(|name| admissible(Some(name.clone()), None));
    let min_af = field("min_af").filter(|min_af| admissible(None, Some(min_af.clone())));
    let pin = af_lock
        .and_then(|content| review_config::lock::pinned_af(&String::from_utf8_lossy(content)))
        .map(|pin| SurveyAfPinV1 {
            version: pin.version,
            digests: pin.digests,
        })
        .filter(|pin| {
            let safe = |value: &str, extra: &[u8]| {
                !value.is_empty()
                    && value.len() <= review_core::project::MAX_MIN_AF_BYTES
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || extra.contains(&byte))
            };
            safe(&pin.version, b".-+")
                && pin
                    .digests
                    .iter()
                    .all(|(target, digest)| safe(target, b"-_.") && review_core::is_digest(digest))
        });
    (name, min_af, pin)
}

/// The head commit of the checkout under the Survey's Clock: `None` when the deadline passed.
pub(super) fn head(git: &Git, clock: &Clock) -> Result<Option<String>, Failure> {
    let reader = Reader { git, clock: *clock };
    reader.answer(
        "rev-parse",
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )
}

/// `sha256:` + hex(SHA-256 of the exact stdout of `git for-each-ref
/// --format=%(refname)%09%(objectname) refs/heads refs/tags`), hashed as it streams under the
/// Survey's Clock, one ref per item: the refs of the survey key. The error is the bound that
/// stopped it: `max_wall_ms`, or `max_record_bytes` for a ref longer than
/// [`MAX_SURVEY_RECORD_BYTES`].
pub(super) fn refs_digest(
    git: &Git,
    clock: &Clock,
) -> Result<Result<String, SurveyReasonV1>, Failure> {
    let reader = Reader { git, clock: *clock };
    let mut hasher = Sha256::new();
    let stopped = reader.stream(
        "for-each-ref",
        &os(&[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)",
            "refs/heads",
            "refs/tags",
        ]),
        &[],
        |input| {
            read_lines(input, clock, |line, ended| {
                hasher.update(line);
                if ended {
                    hasher.update(b"\n");
                }
            })
        },
    )?;
    Ok(match stopped {
        None => Ok(format!(
            "sha256:{}",
            review_core::hex::encode(&hasher.finalize())
        )),
        Some(reason) => Err(reason),
    })
}

/// The `git --version` stdout, trimmed, under the Survey's Clock: `None` when the deadline
/// passed.
pub(super) fn git_version(git: &Git, clock: &Clock) -> Result<Option<String>, Failure> {
    let reader = Reader { git, clock: *clock };
    reader.answer("--version", &["--version"])
}

/// The section after a deadline: `unavailable`, and every later one with it.
fn after_deadline<T>() -> SurveySectionV1<T> {
    SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
}

/// A measured section: `partial` with the first of `reasons` that holds, otherwise complete.
fn section<T>(body: T, reasons: &[(bool, SurveyReasonV1)]) -> SurveySectionV1<T> {
    match reasons.iter().find(|(holds, _)| *holds) {
        Some((_, reason)) => SurveySectionV1::partial(*reason, body),
        None => SurveySectionV1::complete(body),
    }
}

/// Measure the head of `git` into the seven sections under the Survey's one deadline, `clock`,
/// which started before the survey key was computed.
pub(super) fn measure(git: &Git, basis: &Basis, clock: Clock) -> Result<Sections, Failure> {
    let policy = basis.policy;
    let reader = |section: &'static str| Reader {
        git,
        clock: clock.section(section),
    };
    let head = basis.head;
    // Set once the deadline passed: the section it stopped is partial, every later one
    // unavailable.
    let mut halted = false;

    // identity: what is known already, the head's declarations, and the Tree Digest.
    let identity_reader = reader("identity");
    let mut identity = SurveyIdentityV1 {
        project_id: basis.project_id.to_string(),
        repository_id: basis.repository_id.to_string(),
        name: None,
        min_af: None,
        af_pin: None,
        head: head.to_string(),
        refs_digest: basis.refs_digest.to_string(),
        tree_digest: None,
        tree_digest_reason: Some(SurveyReasonV1::MaxWallMs),
        cutoff_unix_s: decimal(basis.cutoff),
        policy_digest: basis.policy_digest.to_string(),
        surveyor_version: decimal(basis.surveyor_version),
        git_version: basis.git_version.to_string(),
    };
    let (declared, deadline) = identity_reader.objects(
        &[format!("{head}:.af/af.toml"), format!("{head}:.af/af.lock")],
        policy.max_file_bytes,
    )?;
    halted |= deadline;
    if !halted {
        let content = |index: usize| {
            declared
                .get(index)
                .and_then(Option::as_ref)
                .filter(|object| object.blob)
                .and_then(|object| object.content.as_deref())
        };
        (identity.name, identity.min_af, identity.af_pin) =
            declarations(basis, content(0), content(1));
        match tree_digest(git, &identity_reader.clock, head)? {
            Some(digest) => {
                identity.tree_digest = Some(digest);
                identity.tree_digest_reason = None;
            }
            None => halted = true,
        }
    }
    let identity = section(identity, &[(halted, SurveyReasonV1::MaxWallMs)]);

    // tree: folded as the listing, then the blobs' line counts, stream.
    let mut seen_paths = 0_u64;
    let mut declaring: Vec<Entry> = Vec::new();
    let mut found = Found::default();
    // A bound that stopped the listing: the later sections did not see all of it either.
    let mut listing_cut = None;
    let tree = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        let tree_reader = reader("tree");
        let tree_clock = tree_reader.clock;
        let mut fold = TreeFold::default();
        let stopped = tree_reader.stream(
            "ls-tree",
            &os(&["ls-tree", "-r", "-l", "-z", "--full-tree", head]),
            &[],
            |input| {
                read_listing(input, &tree_clock, |entry| {
                    if any_match(&policy.exclude, &entry.path) {
                        return;
                    }
                    seen_paths += 1;
                    if entry.kind == EntryKind::File {
                        found.add(&entry.path);
                        if is_decision(&entry.path, policy)
                            || entry.path == policy.vocabulary
                            || entry.path == CODE_POLICY
                            || glob_matches(PIPELINES, &entry.path)
                        {
                            declaring.push(entry.clone());
                        }
                    }
                    if seen_paths <= policy.max_paths {
                        fold.add(&entry, policy);
                    }
                })
            },
        )?;
        let mut deadline = stopped == Some(SurveyReasonV1::MaxWallMs);
        listing_cut = stopped.filter(|reason| *reason != SurveyReasonV1::MaxWallMs);
        if !deadline {
            let blobs = fold.blobs();
            if !blobs.is_empty() {
                let stopped = tree_reader.stream(
                    "cat-file",
                    &os(&["cat-file", "--batch"]),
                    &blobs,
                    |input| {
                        read_batch(input, &tree_clock, None, |_, object| {
                            if let Some(object) = object {
                                fold.lines(&object.oid, object.lines);
                            }
                            true
                        })
                    },
                )?;
                deadline = stopped.is_some();
            }
        }
        halted |= deadline;
        let cut_paths = seen_paths > fold.files();
        if deadline && fold.files() == 0 {
            after_deadline()
        } else {
            let body = fold.finish(seen_paths, policy);
            let cut_lists = !body.seen.is_empty();
            section(
                body,
                &[
                    (deadline, SurveyReasonV1::MaxWallMs),
                    (listing_cut.is_some(), SurveyReasonV1::MaxRecordBytes),
                    (cut_paths, SurveyReasonV1::MaxPaths),
                    (cut_lists, SurveyReasonV1::MaxEntries),
                ],
            )
        }
    };

    // history, and the walk contributors folds.
    let mut walk = Walk::new(basis.cutoff, policy.max_paths);
    let mut reachable = 0;
    // A bound that stopped the walk: `max_paths` or `max_record_bytes`.
    let mut walk_cut = None;
    let history = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        let history_reader = reader("history");
        let history_clock = history_reader.clock;
        match history_reader.answer(
            "rev-list",
            &["rev-list", "--count", "--end-of-options", head, "--"],
        )? {
            None => {
                halted = true;
                after_deadline()
            }
            Some(count) => {
                reachable = count
                    .parse::<u64>()
                    .map_err(|_| unreadable("rev-list --count is not a number".into()))?;
                let stopped = history_reader.stream(
                    "log",
                    &os(&[
                        "-c",
                        "log.showRoot=true",
                        "log",
                        "--date-order",
                        "--no-mailmap",
                        "--no-renames",
                        "--diff-merges=off",
                        "--no-textconv",
                        "--no-ext-diff",
                        "--no-color",
                        "--diff-algorithm=myers",
                        "--numstat",
                        "-z",
                        &log_format(policy),
                        &format!("--max-count={}", policy.max_commits),
                        "--end-of-options",
                        head,
                        "--",
                    ]),
                    &[],
                    |input| read_log(input, &history_clock, policy, |commit| walk.add(commit)),
                )?;
                let mut deadline = stopped == Some(SurveyReasonV1::MaxWallMs);
                walk_cut = stopped.filter(|reason| *reason != SurveyReasonV1::MaxWallMs);
                if walk.commits == 0 && !deadline && walk_cut.is_none() {
                    return Err(unreadable("git log reached no commit".into()));
                }
                if walk.commits == 0 {
                    halted |= deadline;
                    SurveySectionV1::unavailable(walk_cut.unwrap_or(SurveyReasonV1::MaxWallMs))
                } else {
                    let (hot, distinct) = walk.hot_paths(policy.hot_paths.min(policy.max_entries));
                    deadline |= history_clock.item();
                    // The hot paths the head no longer holds, asked of the head itself.
                    let mut present = BTreeSet::new();
                    if !deadline && !hot.is_empty() {
                        let mut args = os(&["ls-tree", "-z", "--full-tree", "--name-only", head]);
                        args.push("--".into());
                        args.extend(hot.iter().map(|(path, _)| {
                            use std::os::unix::ffi::OsStringExt;
                            OsString::from_vec(review_core::decode_path(path))
                        }));
                        deadline |= history_reader
                            .stream("ls-tree", &args, &[], |input| {
                                read_records(input, &history_clock, |record| {
                                    present.insert(review_core::encode_path(record));
                                    Ok(())
                                })
                            })?
                            .is_some();
                    }
                    let absent: BTreeSet<String> = if deadline {
                        BTreeSet::new()
                    } else {
                        hot.iter()
                            .map(|(path, _)| path.clone())
                            .filter(|path| !present.contains(path))
                            .collect()
                    };
                    let mut body =
                        history_section(&walk, reachable, hot, distinct, &absent, policy);
                    let mut refs_cut = false;
                    if !deadline {
                        let mut refs = Refs::default();
                        let stopped = history_reader.stream(
                            "for-each-ref",
                            &os(&[
                                "for-each-ref",
                                "--format=%(creatordate:unix)%09%(refname)",
                                "refs/heads",
                                "refs/tags",
                            ]),
                            &[],
                            |input| {
                                read_lines(input, &history_clock, |line, _| refs.line(line, policy))
                            },
                        )?;
                        deadline = stopped == Some(SurveyReasonV1::MaxWallMs);
                        refs_cut = stopped.is_some() && !deadline;
                        if stopped.is_none() {
                            refs.fill(&mut body, policy);
                        }
                    }
                    halted |= deadline;
                    let cut_commits = walk.commits < reachable;
                    let cut_lists = !body.seen.is_empty();
                    section(
                        body,
                        &[
                            (deadline, SurveyReasonV1::MaxWallMs),
                            (
                                refs_cut || walk_cut == Some(SurveyReasonV1::MaxRecordBytes),
                                SurveyReasonV1::MaxRecordBytes,
                            ),
                            (
                                walk_cut == Some(SurveyReasonV1::MaxPaths),
                                SurveyReasonV1::MaxPaths,
                            ),
                            (cut_commits, SurveyReasonV1::MaxCommits),
                            (cut_lists, SurveyReasonV1::MaxEntries),
                        ],
                    )
                }
            }
        }
    };

    // contributors: the same walk, folded by key and ranked.
    let contributors = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else if walk.commits == 0 {
        SurveySectionV1::unavailable(walk_cut.unwrap_or(SurveyReasonV1::MaxWallMs))
    } else {
        let (body, deadline) = contributors_section(&walk, policy, &clock.section("contributors"));
        halted |= deadline;
        let cut_lists = !body.seen.is_empty();
        section(
            body,
            &[
                (deadline, SurveyReasonV1::MaxWallMs),
                (
                    walk_cut == Some(SurveyReasonV1::MaxRecordBytes),
                    SurveyReasonV1::MaxRecordBytes,
                ),
                (
                    walk_cut == Some(SurveyReasonV1::MaxPaths),
                    SurveyReasonV1::MaxPaths,
                ),
                (walk.commits < reachable, SurveyReasonV1::MaxCommits),
                (cut_lists, SurveyReasonV1::MaxEntries),
            ],
        )
    };

    // decisions, vocabulary and checks read their declarations as they stream, one file per
    // item, and keep a content only when it is text and UTF-8.
    let listing_short = listing_cut.is_some();

    let decisions = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        let decisions_reader = reader("decisions");
        let mut matched: Vec<&Entry> = declaring
            .iter()
            .filter(|entry| is_decision(&entry.path, policy))
            .collect();
        matched.sort_by(|a, b| {
            (decision_number(&a.path), &a.path).cmp(&(decision_number(&b.path), &b.path))
        });
        let mut records: Vec<SurveyDecisionV1> = Vec::new();
        let deadline = each_declaration(&decisions_reader, &matched, policy, |entry, content| {
            records.push(decision(&entry.path, content.as_deref()));
            true
        })?;
        halted |= deadline;
        if deadline && records.is_empty() && !matched.is_empty() {
            after_deadline()
        } else {
            let unread = records
                .iter()
                .any(|record| record.status.as_deref() == Some(SURVEY_UNREADABLE));
            let body = decisions_section(records, policy);
            let cut_lists = !body.seen.is_empty();
            section(
                body,
                &[
                    (deadline, SurveyReasonV1::MaxWallMs),
                    (listing_short, SurveyReasonV1::MaxRecordBytes),
                    (cut_lists, SurveyReasonV1::MaxEntries),
                    (unread, SurveyReasonV1::MaxFileBytes),
                ],
            )
        }
    };

    let vocabulary = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        let vocabulary_reader = reader("vocabulary");
        let vocabulary_clock = vocabulary_reader.clock;
        let file: Vec<&Entry> = declaring
            .iter()
            .filter(|entry| entry.path == policy.vocabulary)
            .take(1)
            .collect();
        let mut read = None;
        let mut deadline = each_declaration(&vocabulary_reader, &file, policy, |_, content| {
            let (body, stopped) = vocabulary_section(
                &policy.vocabulary,
                true,
                content.as_deref(),
                policy,
                &vocabulary_clock,
            );
            read = Some(body);
            !stopped
        })?;
        if file.is_empty() {
            let (body, _) =
                vocabulary_section(&policy.vocabulary, false, None, policy, &vocabulary_clock);
            read = Some(body);
        }
        deadline |= clock.passed() && read.is_none();
        halted |= deadline;
        match read {
            // The deadline passed before the file was read.
            None => after_deadline(),
            Some(body) => {
                let unread = body.terms.is_none();
                let cut_lists = !body.seen.is_empty();
                section(
                    body,
                    &[
                        (deadline, SurveyReasonV1::MaxWallMs),
                        (listing_short, SurveyReasonV1::MaxRecordBytes),
                        (cut_lists, SurveyReasonV1::MaxEntries),
                        (unread, SurveyReasonV1::MaxFileBytes),
                    ],
                )
            }
        }
    };

    let checks = if halted || clock.passed() {
        after_deadline()
    } else {
        let checks_reader = reader("checks");
        let checks_clock = checks_reader.clock;
        let mut declaring: Vec<&Entry> = declaring
            .iter()
            .filter(|entry| entry.path == CODE_POLICY || glob_matches(PIPELINES, &entry.path))
            .collect();
        declaring.sort_by(|a, b| a.path.cmp(&b.path));
        let mut fold = ChecksFold::default();
        let deadline = each_declaration(&checks_reader, &declaring, policy, |entry, content| {
            fold.add(&entry.path, content.as_deref(), &checks_clock)
        })?;
        // The toolchain files and workflows the listing found are reached in any case.
        let body = fold.finish(&found, policy);
        let unread = !body.unreadable.is_empty();
        let cut_lists = !body.seen.is_empty();
        section(
            body,
            &[
                (deadline, SurveyReasonV1::MaxWallMs),
                (listing_short, SurveyReasonV1::MaxRecordBytes),
                (cut_lists, SurveyReasonV1::MaxEntries),
                (unread, SurveyReasonV1::MaxFileBytes),
            ],
        )
    };

    Ok(Sections {
        identity,
        tree,
        history,
        contributors,
        decisions,
        vocabulary,
        checks,
    })
}

/// The Tree Digest of `head`: the content digest of its committed capture of the whole tree,
/// the capture every committed Snapshot uses, into a scratch CAS outside the checkout that
/// lives as long as this call. `None` when the capture did not finish before the deadline:
/// never the digest of a part.
fn tree_digest(git: &Git, clock: &Clock, head: &str) -> Result<Option<String>, Failure> {
    let Some(remaining) = clock.remaining() else {
        return Ok(None);
    };
    let failed = |detail: String| {
        Failure::new(
            "the committed capture of the head failed; run `af project refresh` again",
            detail,
        )
    };
    let scratch = tempfile::Builder::new()
        .prefix("af-survey-")
        .tempdir()
        .map_err(|error| failed(format!("creating a scratch directory: {error}")))?;
    let cas = review_store::Cas::open(scratch.path().join("cas"))
        .map_err(|error| failed(error.to_string()))?;
    let repo = git.repo_within(remaining);
    match review_source_git::Capture::new(&repo, &cas).committed(head) {
        Ok(snapshot) if !clock.passed() => Ok(Some(snapshot.content_digest)),
        Ok(_) => Ok(None),
        Err(_) if clock.passed() => Ok(None),
        Err(error) => Err(failed(error.to_string())),
    }
}
