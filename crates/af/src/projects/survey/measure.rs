//! The seven sections of a Survey, measured from the head through git plumbing (ADR-0152).
//!
//! Every git process runs through `projects/git.rs` with what remains of the Survey's one
//! `max_wall_ms` deadline: `cat-file --batch`, `ls-tree`, `rev-list`, `log` and `for-each-ref`,
//! and the committed capture for the Tree Digest. None checks anything out, runs a hook, a
//! filter, a textconv or other project code, or writes inside the checkout. Every stream is read
//! as it arrives, never buffered whole, and every loop over its items (tree entries, blobs,
//! commits, numstat lines, decision files, tags and refs) checks the Clock once per item: a
//! section that reaches the deadline stops at that item and is `partial` with what it reached,
//! every section after it is `unavailable`, and the Survey is still recorded. Any other git
//! failure fails the Survey.
//!
//! The readers and the folds are pure functions of git's bytes, the policy and the cutoff, so a
//! Survey is byte-identical for the same head, refs, policy, cutoff and Surveyor version.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use review_core::project::{
    MAX_SURVEY_TEXT_BYTES, ProjectRegisteredPayloadV1, SURVEY_DECISION_STATUSES,
    SURVEY_EMAIL_HASH_HEX_LEN, SURVEY_NAME_HASH_PREFIX, SURVEY_RELEASE_TAGS, SURVEY_UNREADABLE,
    SurveyAfPinV1, SurveyCheckV1, SurveyChecksV1, SurveyChurnV1, SurveyContributorV1,
    SurveyContributorsV1, SurveyDecisionV1, SurveyDecisionsV1, SurveyDirectoryV1, SurveyHistoryV1,
    SurveyHotPathV1, SurveyIdentityPolicyV1, SurveyIdentityV1, SurveyLanguageV1, SurveyNameV1,
    SurveyReasonV1, SurveySectionV1, SurveySeenV1, SurveyTagsV1, SurveyTestsV1, SurveyTreeV1,
    SurveyVocabularyV1, SurveyWindowV1, SurveyWindowsV1, is_email_shaped, is_survey_text,
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
    ".nvmrc",
    ".python-version",
    ".tool-versions",
    "go.mod",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
];
/// Where a project's code policy declares its checks.
pub(crate) const CODE_POLICY: &str = ".af/code-policy.toml";
/// The review pipelines whose `[[checks]]` `checks` lists: the `.af/` layout's `pipelines/`.
pub(crate) const PIPELINES: &str = ".af/pipelines/*.toml";
/// The CI workflow files `checks` counts.
const WORKFLOWS: [&str; 2] = [".github/workflows/*.yml", ".github/workflows/*.yaml"];
/// A debug build's fixture may make every item of every stream cost this many microseconds, so
/// a test whose subject is the deadline reaches it inside a section. Release binaries never
/// read it.
#[cfg(debug_assertions)]
const TEST_SLOW_ITEM_US: &str = "AF_TEST_SURVEY_SLOW_ITEM_US";

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

/// The Survey's one deadline.
#[derive(Debug, Clone, Copy)]
pub(super) struct Clock {
    deadline: Instant,
    slow: Duration,
}

impl Clock {
    pub(super) fn new(started: Instant, max_wall_ms: u64) -> Self {
        Self {
            deadline: started + Duration::from_millis(max_wall_ms),
            slow: slow_item(),
        }
    }

    fn remaining(&self) -> Option<Duration> {
        let now = Instant::now();
        (now < self.deadline).then(|| self.deadline - now)
    }

    fn passed(&self) -> bool {
        self.remaining().is_none()
    }

    /// One item of a stream: whether the deadline passed before it, where the stream stops.
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

/// How a reader of one git stream ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ended {
    /// The stream ended.
    Done,
    /// The deadline passed before an item; the stream stopped there.
    Deadline,
    /// git wrote what af does not read.
    Bad(String),
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
    /// remains of the deadline. `true` when the deadline passed before the stream ended.
    fn stream<P>(
        &self,
        what: &str,
        args: &[OsString],
        lines: &[String],
        parse: P,
    ) -> Result<bool, Failure>
    where
        P: FnOnce(&mut dyn BufRead) -> Ended + Send,
    {
        let Some(remaining) = self.clock.remaining() else {
            return Ok(true);
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
                Ended::Deadline => Ok(true),
                Ended::Bad(detail) => Err(unreadable(format!("git {what}: {detail}"))),
                Ended::Done if output.status.success() => Ok(false),
                Ended::Done => Err(Self::failed(
                    what,
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                )),
            },
            Err(stopped) if stopped.deadline => Ok(true),
            Err(stopped) => Err(Self::failed(what, stopped.message)),
        }
    }

    /// The objects `names` name, in order, through one `cat-file --batch`: a missing one is
    /// `None`; a content over `keep` bytes is not kept. Also whether the deadline stopped it,
    /// with the objects before it.
    fn objects(&self, names: &[String], keep: u64) -> Result<(Vec<Option<Object>>, bool), Failure> {
        let mut objects = Vec::with_capacity(names.len());
        if names.is_empty() {
            return Ok((objects, false));
        }
        let clock = self.clock;
        let deadline = self.stream("cat-file", &os(&["cat-file", "--batch"]), names, |input| {
            read_batch(input, &clock, Some(keep), |_, object| objects.push(object))
        })?;
        Ok((objects, deadline))
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
/// object requested, in order. `each` receives the request's index and the object; contents of
/// at most `keep` bytes are kept, and every object's newlines are counted as it streams.
pub(super) fn read_batch(
    input: &mut dyn BufRead,
    clock: &Clock,
    keep: Option<u64>,
    mut each: impl FnMut(usize, Option<Object>),
) -> Ended {
    let mut header = Vec::new();
    let mut index = 0;
    loop {
        header.clear();
        match input.read_until(b'\n', &mut header) {
            Ok(0) => return Ended::Done,
            Ok(_) => {}
            Err(error) => return Ended::Bad(error.to_string()),
        }
        if clock.item() {
            return Ended::Deadline;
        }
        let line = String::from_utf8_lossy(header.strip_suffix(b"\n").unwrap_or(&header));
        if line.ends_with(" missing") || line.ends_with(" ambiguous") {
            each(index, None);
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
            Ok((lines, content)) => each(
                index,
                Some(Object {
                    oid: oid.to_string(),
                    blob: *kind == "blob",
                    size,
                    lines,
                    content,
                }),
            ),
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
        record.clear();
        match input.read_until(0, &mut record) {
            Ok(0) => return Ended::Done,
            Ok(_) => {}
            Err(error) => return Ended::Bad(error.to_string()),
        }
        if clock.item() {
            return Ended::Deadline;
        }
        let record = record.strip_suffix(&[0]).unwrap_or(&record);
        if record.is_empty() {
            continue;
        }
        if let Err(detail) = each(record) {
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

/// Read newline-ended lines, one item each, as `for-each-ref` prints them.
fn read_lines(input: &mut dyn BufRead, clock: &Clock, mut each: impl FnMut(&str)) -> Ended {
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(b'\n', &mut line) {
            Ok(0) => return Ended::Done,
            Ok(_) => {}
            Err(error) => return Ended::Bad(error.to_string()),
        }
        if clock.item() {
            return Ended::Deadline;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if !text.is_empty() {
            each(text);
        }
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

/// `tree` over the entries considered (at most `max_paths`, after `exclude`), of the `seen`
/// entries the listing held; `lines` holds the newline count of each blob read, `None` for a
/// binary one, and nothing for a blob not read before the deadline.
pub(super) fn tree_section(
    entries: &[Entry],
    lines: &BTreeMap<String, Option<u64>>,
    seen_paths: u64,
    policy: &Policy,
) -> SurveyTreeV1 {
    let mut directories: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    let mut languages: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let (mut files, mut bytes, mut total_lines, mut without) = (0, 0, 0, 0);
    let mut tests = (0, 0, 0, 0);
    for entry in entries {
        let counted = match entry.kind {
            EntryKind::File if entry.size <= policy.max_file_bytes => {
                lines.get(&entry.oid).copied().flatten()
            }
            _ => None,
        };
        let language = match entry.kind {
            EntryKind::File => policy.language(basename(&entry.path)),
            EntryKind::Symlink | EntryKind::Submodule => "other".into(),
        };
        let entry_lines = counted.unwrap_or(0);
        files += 1;
        bytes += entry.size;
        total_lines += entry_lines;
        without += u64::from(counted.is_none());
        let directory = directories
            .entry(top_level(&entry.path).to_string())
            .or_default();
        directory.0 += 1;
        directory.1 += entry_lines;
        directory.2 += entry.size;
        let language = languages.entry(language).or_default();
        language.0 += 1;
        language.1 += entry_lines;
        if any_match(&policy.tests, &entry.path) {
            tests.0 += 1;
            tests.1 += entry_lines;
        } else {
            tests.2 += 1;
            tests.3 += entry_lines;
        }
    }
    let mut seen = SurveySeenV1::new();
    if seen_paths > files {
        seen.insert("paths".into(), decimal(seen_paths));
    }
    let directories = cut_map(directories, "directories", policy.max_entries, &mut seen);
    let languages = cut_map(languages, "languages", policy.max_entries, &mut seen);
    SurveyTreeV1 {
        files: decimal(files),
        bytes: decimal(bytes),
        lines: decimal(total_lines),
        files_without_lines: decimal(without),
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
    pub(super) identity: String,
    /// The identity is a name cut to [`MAX_SURVEY_TEXT_BYTES`].
    pub(super) truncated: bool,
    /// Agent-assisted: the author or a `Co-Authored-By` name matches the agent patterns.
    pub(super) agent: bool,
    pub(super) changes: Vec<Change>,
}

/// The first 16 hex digits of SHA-256 of `text` lowercased and trimmed.
pub(crate) fn hash16(text: &str) -> String {
    let digest = Sha256::digest(text.trim().to_lowercase().as_bytes());
    review_core::hex::encode(&digest)[..SURVEY_EMAIL_HASH_HEX_LEN].to_string()
}

/// A contributor's identity under the policy, and whether a name was cut. Under `name` an
/// author name that is itself an email address, or that a record cannot hold as text, is `h:`
/// and the 16-hex hash of the name; a longer one is cut to [`MAX_SURVEY_TEXT_BYTES`]. Under
/// `email_hash` it is the 16-hex hash of the email. An email never enters a record.
pub(super) fn identity_of(
    name: &str,
    email: &str,
    policy: SurveyIdentityPolicyV1,
) -> (String, bool) {
    match policy {
        SurveyIdentityPolicyV1::Name => {
            let hashed = || (format!("{SURVEY_NAME_HASH_PREFIX}{}", hash16(name)), false);
            if is_email_shaped(name.trim()) {
                return hashed();
            }
            match display(name) {
                Some((kept, truncated)) if !is_email_shaped(&kept) => (kept, truncated),
                _ => hashed(),
            }
        }
        SurveyIdentityPolicyV1::EmailHash => (hash16(email), false),
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
fn commit_of(fields: &[String], policy: &Policy) -> Result<Commit, String> {
    let [id, parents, time, name, email, trailers] = fields else {
        return Err("a log record without its six fields".into());
    };
    if !review_core::project::is_commit_id(id) {
        return Err("a log record without a commit ID".into());
    }
    let agent = any_match(&policy.agent_authors, name.trim())
        || trailers
            .split('\u{1d}')
            .map(trailer_name)
            .filter(|value| !value.is_empty())
            .any(|value| any_match(&policy.agent_trailers, value));
    let (identity, truncated) = identity_of(name, email, policy.identity);
    Ok(Commit {
        id: id.clone(),
        merge: parents.split_whitespace().count() >= 2,
        time: time
            .trim()
            .parse()
            .map_err(|_| "a committer time that is not a number")?,
        identity,
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
/// receives every commit read to its end.
pub(super) fn read_log(
    input: &mut dyn BufRead,
    clock: &Clock,
    policy: &Policy,
    mut each: impl FnMut(Commit),
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
                each_commit(&mut current, &mut each);
                return Ended::Done;
            }
            Some(RECORD) => {
                input.consume(1);
                each_commit(&mut current, &mut each);
                if clock.item() {
                    return Ended::Deadline;
                }
                let mut fields = Vec::with_capacity(6);
                for _ in 0..6 {
                    line.clear();
                    match input.read_until(UNIT, &mut line) {
                        Ok(_) if line.last() == Some(&UNIT) => {
                            fields.push(String::from_utf8_lossy(&line[..line.len() - 1]).into());
                        }
                        Ok(_) => return Ended::Bad("a log record ends early".into()),
                        Err(error) => return Ended::Bad(error.to_string()),
                    }
                }
                match commit_of(&fields, policy) {
                    Ok(commit) => current = Some(commit),
                    Err(detail) => return Ended::Bad(detail),
                }
            }
            Some(0 | b'\n') => input.consume(1),
            Some(_) => {
                line.clear();
                if let Err(error) = input.read_until(0, &mut line) {
                    return Ended::Bad(error.to_string());
                }
                if clock.item() {
                    return Ended::Deadline;
                }
                let numstat = line.strip_suffix(&[0]).unwrap_or(&line);
                let Some(commit) = current.as_mut() else {
                    return Ended::Bad("a numstat line before its commit".into());
                };
                match change_of(numstat, policy) {
                    Ok(change) => commit.changes.extend(change),
                    Err(detail) => return Ended::Bad(detail),
                }
            }
        }
    }
}

fn each_commit(current: &mut Option<Commit>, each: &mut impl FnMut(Commit)) {
    if let Some(commit) = current.take() {
        each(commit);
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
    agent: u64,
}

impl Walk {
    pub(super) fn new(cutoff: u64) -> Self {
        Self {
            cutoff,
            ..Self::default()
        }
    }

    /// Fold one commit. A merge counts as a commit and a merge and contributes no churn.
    pub(super) fn add(&mut self, commit: Commit) {
        let cutoff = self.cutoff;
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
        let recent = in_window(commit.time, cutoff, 90);
        let changes: &[Change] = if commit.merge { &[] } else { &commit.changes };
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
        let totals = self
            .identities
            .entry(commit.identity)
            .or_insert_with(|| Totals {
                first: u64::MAX,
                ..Totals::default()
            });
        totals.commits += 1;
        totals.added += added;
        totals.removed += removed;
        totals.first = totals.first.min(commit.time);
        totals.last = totals.last.max(commit.time);
        totals.active |= recent;
        totals.truncated |= commit.truncated;
        self.agent += u64::from(commit.agent);
        self.oldest = Some(commit.id);
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

/// The refs `history` counts, read from one `for-each-ref --sort=-creatordate` of `refs/heads`
/// and `refs/tags`, newest first.
#[derive(Debug, Default)]
pub(super) struct Refs {
    branches: u64,
    tags: u64,
    release: Vec<SurveyNameV1>,
    release_seen: u64,
}

impl Refs {
    /// One ref name.
    pub(super) fn add(&mut self, refname: &str, policy: &Policy) {
        if refname.starts_with("refs/heads/") {
            self.branches += 1;
        } else if let Some(tag) = refname.strip_prefix("refs/tags/") {
            self.tags += 1;
            if glob_matches(&policy.release_tags, tag)
                && let Some(name) = named(tag)
            {
                self.release_seen += 1;
                if self.release.len() < SURVEY_RELEASE_TAGS {
                    self.release.push(name);
                }
            }
        }
    }

    /// The tags and the local branch count, and the release tags cut at `max_entries`.
    pub(super) fn fill(mut self, history: &mut SurveyHistoryV1, policy: &Policy) {
        let held = usize::try_from(policy.max_entries).unwrap_or(usize::MAX);
        if self.release.len() > held {
            history
                .seen
                .insert("tags".into(), decimal(self.release_seen));
            self.release.truncate(held);
        }
        history.tags = Some(SurveyTagsV1 {
            count: decimal(self.tags),
            release: self.release,
        });
        history.local_branches = Some(decimal(self.branches));
    }
}

/// `contributors` from the walk: every identity with its totals, the identities with the most
/// commits kept at `max_entries`, and the human and agent-assisted partition, each commit once.
pub(super) fn contributors_section(walk: &Walk, policy: &Policy) -> SurveyContributorsV1 {
    let total = walk.commits;
    let share = |part: u64| decimal((part * 10_000).checked_div(total).unwrap_or(0));
    let top = walk
        .identities
        .values()
        .map(|totals| totals.commits)
        .max()
        .unwrap_or(0);
    let mut ranked: Vec<(&String, &Totals)> = walk.identities.iter().collect();
    ranked.sort_by(|a, b| b.1.commits.cmp(&a.1.commits).then(a.0.cmp(b.0)));
    let mut seen = SurveySeenV1::new();
    cut(&mut ranked, "by_identity", policy.max_entries, &mut seen);
    SurveyContributorsV1 {
        identity: policy.identity,
        identities: decimal(walk.identities.len() as u64),
        active_90d: decimal(
            walk.identities
                .values()
                .filter(|totals| totals.active)
                .count() as u64,
        ),
        human_commits: decimal(total - walk.agent),
        agent_commits: decimal(walk.agent),
        human_share_bp: share(total - walk.agent),
        agent_share_bp: share(walk.agent),
        top_share_bp: share(top),
        by_identity: ranked
            .into_iter()
            .map(|(identity, totals)| {
                (
                    identity.clone(),
                    SurveyContributorV1 {
                        commits: decimal(totals.commits),
                        added: decimal(totals.added),
                        removed: decimal(totals.removed),
                        first_unix_s: decimal(totals.first),
                        last_unix_s: decimal(totals.last),
                        active_90d: totals.active,
                        truncated: totals.truncated,
                    },
                )
            })
            .collect(),
        seen,
    }
}

/// The first `YYYY-MM-DD` of `line` that no digit touches.
fn first_date(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    (0..bytes.len().saturating_sub(9)).find_map(|start| {
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
        (shaped && !touched).then(|| String::from_utf8_lossy(candidate).into_owned())
    })
}

/// The status a line names, as a whole word in any case: the earliest on the line.
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

/// One decision record: number from the file name, title from the first H1, status and date
/// from the first other line that names a status. Without a text content (over
/// `max_file_bytes` or binary) it is `unreadable` and has no title.
pub(super) fn decision(path: &str, content: Option<&[u8]>) -> SurveyDecisionV1 {
    let number = decision_number(path).unwrap_or_default().to_string();
    let Some(text) = content.map(String::from_utf8_lossy) else {
        return SurveyDecisionV1 {
            number,
            path: path.to_string(),
            title: None,
            truncated: false,
            status: Some(SURVEY_UNREADABLE.into()),
            date: None,
        };
    };
    let mut title = None;
    let mut title_line = None;
    for (index, line) in text.lines().enumerate() {
        if let Some(heading) = line.strip_prefix("# ") {
            title = display(heading);
            title_line = Some(index);
            break;
        }
    }
    let mut status = None;
    let mut date = None;
    for (index, line) in text.lines().enumerate() {
        if Some(index) == title_line {
            continue;
        }
        if let Some(named) = named_status(line) {
            status = Some(named.to_string());
            date = first_date(line);
            break;
        }
    }
    let (title, truncated) = title.map_or((None, false), |(title, cut)| (Some(title), cut));
    SurveyDecisionV1 {
        number,
        path: path.to_string(),
        title,
        truncated,
        status,
        date,
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

/// The names of the bold-colon headings, `**Term**:` alone on a line, in file order, each once.
pub(super) fn terms(content: &[u8]) -> Vec<SurveyNameV1> {
    let text = String::from_utf8_lossy(content);
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for line in text.lines() {
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
        if let Some(term) = named(inner)
            && seen.insert(term.name.clone())
        {
            terms.push(term);
        }
    }
    terms
}

/// `vocabulary`: the file, whether the head holds it, and its terms; `content` is `None` when
/// the file is over `max_file_bytes` or not text, which leaves the terms unmeasured.
pub(super) fn vocabulary_section(
    file: &str,
    present: bool,
    content: Option<&[u8]>,
    policy: &Policy,
) -> SurveyVocabularyV1 {
    let mut seen = SurveySeenV1::new();
    let listed = match (present, content) {
        (false, _) => Some(Vec::new()),
        (true, Some(content)) => {
            let mut found = terms(content);
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
    SurveyVocabularyV1 {
        file: file.to_string(),
        present,
        count,
        terms: listed,
        seen,
    }
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
/// `command.program`. Read leniently; a file that does not parse declares nothing here.
pub(super) fn code_policy_checks(content: &[u8]) -> Vec<SurveyCheckV1> {
    let Ok(value) = toml::from_str::<toml::Value>(&String::from_utf8_lossy(content)) else {
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
            check(CODE_POLICY, name, program)
        })
        .collect()
}

/// The `[[checks]]` of one review pipeline: `name` and `program`.
pub(super) fn pipeline_checks(path: &str, content: &[u8]) -> Vec<SurveyCheckV1> {
    let Ok(value) = toml::from_str::<toml::Value>(&String::from_utf8_lossy(content)) else {
        return Vec::new();
    };
    let Some(checks) = value.get("checks").and_then(toml::Value::as_array) else {
        return Vec::new();
    };
    checks
        .iter()
        .filter_map(|declared| {
            check(
                path,
                declared.get("name")?.as_str()?,
                declared.get("program")?.as_str()?,
            )
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

/// `checks` from the declarations read (`None` content: over `max_file_bytes` or not text,
/// listed under `unreadable`) and what the listing found.
pub(super) fn checks_section(
    declarations: &[(&str, Option<&[u8]>)],
    found: &Found,
    policy: &Policy,
) -> SurveyChecksV1 {
    let mut declared = Vec::new();
    let mut unreadable = Vec::new();
    for (path, content) in declarations {
        match content {
            None => unreadable.push(path.to_string()),
            Some(content) if *path == CODE_POLICY => declared.extend(code_policy_checks(content)),
            Some(content) => declared.extend(pipeline_checks(path, content)),
        }
    }
    declared.sort();
    unreadable.sort();
    let mut seen = SurveySeenV1::new();
    cut(&mut declared, "declared", policy.max_entries, &mut seen);
    SurveyChecksV1 {
        declared,
        toolchain: found.toolchain.iter().cloned().collect(),
        ci_workflows: decimal(found.workflows),
        unreadable,
        seen,
    }
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

/// `sha256:` + hex(SHA-256 of the exact stdout of `git for-each-ref
/// --format=%(refname)%09%(objectname) refs/heads refs/tags`), hashed as it streams: the refs
/// of the survey key.
pub(super) fn refs_digest(git: &Git) -> Result<String, String> {
    let args = os(&[
        "for-each-ref",
        "--format=%(refname)%09%(objectname)",
        "refs/heads",
        "refs/tags",
    ]);
    let output = git
        .stream(
            &args,
            |_| Ok::<(), std::io::Error>(()),
            |stdout| -> Result<String, std::io::Error> {
                let mut hasher = Sha256::new();
                let mut chunk = vec![0_u8; STREAM_BUFFER];
                loop {
                    let read = stdout.read(&mut chunk)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&chunk[..read]);
                }
                Ok(format!(
                    "sha256:{}",
                    review_core::hex::encode(&hasher.finalize())
                ))
            },
        )
        .map_err(|stopped| stopped.message)?;
    if !output.status.success() {
        return Err(format!(
            "git for-each-ref: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    output
        .output
        .map_err(|error| format!("git for-each-ref: {error}"))
}

/// The contents of the declarations read, in order: `None` for one over `max_file_bytes` or
/// not text, and none past the file where the deadline stopped the reads, which the flag says.
type Contents = (Vec<Option<Vec<u8>>>, bool);

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

/// Measure the head of `git` into the seven sections under one deadline from `started`.
pub(super) fn measure(git: &Git, basis: &Basis, started: Instant) -> Result<Sections, Failure> {
    let policy = basis.policy;
    let reader = Reader {
        git,
        clock: Clock::new(started, policy.max_wall_ms),
    };
    let clock = reader.clock;
    let head = basis.head;
    // Set once the deadline passed: the section it stopped is partial, every later one
    // unavailable.
    let mut halted = false;

    // identity: what is known already, the head's declarations, and the Tree Digest.
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
    let (declared, deadline) = reader.objects(
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
        match tree_digest(git, &clock, head)? {
            Some(digest) => {
                identity.tree_digest = Some(digest);
                identity.tree_digest_reason = None;
            }
            None => halted = true,
        }
    }
    let identity = section(identity, &[(halted, SurveyReasonV1::MaxWallMs)]);

    // tree: the listing, then the line counts of its text blobs.
    let mut considered: Vec<Entry> = Vec::new();
    let mut seen_paths = 0_u64;
    let mut declaring: Vec<Entry> = Vec::new();
    let mut found = Found::default();
    let tree = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        let deadline = reader.stream(
            "ls-tree",
            &os(&["ls-tree", "-r", "-l", "-z", "--full-tree", head]),
            &[],
            |input| {
                read_listing(input, &clock, |entry| {
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
                        considered.push(entry);
                    }
                })
            },
        )?;
        let mut lines: BTreeMap<String, Option<u64>> = BTreeMap::new();
        if deadline {
            halted = true;
        } else {
            let oids: Vec<String> = considered
                .iter()
                .filter(|entry| {
                    entry.kind == EntryKind::File && entry.size <= policy.max_file_bytes
                })
                .map(|entry| entry.oid.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if !oids.is_empty() {
                halted |=
                    reader.stream("cat-file", &os(&["cat-file", "--batch"]), &oids, |input| {
                        read_batch(input, &clock, None, |_, object| {
                            if let Some(object) = object {
                                lines.insert(object.oid, object.lines);
                            }
                        })
                    })?;
            }
        }
        let body = tree_section(&considered, &lines, seen_paths, policy);
        let cut_paths = seen_paths > considered.len() as u64;
        let cut_lists = !body.seen.is_empty();
        section(
            body,
            &[
                (halted, SurveyReasonV1::MaxWallMs),
                (cut_paths, SurveyReasonV1::MaxPaths),
                (cut_lists, SurveyReasonV1::MaxEntries),
            ],
        )
    };

    // history, and the walk contributors folds.
    let mut walk = Walk::new(basis.cutoff);
    let mut reachable = 0;
    let history = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        match reader.answer(
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
                let deadline = reader.stream(
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
                    |input| read_log(input, &clock, policy, |commit| walk.add(commit)),
                )?;
                halted |= deadline;
                if walk.commits == 0 && !halted {
                    return Err(unreadable("git log reached no commit".into()));
                }
                if walk.commits == 0 {
                    after_deadline()
                } else {
                    let (hot, distinct) = walk.hot_paths(policy.hot_paths.min(policy.max_entries));
                    // The hot paths the head no longer holds, asked of the head itself.
                    let mut present = BTreeSet::new();
                    if !halted && !hot.is_empty() {
                        let mut args = os(&["ls-tree", "-z", "--full-tree", "--name-only", head]);
                        args.push("--".into());
                        args.extend(hot.iter().map(|(path, _)| {
                            use std::os::unix::ffi::OsStringExt;
                            OsString::from_vec(review_core::decode_path(path))
                        }));
                        halted |= reader.stream("ls-tree", &args, &[], |input| {
                            read_records(input, &clock, |record| {
                                present.insert(review_core::encode_path(record));
                                Ok(())
                            })
                        })?;
                    }
                    let absent: BTreeSet<String> = if halted {
                        BTreeSet::new()
                    } else {
                        hot.iter()
                            .map(|(path, _)| path.clone())
                            .filter(|path| !present.contains(path))
                            .collect()
                    };
                    let mut body =
                        history_section(&walk, reachable, hot, distinct, &absent, policy);
                    if !halted {
                        let mut refs = Refs::default();
                        halted |= reader.stream(
                            "for-each-ref",
                            &os(&[
                                "for-each-ref",
                                "--sort=-creatordate",
                                "--format=%(refname)",
                                "refs/heads",
                                "refs/tags",
                            ]),
                            &[],
                            |input| read_lines(input, &clock, |refname| refs.add(refname, policy)),
                        )?;
                        if !halted {
                            refs.fill(&mut body, policy);
                        }
                    }
                    let cut_commits = walk.commits < reachable;
                    let cut_lists = !body.seen.is_empty();
                    section(
                        body,
                        &[
                            (halted, SurveyReasonV1::MaxWallMs),
                            (cut_commits, SurveyReasonV1::MaxCommits),
                            (cut_lists, SurveyReasonV1::MaxEntries),
                        ],
                    )
                }
            }
        }
    };

    // contributors: the same walk, folded by author.
    let contributors = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        let body = contributors_section(&walk, policy);
        let cut_lists = !body.seen.is_empty();
        section(
            body,
            &[
                (walk.commits < reachable, SurveyReasonV1::MaxCommits),
                (cut_lists, SurveyReasonV1::MaxEntries),
            ],
        )
    };

    // decisions, vocabulary and checks read their declarations, one file per item.
    let readable = |entry: &Entry| entry.size <= policy.max_file_bytes;
    let read = |entries: &[&Entry]| -> Result<Contents, Failure> {
        let names: Vec<String> = entries
            .iter()
            .filter(|entry| readable(entry))
            .map(|entry| entry.oid.clone())
            .collect();
        let (objects, deadline) = reader.objects(&names, policy.max_file_bytes)?;
        let mut objects = objects.into_iter();
        let mut contents = Vec::new();
        for entry in entries {
            if !readable(entry) {
                contents.push(None);
                continue;
            }
            match objects.next() {
                Some(object) => contents.push(
                    object
                        .filter(|object| object.lines.is_some())
                        .and_then(|object| object.content),
                ),
                // The deadline stopped the stream before this file.
                None => break,
            }
        }
        Ok((contents, deadline))
    };

    let decisions = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        let mut matched: Vec<&Entry> = declaring
            .iter()
            .filter(|entry| is_decision(&entry.path, policy))
            .collect();
        matched.sort_by(|a, b| {
            (decision_number(&a.path), &a.path).cmp(&(decision_number(&b.path), &b.path))
        });
        let (contents, deadline) = read(&matched)?;
        halted |= deadline;
        let records: Vec<SurveyDecisionV1> = matched
            .iter()
            .zip(&contents)
            .map(|(entry, content)| decision(&entry.path, content.as_deref()))
            .collect();
        let unread = records
            .iter()
            .any(|record| record.status.as_deref() == Some(SURVEY_UNREADABLE));
        let body = decisions_section(records, policy);
        let cut_lists = !body.seen.is_empty();
        section(
            body,
            &[
                (halted, SurveyReasonV1::MaxWallMs),
                (unread, SurveyReasonV1::MaxFileBytes),
                (cut_lists, SurveyReasonV1::MaxEntries),
            ],
        )
    };

    let vocabulary = if halted || clock.passed() {
        halted = true;
        after_deadline()
    } else {
        let file: Vec<&Entry> = declaring
            .iter()
            .filter(|entry| entry.path == policy.vocabulary)
            .take(1)
            .collect();
        let (contents, deadline) = read(&file)?;
        if deadline && contents.is_empty() {
            halted = true;
            after_deadline()
        } else {
            let content = contents.first().cloned().flatten();
            let body = vocabulary_section(
                &policy.vocabulary,
                !file.is_empty(),
                content.as_deref(),
                policy,
            );
            let unread = body.terms.is_none();
            let cut_lists = !body.seen.is_empty();
            section(
                body,
                &[
                    (unread, SurveyReasonV1::MaxFileBytes),
                    (cut_lists, SurveyReasonV1::MaxEntries),
                ],
            )
        }
    };

    let checks = if halted || clock.passed() {
        after_deadline()
    } else {
        let mut declaring: Vec<&Entry> = declaring
            .iter()
            .filter(|entry| entry.path == CODE_POLICY || glob_matches(PIPELINES, &entry.path))
            .collect();
        declaring.sort_by(|a, b| a.path.cmp(&b.path));
        let (contents, deadline) = read(&declaring)?;
        let declarations: Vec<(&str, Option<&[u8]>)> = declaring
            .iter()
            .zip(&contents)
            .map(|(entry, content)| (entry.path.as_str(), content.as_deref()))
            .collect();
        let body = checks_section(&declarations, &found, policy);
        let unread = !body.unreadable.is_empty();
        let cut_lists = !body.seen.is_empty();
        section(
            body,
            &[
                (deadline, SurveyReasonV1::MaxWallMs),
                (unread, SurveyReasonV1::MaxFileBytes),
                (cut_lists, SurveyReasonV1::MaxEntries),
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
