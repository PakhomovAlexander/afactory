//! The seven sections of a Survey, measured from the head through git plumbing (ADR-0152).
//!
//! Every git process runs through [`Git::run_within`] with what remains of the Survey's one
//! `max_wall_ms` deadline: `ls-tree`, `cat-file --batch`, `rev-list`, `log`, `tag` and
//! `for-each-ref`, and the committed capture for the Tree Digest. None checks anything out, runs
//! a hook, a filter, a textconv or other project code, or writes inside the checkout. A section
//! that cannot finish before the deadline is `partial` with what it reached or `unavailable`, and
//! every section after it is `unavailable`; the Survey is still recorded. Any other git failure
//! fails the Survey.
//!
//! The parsers and the folds are pure functions of git's bytes, the policy and the cutoff, so a
//! Survey is byte-identical for the same head, policy, cutoff and Surveyor version.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use review_core::project::{
    MAX_SURVEY_TEXT_BYTES, ProjectRegisteredPayloadV1, SURVEY_DECISION_STATUSES,
    SURVEY_EMAIL_HASH_HEX_LEN, SURVEY_NEWEST_DECISIONS, SURVEY_RELEASE_TAGS, SurveyAfPinV1,
    SurveyCheckV1, SurveyChecksV1, SurveyChurnV1, SurveyContributorV1, SurveyContributorsV1,
    SurveyDecisionV1, SurveyDecisionsV1, SurveyDirectoryV1, SurveyHistoryV1, SurveyHotPathV1,
    SurveyIdentityPolicyV1, SurveyIdentityV1, SurveyLanguageV1, SurveyReasonV1, SurveySectionV1,
    SurveyTagsV1, SurveyTestsV1, SurveyTreeV1, SurveyVocabularyV1, SurveyWindowV1, SurveyWindowsV1,
    is_survey_text,
};
use review_core::task::usage::DecimalU64;
use sha2::{Digest, Sha256};

use super::glob;
use super::policy::Policy;
use crate::projects::Git;

const DAY_S: u64 = 86_400;
/// A blob with a NUL in its first 8 KiB is binary and has no line count.
const BINARY_PROBE_BYTES: usize = 8 * 1024;
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

fn decimal(value: u64) -> DecimalU64 {
    value.into()
}

/// Why a git read gave nothing: the deadline passed, or git failed.
#[derive(Debug)]
pub(super) enum Halt {
    Deadline,
    Failed(Failure),
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

/// The Survey's one deadline.
#[derive(Debug, Clone, Copy)]
pub(super) struct Clock {
    deadline: Instant,
}

impl Clock {
    pub(super) fn new(started: Instant, max_wall_ms: u64) -> Self {
        Self {
            deadline: started + Duration::from_millis(max_wall_ms),
        }
    }

    fn remaining(&self) -> Option<Duration> {
        let now = Instant::now();
        (now < self.deadline).then(|| self.deadline - now)
    }

    fn passed(&self) -> bool {
        self.remaining().is_none()
    }
}

/// Plumbing under the clock.
struct Reader<'a> {
    git: &'a Git,
    clock: Clock,
}

impl Reader<'_> {
    /// git's stdout, requiring success, under what remains of the deadline.
    fn read(&self, what: &str, args: &[&str], input: Option<Vec<u8>>) -> Result<Vec<u8>, Halt> {
        let remaining = self.clock.remaining().ok_or(Halt::Deadline)?;
        let failed = |detail: String| {
            Halt::Failed(Failure::new(
                format!(
                    "git {what} failed while surveying the head; run `af project refresh` again"
                ),
                detail,
            ))
        };
        match self.git.run_within(args, input, remaining) {
            Ok(output) if output.status.success() => Ok(output.stdout),
            Ok(output) => Err(failed(format!(
                "git {what}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
            Err(stopped) if stopped.deadline => Err(Halt::Deadline),
            Err(stopped) => Err(failed(stopped.message)),
        }
    }

    /// The contents of `oids` through one `cat-file --batch`; a missing object is absent.
    fn blobs(&self, oids: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>, Halt> {
        if oids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut input = Vec::new();
        for oid in oids {
            input.extend_from_slice(oid.as_bytes());
            input.push(b'\n');
        }
        let output = self.read("cat-file", &["cat-file", "--batch"], Some(input))?;
        parse_batch(&output).map_err(|detail| {
            Halt::Failed(Failure::new(
                "git cat-file answered in a shape af does not read; run `af project refresh` again",
                detail,
            ))
        })
    }
}

/// `cat-file --batch` output: `<oid> <type> <size>\n<content>\n`, or `<oid> missing\n`.
pub(super) fn parse_batch(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut blobs = BTreeMap::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = bytes[at..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| at + offset)
            .ok_or("an unterminated header")?;
        let header = String::from_utf8_lossy(&bytes[at..end]).into_owned();
        at = end + 1;
        let fields: Vec<&str> = header.split(' ').collect();
        match fields.as_slice() {
            [_, "missing" | "ambiguous"] => continue,
            [oid, _, size] => {
                let size: usize = size
                    .parse()
                    .map_err(|_| format!("the header `{header}` has no size"))?;
                let content = bytes
                    .get(at..at + size)
                    .ok_or_else(|| format!("{oid} is shorter than its {size} bytes"))?;
                blobs.insert(oid.to_string(), content.to_vec());
                at += size + 1;
            }
            _ => return Err(format!("the header `{header}`")),
        }
    }
    Ok(blobs)
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

/// `<mode> SP <type> SP <oid> SP+ <size> TAB <path> NUL`, in tree order.
pub(super) fn parse_listing(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for record in bytes.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
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
        entries.push(Entry {
            kind,
            oid: oid.to_string(),
            size: if kind == EntryKind::Submodule {
                0
            } else {
                size.parse()
                    .map_err(|_| format!("the ls-tree size `{size}`"))?
            },
            path: review_core::encode_path(&record[tab + 1..]),
        });
    }
    Ok(entries)
}

/// The top-level directory of a path; `.` for a root-level file.
pub(super) fn top_level(path: &str) -> &str {
    path.split_once('/').map_or(".", |(top, _)| top)
}

fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Newlines of a text blob; `None` for a binary one.
pub(super) fn text_lines(content: &[u8]) -> Option<u64> {
    let probe = &content[..content.len().min(BINARY_PROBE_BYTES)];
    if probe.contains(&0) {
        return None;
    }
    Some(content.iter().filter(|byte| **byte == b'\n').count() as u64)
}

/// The blobs whose lines `tree` counts: files at or under `max_file_bytes`.
pub(super) fn text_candidates(entries: &[Entry], policy: &Policy) -> BTreeSet<String> {
    entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File && entry.size <= policy.max_file_bytes)
        .map(|entry| entry.oid.clone())
        .collect()
}

/// `tree` over the entries left after `exclude`. Without `blobs` (the deadline passed before
/// they were read) every file is one without a line count.
pub(super) fn tree_section(
    entries: &[Entry],
    blobs: Option<&BTreeMap<String, Vec<u8>>>,
    policy: &Policy,
) -> SurveyTreeV1 {
    let mut directories: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    let mut languages: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let (mut files, mut bytes, mut lines, mut without) = (0, 0, 0, 0);
    let mut tests = (0, 0, 0, 0);
    for entry in entries {
        let counted = match entry.kind {
            EntryKind::File if entry.size <= policy.max_file_bytes => blobs
                .and_then(|blobs| blobs.get(&entry.oid))
                .and_then(|content| text_lines(content)),
            _ => None,
        };
        let language = match entry.kind {
            EntryKind::File => policy.language(basename(&entry.path)),
            EntryKind::Symlink | EntryKind::Submodule => "other".into(),
        };
        let entry_lines = counted.unwrap_or(0);
        files += 1;
        bytes += entry.size;
        lines += entry_lines;
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
        if glob::any_path(&policy.tests, &entry.path) {
            tests.0 += 1;
            tests.1 += entry_lines;
        } else {
            tests.2 += 1;
            tests.3 += entry_lines;
        }
    }
    SurveyTreeV1 {
        files: decimal(files),
        bytes: decimal(bytes),
        lines: decimal(lines),
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
    }
}

/// One `--numstat` line of a commit, after `exclude`; a binary change adds and removes nothing.
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
    pub(super) agent: bool,
    pub(super) changes: Vec<Change>,
}

/// The first 16 hex digits of SHA-256 of `text` lowercased and trimmed.
pub(crate) fn hash16(text: &str) -> String {
    let digest = Sha256::digest(text.trim().to_lowercase().as_bytes());
    review_core::hex::encode(&digest)[..SURVEY_EMAIL_HASH_HEX_LEN].to_string()
}

/// A contributor's identity under the policy. A name that is blank, too long, or holds `@` is
/// recorded by the `email_hash` rule applied to the name, so an address used as a name never
/// enters a record.
pub(super) fn identity_of(name: &str, email: &str, policy: SurveyIdentityPolicyV1) -> String {
    match policy {
        SurveyIdentityPolicyV1::Name => {
            let name = name.trim();
            if is_survey_text(name) && !name.contains('@') {
                name.to_string()
            } else {
                hash16(name)
            }
        }
        SurveyIdentityPolicyV1::EmailHash => hash16(email),
    }
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

/// Parse `log -z --numstat` in [`log_format`], deriving each identity and agent flag as it goes:
/// an email is hashed, or never asked for, and is never kept.
pub(super) fn parse_log(bytes: &[u8], policy: &Policy) -> Result<Vec<Commit>, String> {
    const RECORD: u8 = 0x1e;
    const UNIT: u8 = 0x1f;
    let mut commits = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != RECORD {
            return Err("a log record does not start where af expects one".into());
        }
        at += 1;
        let mut fields = Vec::with_capacity(6);
        for _ in 0..6 {
            let end = bytes[at..]
                .iter()
                .position(|byte| *byte == UNIT)
                .map(|offset| at + offset)
                .ok_or("a log record ends early")?;
            fields.push(String::from_utf8_lossy(&bytes[at..end]).into_owned());
            at = end + 1;
        }
        while at < bytes.len() && matches!(bytes[at], 0 | b'\n') {
            at += 1;
        }
        let mut changes = Vec::new();
        while at < bytes.len() && bytes[at] != RECORD {
            let end = bytes[at..]
                .iter()
                .position(|byte| *byte == 0)
                .map_or(bytes.len(), |offset| at + offset);
            let line = &bytes[at..end];
            at = end + 1;
            let line = line.strip_prefix(b"\n").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            let mut parts = line.splitn(3, |byte| *byte == b'\t');
            let (Some(added), Some(removed), Some(path)) =
                (parts.next(), parts.next(), parts.next())
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
            if glob::any_path(&policy.exclude, &path) {
                continue;
            }
            changes.push(Change {
                added: count(added)?,
                removed: count(removed)?,
                path,
            });
        }
        let [id, parents, time, name, email, trailers] = fields.as_slice() else {
            unreachable!("six fields were read");
        };
        if !review_core::project::is_commit_id(id) {
            return Err("a log record without a commit ID".into());
        }
        let agent = glob::any_name(&policy.agent_authors, name.trim())
            || trailers
                .split('\u{1d}')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .any(|value| glob::any_name(&policy.agent_trailers, value));
        commits.push(Commit {
            id: id.clone(),
            merge: parents.split_whitespace().count() >= 2,
            time: time
                .trim()
                .parse()
                .map_err(|_| "a committer time that is not a number")?,
            identity: identity_of(name, email, policy.identity),
            agent,
            changes,
        });
    }
    Ok(commits)
}

/// The windows' starts: the cutoff minus 30, 90 and 365 days.
fn since(cutoff: u64, days: u64) -> u64 {
    cutoff.saturating_sub(days * DAY_S)
}

/// `history` from the commits reached, without its tags and branches.
pub(super) fn history_section(
    commits: &[Commit],
    reachable: u64,
    cutoff: u64,
    head_paths: &BTreeSet<&str>,
    policy: &Policy,
) -> SurveyHistoryV1 {
    let window = |days: u64| {
        let start = since(cutoff, days);
        let inside: Vec<&Commit> = commits.iter().filter(|c| c.time >= start).collect();
        SurveyWindowV1 {
            commits: decimal(inside.len() as u64),
            merges: decimal(inside.iter().filter(|c| c.merge).count() as u64),
        }
    };
    let recent = since(cutoff, 90);
    let mut churn: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    let mut touched: BTreeMap<&str, u64> = BTreeMap::new();
    for commit in commits.iter().filter(|commit| commit.time >= recent) {
        let mut directories = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for change in &commit.changes {
            let directory = churn
                .entry(top_level(&change.path).to_string())
                .or_default();
            directory.1 += change.added;
            directory.2 += change.removed;
            directories.insert(top_level(&change.path));
            paths.insert(change.path.as_str());
        }
        for directory in directories {
            churn.get_mut(directory).expect("entered above").0 += 1;
        }
        for path in paths {
            *touched.entry(path).or_default() += 1;
        }
    }
    let mut hot: Vec<(&str, u64)> = touched.into_iter().collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    hot.truncate(policy.hot_paths as usize);
    let cut = (commits.len() as u64) < reachable;
    SurveyHistoryV1 {
        reachable_commits: decimal(reachable),
        commits: decimal(commits.len() as u64),
        merges: decimal(commits.iter().filter(|c| c.merge).count() as u64),
        windows: SurveyWindowsV1 {
            days_30: window(30),
            days_90: window(90),
            days_365: window(365),
        },
        first_commit_unix_s: decimal(commits.iter().map(|c| c.time).min().unwrap_or(0)),
        last_commit_unix_s: decimal(commits.iter().map(|c| c.time).max().unwrap_or(0)),
        oldest_reached: cut
            .then(|| commits.last().map(|commit| commit.id.clone()))
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
                path: path.to_string(),
                commits: decimal(commits),
                removed: (!head_paths.contains(path)).then_some(true),
            })
            .collect(),
        tags: None,
        local_branches: None,
    }
}

/// `tags`: the count of every tag, and the newest five matching `release_tags`, from
/// `tag --list --sort=-creatordate`.
pub(super) fn tags_section(listed: &[u8], policy: &Policy) -> SurveyTagsV1 {
    let names: Vec<String> = String::from_utf8_lossy(listed)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    SurveyTagsV1 {
        count: decimal(names.len() as u64),
        release: names
            .iter()
            .filter(|name| is_survey_text(name) && glob::name_matches(&policy.release_tags, name))
            .take(SURVEY_RELEASE_TAGS)
            .cloned()
            .collect(),
    }
}

/// `contributors` from the commits reached.
pub(super) fn contributors_section(
    commits: &[Commit],
    cutoff: u64,
    policy: &Policy,
) -> SurveyContributorsV1 {
    let recent = since(cutoff, 90);
    // commits, added, removed, first, last, active
    let mut by_identity: BTreeMap<String, (u64, u64, u64, u64, u64, bool)> = BTreeMap::new();
    for commit in commits {
        let entry =
            by_identity
                .entry(commit.identity.clone())
                .or_insert((0, 0, 0, u64::MAX, 0, false));
        entry.0 += 1;
        entry.1 += commit
            .changes
            .iter()
            .map(|change| change.added)
            .sum::<u64>();
        entry.2 += commit
            .changes
            .iter()
            .map(|change| change.removed)
            .sum::<u64>();
        entry.3 = entry.3.min(commit.time);
        entry.4 = entry.4.max(commit.time);
        entry.5 |= commit.time >= recent;
    }
    let total = commits.len() as u64;
    let share = |part: u64| decimal((part * 10_000).checked_div(total).unwrap_or(0));
    let agent = commits.iter().filter(|commit| commit.agent).count() as u64;
    let top = by_identity.values().map(|entry| entry.0).max().unwrap_or(0);
    SurveyContributorsV1 {
        identity: policy.identity,
        identities: decimal(by_identity.len() as u64),
        active_90d: decimal(by_identity.values().filter(|entry| entry.5).count() as u64),
        human_commits: decimal(total - agent),
        agent_commits: decimal(agent),
        human_share_bp: share(total - agent),
        agent_share_bp: share(agent),
        top_share_bp: share(top),
        by_identity: by_identity
            .into_iter()
            .map(|(identity, entry)| {
                (
                    identity,
                    SurveyContributorV1 {
                        commits: decimal(entry.0),
                        added: decimal(entry.1),
                        removed: decimal(entry.2),
                        first_unix_s: decimal(entry.3),
                        last_unix_s: decimal(entry.4),
                        active_90d: entry.5,
                    },
                )
            })
            .collect(),
    }
}

/// `text` cut to the bound a Survey copies, at a character boundary, or nothing when it is
/// not text a record may hold.
fn bounded(text: &str) -> Option<String> {
    let text = text.trim();
    let mut end = text.len().min(MAX_SURVEY_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = text[..end].trim();
    is_survey_text(text).then(|| text.to_string())
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

/// One decision record: number from the file name, title from the first H1, status and date
/// from the first other line that names a status.
pub(super) fn decision(path: &str, content: Option<&[u8]>) -> (Option<SurveyDecisionV1>, String) {
    let number: String = basename(path)
        .chars()
        .take_while(char::is_ascii_digit)
        .take(20)
        .collect();
    let text = content
        .filter(|content| text_lines(content).is_some())
        .map(String::from_utf8_lossy);
    let mut title = None;
    let mut status = None;
    let mut date = None;
    if let Some(text) = &text {
        let mut title_line = None;
        for (index, line) in text.lines().enumerate() {
            if let Some(heading) = line.strip_prefix("# ") {
                title = bounded(heading);
                title_line = Some(index);
                break;
            }
        }
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
    }
    let counted = status.clone().unwrap_or_else(|| "unknown".into());
    let record = (!number.is_empty()).then(|| SurveyDecisionV1 {
        number,
        path: path.to_string(),
        title,
        status,
        date,
    });
    (record, counted)
}

/// `decisions` over the matched records and their contents.
pub(super) fn decisions_section(records: &[(&str, Option<&[u8]>)]) -> SurveyDecisionsV1 {
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    let mut numbered = Vec::new();
    for (path, content) in records {
        let (record, status) = decision(path, *content);
        *by_status.entry(status).or_default() += 1;
        numbered.extend(record);
    }
    let value = |number: &str| {
        let digits = number.trim_start_matches('0');
        (digits.len(), digits.to_string())
    };
    numbered.sort_by(|a: &SurveyDecisionV1, b| {
        value(&b.number)
            .cmp(&value(&a.number))
            .then(a.path.cmp(&b.path))
    });
    numbered.truncate(SURVEY_NEWEST_DECISIONS);
    SurveyDecisionsV1 {
        count: decimal(records.len() as u64),
        by_status: by_status
            .into_iter()
            .map(|(status, count)| (status, decimal(count)))
            .collect(),
        newest: numbered,
    }
}

/// The names of the bold-colon headings, `**Term**:` alone on a line, in file order, each once.
pub(super) fn terms(content: &[u8]) -> Vec<String> {
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
        if let Some(term) = bounded(inner)
            && seen.insert(term.clone())
        {
            terms.push(term);
        }
    }
    terms
}

/// A program as a record may hold it: an absolute path becomes its file name.
fn program_name(program: &str) -> Option<String> {
    let program = if program.starts_with('/') {
        basename(program)
    } else {
        program
    };
    bounded(program)
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
        .filter_map(|(key, check)| {
            let name = check
                .get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            let program = check
                .get("command")
                .and_then(|command| command.get("program"))
                .and_then(toml::Value::as_str)?;
            Some(SurveyCheckV1 {
                source: CODE_POLICY.into(),
                name: bounded(name)?,
                program: program_name(program)?,
            })
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
        .filter_map(|check| {
            Some(SurveyCheckV1 {
                source: path.to_string(),
                name: bounded(check.get("name")?.as_str()?)?,
                program: program_name(check.get("program")?.as_str()?)?,
            })
        })
        .collect()
}

/// `checks` from the declarations read and the tree's entries.
pub(super) fn checks_section(
    entries: &[Entry],
    code_policy: Option<&[u8]>,
    pipelines: &[(&str, &[u8])],
) -> SurveyChecksV1 {
    let mut declared: Vec<SurveyCheckV1> = code_policy.map(code_policy_checks).unwrap_or_default();
    for (path, content) in pipelines {
        declared.extend(pipeline_checks(path, content));
    }
    declared.sort();
    let present: BTreeSet<&str> = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .map(|entry| entry.path.as_str())
        .collect();
    let mut toolchain: Vec<String> = TOOLCHAIN_FILES
        .iter()
        .filter(|name| present.contains(**name))
        .map(|name| name.to_string())
        .collect();
    toolchain.sort();
    let workflows = present
        .iter()
        .filter(|path| {
            glob::path_matches(".github/workflows/*.yml", path)
                || glob::path_matches(".github/workflows/*.yaml", path)
        })
        .count();
    SurveyChecksV1 {
        declared,
        toolchain,
        ci_workflows: decimal(workflows as u64),
    }
}

/// What the Surveyor knows before it reads anything: the Project and the head, the policy and
/// the cutoff.
pub(super) struct Basis<'a> {
    pub(super) project_id: &'a str,
    pub(super) repository_id: &'a str,
    pub(super) head: &'a str,
    pub(super) policy: &'a Policy,
    pub(super) policy_digest: &'a str,
    pub(super) cutoff: u64,
    pub(super) surveyor_version: u64,
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

/// Measure the head of `git` into the seven sections under one deadline from `started`.
pub(super) fn measure(git: &Git, basis: &Basis, started: Instant) -> Result<Sections, Failure> {
    let policy = basis.policy;
    let reader = Reader {
        git,
        clock: Clock::new(started, policy.max_wall_ms),
    };
    let fail = |halt: Halt| match halt {
        Halt::Failed(failure) => Err(failure),
        Halt::Deadline => Ok(()),
    };
    let unreadable = |detail: String| {
        Failure::new(
            "git answered in a shape af does not read; run `af project refresh` again",
            detail,
        )
    };

    // The head tree, every later section's source.
    let listing = match reader.read(
        "ls-tree",
        &["ls-tree", "-r", "-l", "-z", "--full-tree", basis.head],
        None,
    ) {
        Ok(bytes) => Some(parse_listing(&bytes).map_err(unreadable)?),
        Err(halt) => {
            fail(halt)?;
            None
        }
    };

    // identity: what is known already, the head's declarations, and the Tree Digest.
    let mut identity = SurveyIdentityV1 {
        project_id: basis.project_id.to_string(),
        repository_id: basis.repository_id.to_string(),
        name: None,
        min_af: None,
        af_pin: None,
        head: basis.head.to_string(),
        tree_digest: None,
        cutoff_unix_s: decimal(basis.cutoff),
        policy_digest: basis.policy_digest.to_string(),
        surveyor_version: decimal(basis.surveyor_version),
    };
    let mut halted = listing.is_none();
    if let Some(listing) = &listing {
        let oid_of = |path: &str| {
            listing
                .iter()
                .find(|entry| entry.kind == EntryKind::File && entry.path == path)
                .map(|entry| entry.oid.clone())
        };
        let (toml_oid, lock_oid) = (oid_of(".af/af.toml"), oid_of(".af/af.lock"));
        let wanted: BTreeSet<String> = toml_oid.iter().chain(&lock_oid).cloned().collect();
        match reader.blobs(&wanted) {
            Ok(blobs) => {
                let read = |oid: &Option<String>| {
                    oid.as_ref()
                        .and_then(|oid| blobs.get(oid))
                        .map(Vec::as_slice)
                };
                (identity.name, identity.min_af, identity.af_pin) =
                    declarations(basis, read(&toml_oid), read(&lock_oid));
            }
            Err(halt) => {
                fail(halt)?;
                halted = true;
            }
        }
        if !halted {
            match tree_digest(git, &reader.clock, basis.head) {
                Ok(digest) => identity.tree_digest = Some(digest),
                Err(halt) => {
                    fail(halt)?;
                    halted = true;
                }
            }
        }
    }
    let identity = if halted {
        SurveySectionV1::partial(SurveyReasonV1::MaxWallMs, identity)
    } else {
        SurveySectionV1::complete(identity)
    };
    let all: &[Entry] = listing.as_deref().unwrap_or(&[]);
    let entries: Vec<Entry> = all
        .iter()
        .filter(|entry| !glob::any_path(&policy.exclude, &entry.path))
        .cloned()
        .collect();
    let start = |halted: &mut bool| {
        *halted |= reader.clock.passed();
        !*halted
    };

    // tree
    let tree = if start(&mut halted) {
        match reader.blobs(&text_candidates(&entries, policy)) {
            Ok(blobs) => SurveySectionV1::complete(tree_section(&entries, Some(&blobs), policy)),
            Err(halt) => {
                fail(halt)?;
                halted = true;
                SurveySectionV1::partial(
                    SurveyReasonV1::MaxWallMs,
                    tree_section(&entries, None, policy),
                )
            }
        }
    } else {
        SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
    };

    // history, and the commits contributors folds
    let mut commits = None;
    let history = if start(&mut halted) {
        let walked = reader
            .read(
                "rev-list",
                &["rev-list", "--count", "--end-of-options", basis.head, "--"],
                None,
            )
            .and_then(|count| {
                let count = String::from_utf8_lossy(&count).trim().parse::<u64>();
                let log = reader.read(
                    "log",
                    &[
                        "-c",
                        "log.showRoot=true",
                        "log",
                        "--no-mailmap",
                        "--no-renames",
                        "--no-textconv",
                        "--no-ext-diff",
                        "--no-color",
                        "--diff-algorithm=myers",
                        "--numstat",
                        "-z",
                        &log_format(policy),
                        &format!("--max-count={}", policy.max_commits),
                        "--end-of-options",
                        basis.head,
                        "--",
                    ],
                    None,
                )?;
                Ok((count, log))
            });
        match walked {
            Ok((count, log)) => {
                let reachable =
                    count.map_err(|_| unreadable("rev-list --count is not a number".into()))?;
                let reached = parse_log(&log, policy).map_err(unreadable)?;
                if reached.is_empty() {
                    return Err(unreadable("git log reached no commit".into()));
                }
                let head_paths: BTreeSet<&str> =
                    all.iter().map(|entry| entry.path.as_str()).collect();
                let mut body =
                    history_section(&reached, reachable, basis.cutoff, &head_paths, policy);
                let cut = body.oldest_reached.is_some();
                commits = Some((reached, cut));
                let tags = reader.read(
                    "tag",
                    &["tag", "--list", "--sort=-creatordate", "--no-column"],
                    None,
                );
                match tags {
                    Ok(listed) => body.tags = Some(tags_section(&listed, policy)),
                    Err(halt) => {
                        fail(halt)?;
                        halted = true;
                    }
                }
                if !halted {
                    match reader.read(
                        "for-each-ref",
                        &["for-each-ref", "--format=%(refname)", "refs/heads"],
                        None,
                    ) {
                        Ok(heads) => {
                            let count = String::from_utf8_lossy(&heads)
                                .lines()
                                .filter(|line| !line.trim().is_empty())
                                .count();
                            body.local_branches = Some(decimal(count as u64));
                        }
                        Err(halt) => {
                            fail(halt)?;
                            halted = true;
                        }
                    }
                }
                if halted {
                    SurveySectionV1::partial(SurveyReasonV1::MaxWallMs, body)
                } else if cut {
                    SurveySectionV1::partial(SurveyReasonV1::MaxCommits, body)
                } else {
                    SurveySectionV1::complete(body)
                }
            }
            Err(halt) => {
                fail(halt)?;
                halted = true;
                SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
            }
        }
    } else {
        SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
    };

    // contributors
    let contributors = match (&commits, start(&mut halted)) {
        (Some((reached, cut)), true) => {
            let body = contributors_section(reached, basis.cutoff, policy);
            if *cut {
                SurveySectionV1::partial(SurveyReasonV1::MaxCommits, body)
            } else {
                SurveySectionV1::complete(body)
            }
        }
        _ => SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs),
    };

    // decisions
    let readable = |entry: &&Entry| entry.kind == EntryKind::File;
    let decisions = if start(&mut halted) {
        let matched: Vec<&Entry> = entries
            .iter()
            .filter(readable)
            .filter(|entry| glob::path_matches(&policy.decisions, &entry.path))
            .collect();
        let wanted = matched
            .iter()
            .filter(|entry| entry.size <= policy.max_file_bytes)
            .map(|entry| entry.oid.clone())
            .collect();
        match reader.blobs(&wanted) {
            Ok(blobs) => {
                let records: Vec<(&str, Option<&[u8]>)> = matched
                    .iter()
                    .map(|entry| {
                        let content = (entry.size <= policy.max_file_bytes)
                            .then(|| blobs.get(&entry.oid).map(Vec::as_slice))
                            .flatten();
                        (entry.path.as_str(), content)
                    })
                    .collect();
                SurveySectionV1::complete(decisions_section(&records))
            }
            Err(halt) => {
                fail(halt)?;
                halted = true;
                SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
            }
        }
    } else {
        SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
    };

    // vocabulary
    let vocabulary = if start(&mut halted) {
        let file = entries
            .iter()
            .filter(readable)
            .find(|entry| entry.path == policy.vocabulary);
        let wanted = file
            .filter(|entry| entry.size <= policy.max_file_bytes)
            .map(|entry| entry.oid.clone())
            .into_iter()
            .collect();
        match reader.blobs(&wanted) {
            Ok(blobs) => {
                let found = file
                    .and_then(|entry| blobs.get(&entry.oid))
                    .filter(|content| text_lines(content).is_some())
                    .map(|content| terms(content))
                    .unwrap_or_default();
                SurveySectionV1::complete(SurveyVocabularyV1 {
                    file: policy.vocabulary.clone(),
                    present: file.is_some(),
                    count: decimal(found.len() as u64),
                    terms: found,
                })
            }
            Err(halt) => {
                fail(halt)?;
                halted = true;
                SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
            }
        }
    } else {
        SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
    };

    // checks
    let checks = if start(&mut halted) {
        let declaring: Vec<&Entry> = entries
            .iter()
            .filter(readable)
            .filter(|entry| entry.size <= policy.max_file_bytes)
            .filter(|entry| entry.path == CODE_POLICY || glob::path_matches(PIPELINES, &entry.path))
            .collect();
        let wanted = declaring.iter().map(|entry| entry.oid.clone()).collect();
        match reader.blobs(&wanted) {
            Ok(blobs) => {
                let content = |entry: &Entry| blobs.get(&entry.oid).map(Vec::as_slice);
                let code_policy = declaring
                    .iter()
                    .find(|entry| entry.path == CODE_POLICY)
                    .and_then(|entry| content(entry));
                let pipelines: Vec<(&str, &[u8])> = declaring
                    .iter()
                    .filter(|entry| entry.path != CODE_POLICY)
                    .filter_map(|entry| Some((entry.path.as_str(), content(entry)?)))
                    .collect();
                SurveySectionV1::complete(checks_section(&entries, code_policy, &pipelines))
            }
            Err(halt) => {
                fail(halt)?;
                SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
            }
        }
    } else {
        SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs)
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

/// The Tree Digest of `head`: the content digest of its committed capture, the capture every
/// committed Snapshot uses, into a scratch CAS outside the checkout that lives as long as this
/// call.
fn tree_digest(git: &Git, clock: &Clock, head: &str) -> Result<String, Halt> {
    let remaining = clock.remaining().ok_or(Halt::Deadline)?;
    let failed = |detail: String| {
        Halt::Failed(Failure::new(
            "the committed capture of the head failed; run `af project refresh` again",
            detail,
        ))
    };
    let scratch = tempfile::Builder::new()
        .prefix("af-survey-")
        .tempdir()
        .map_err(|error| failed(format!("creating a scratch directory: {error}")))?;
    let cas = review_store::Cas::open(scratch.path().join("cas"))
        .map_err(|error| failed(error.to_string()))?;
    let repo = git.repo_within(remaining);
    match review_source_git::Capture::new(&repo, &cas).committed(head) {
        Ok(snapshot) if !clock.passed() => Ok(snapshot.content_digest),
        Ok(_) => Err(Halt::Deadline),
        Err(_) if clock.passed() => Err(Halt::Deadline),
        Err(error) => Err(failed(error.to_string())),
    }
}
