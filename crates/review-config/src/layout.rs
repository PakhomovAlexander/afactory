//! The canonical `.af/` layout, declared once.
//!
//! Git holds declarations: configuration, pipeline definitions, Worker and Task packages, checks
//! and policy. Everything the kernel records — Task files, run state, candidate patches,
//! transcripts, reviewer results, receipts, logs and measurements — belongs to the Store under
//! `$XDG_STATE_HOME/af`, never to a checkout.
//!
//! [`LAYOUT`] is the only list. `af help config`, the rendered documentation and every
//! classification read it, because a second list would eventually become a second answer. A test
//! walks the kernel's own sources and refuses any authority path this table does not name, so the
//! table cannot fall behind the code that resolves it.
//!
//! [`classify_manifest`] applies the table to one captured Snapshot manifest. It is a pure
//! function of the manifest and the table — no working tree, no sandbox, no host path — so the
//! same Snapshot answers the same on every machine. It reports; it never removes a path from a
//! Snapshot or from a delivered worktree.

use review_source_git::Manifest;
use serde::{Deserialize, Serialize};

/// The repository-relative authority directory this layout governs, without a separator.
pub const AF_DIR: &str = ".af";

/// Whether an entry is one file or a directory of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
}

impl EntryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
}

/// Who writes an entry — the writer of record, the one that may replace it. `af onboard`
/// scaffolds the project file, the pipelines and the Worker packages when the authority
/// directory is absent and never overwrites them again, which is why those are the project's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    Human,
    Onboard,
    CatalogInit,
    CatalogSync,
    SelfOptimize,
    Kernel,
}

impl Writer {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Onboard => "af onboard",
            Self::CatalogInit => "af catalog init",
            Self::CatalogSync => "af catalog sync",
            Self::SelfOptimize => "af self optimize",
            Self::Kernel => "the kernel",
        }
    }
}

/// Whether git versions an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Versioning {
    /// Committed, reviewed, and part of every Snapshot this repository captures.
    Versioned,
    /// Deliberately not committed: one checkout's personal overrides.
    Ignored,
    /// Never on disk at all — the kernel builds it inside an in-memory authority Manifest.
    Synthetic,
}

impl Versioning {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Versioned => "versioned",
            Self::Ignored => "gitignored",
            Self::Synthetic => "synthetic",
        }
    }
}

/// One declared entry directly under the authority directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The exact path segment, without a trailing separator.
    pub segment: &'static str,
    pub kind: EntryKind,
    pub writer: Writer,
    pub versioning: Versioning,
    /// What the entry holds, in one short phrase.
    pub purpose: &'static str,
}

impl Entry {
    /// How the entry reads in a table or in prose: a directory carries a trailing separator.
    pub fn name(&self) -> String {
        match self.kind {
            EntryKind::File => self.segment.to_string(),
            EntryKind::Directory => format!("{}/", self.segment),
        }
    }
}

const fn file(
    segment: &'static str,
    writer: Writer,
    versioning: Versioning,
    purpose: &'static str,
) -> Entry {
    Entry {
        segment,
        kind: EntryKind::File,
        writer,
        versioning,
        purpose,
    }
}

const fn directory(
    segment: &'static str,
    writer: Writer,
    versioning: Versioning,
    purpose: &'static str,
) -> Entry {
    Entry {
        segment,
        kind: EntryKind::Directory,
        writer,
        versioning,
        purpose,
    }
}

/// Every canonical entry: files first, then directories. Adding an authority path to the kernel
/// means adding it here; the drift test over the kernel's own sources insists on it.
pub const LAYOUT: &[Entry] = &[
    file(
        "af.toml",
        Writer::Human,
        Versioning::Versioned,
        "the project configuration layer",
    ),
    file(
        "af.local.toml",
        Writer::Human,
        Versioning::Ignored,
        "this checkout's personal overrides",
    ),
    file(
        "af.lock",
        Writer::Onboard,
        Versioning::Versioned,
        "pinned af release, pipelines, Workers",
    ),
    file(
        "README.md",
        Writer::Onboard,
        Versioning::Versioned,
        "how to run a review in this repository",
    ),
    file(
        "code-policy.toml",
        Writer::Human,
        Versioning::Versioned,
        "code Task acceptance policy",
    ),
    file(
        "document-policy.toml",
        Writer::Human,
        Versioning::Versioned,
        "document Task acceptance policy",
    ),
    file(
        "task-catalog.toml",
        Writer::Human,
        Versioning::Versioned,
        "Task packages, kinds, selection, providers",
    ),
    file(
        "optimization-policy.json",
        Writer::Human,
        Versioning::Versioned,
        "self-optimization policy",
    ),
    file(
        "optimization-sources.toml",
        Writer::Human,
        Versioning::Versioned,
        "self-optimization capture sources",
    ),
    directory(
        "pipelines",
        Writer::Human,
        Versioning::Versioned,
        "review pipeline definitions",
    ),
    directory(
        "workers",
        Writer::Human,
        Versioning::Versioned,
        "reviewer Worker packages",
    ),
    directory(
        "task-packages",
        Writer::Human,
        Versioning::Versioned,
        "Task Worker, kind and Pipeline packages",
    ),
    directory(
        "packages",
        Writer::CatalogInit,
        Versioning::Versioned,
        "Task packages a starter wrote",
    ),
    directory(
        "checks",
        Writer::Human,
        Versioning::Versioned,
        "project check scripts a Gate runs",
    ),
    directory(
        "vendor",
        Writer::CatalogSync,
        Versioning::Versioned,
        "shared catalogs an explicit sync imported",
    ),
    directory(
        "artifact-reuse",
        Writer::SelfOptimize,
        Versioning::Versioned,
        "accepted artifact-reuse receipts",
    ),
    directory(
        "cache",
        Writer::SelfOptimize,
        Versioning::Versioned,
        "accepted sandbox cache selections",
    ),
    directory(
        "task-compat",
        Writer::Kernel,
        Versioning::Synthetic,
        "legacy Task authority, built in memory",
    ),
];

/// What one repository-relative path is, as far as this layout is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Not under the authority directory at all; this layout says nothing about it.
    Outside,
    /// The authority directory itself.
    Root,
    /// Named by exactly one declared entry.
    Declared(&'static Entry),
    /// Under the authority directory, named by nothing in [`LAYOUT`].
    Undeclared,
}

impl Classification {
    pub fn entry(self) -> Option<&'static Entry> {
        match self {
            Self::Declared(entry) => Some(entry),
            _ => None,
        }
    }
}

/// Classify one repository-relative, slash-separated path — a Snapshot manifest path, never a
/// host path. A declared directory covers everything beneath it; a declared file covers exactly
/// itself, so nothing may hang below one. A synthetic entry exists only inside an in-memory
/// authority Manifest, so on a repository path it declares nothing: a tracked copy of it is as
/// undeclared as any other stray tree.
pub fn classify(path: &str) -> Classification {
    let path = path.strip_prefix("./").unwrap_or(path);
    let path = path.trim_end_matches('/');
    if path == AF_DIR {
        return Classification::Root;
    }
    let Some(rest) = path.strip_prefix(".af/") else {
        return Classification::Outside;
    };
    let (segment, beneath) = match rest.split_once('/') {
        Some((head, tail)) => (head, Some(tail)),
        None => (rest, None),
    };
    if segment.is_empty() {
        return Classification::Root;
    }
    let Some(entry) = LAYOUT.iter().find(|entry| entry.segment == segment) else {
        return Classification::Undeclared;
    };
    if entry.versioning == Versioning::Synthetic {
        return Classification::Undeclared;
    }
    match entry.kind {
        EntryKind::Directory => Classification::Declared(entry),
        EntryKind::File if beneath.is_none() => Classification::Declared(entry),
        EntryKind::File => Classification::Undeclared,
    }
}

/// Whether the layout declares this path. The authority directory itself counts as declared.
pub fn is_declared(path: &str) -> bool {
    let found = classify(path);
    found.entry().is_some() || found == Classification::Root
}

/// Whether the path sits under the authority directory and nothing declares it.
pub fn is_undeclared(path: &str) -> bool {
    classify(path) == Classification::Undeclared
}

/// Classify one *manifest* path spelling.
///
/// A manifest path is a lossless rendering of raw bytes, so the decision is taken on the decoded
/// bytes rather than on the spelling. Only the first segment under `.af/` has to be text, because
/// every declared segment is ASCII: a declared directory covers whatever bytes hang beneath it,
/// a declared file covers exactly itself, and a first segment that is not UTF-8 names nothing and
/// is undeclared, which is the fail-closed answer.
pub fn classify_manifest_path(encoded: &str) -> Classification {
    let raw = review_core::decode_path(encoded);
    let raw = raw.strip_prefix(b"./").unwrap_or(&raw);
    let Some(rest) = raw.strip_prefix(b".af/") else {
        return match std::str::from_utf8(raw) {
            Ok(path) => classify(path),
            Err(_) => Classification::Outside,
        };
    };
    let (segment, beneath) = match rest.iter().position(|byte| *byte == b'/') {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    let Ok(segment) = std::str::from_utf8(segment) else {
        return Classification::Undeclared;
    };
    match classify(&format!(".af/{segment}")) {
        Classification::Declared(entry) if entry.kind == EntryKind::Directory => {
            Classification::Declared(entry)
        }
        Classification::Declared(entry) if beneath.is_none() => Classification::Declared(entry),
        Classification::Declared(_) => Classification::Undeclared,
        other => other,
    }
}

/// One group of a Snapshot's authority paths: what they are called and what they weigh.
///
/// The paths keep their manifest spelling, the same one `ignored_paths` records, so a receipt and
/// a manifest name a path identically. Advisory data only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathGroup {
    /// Manifest path spellings, in the manifest's own order — which is sorted, so this is too.
    pub paths: Vec<String>,
    /// The sum of the entries' recorded sizes. Saturating: a classification is a total function
    /// of the manifest and must not panic on a hostile one.
    pub bytes: u64,
}

impl PathGroup {
    pub fn count(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// What the layout says about every authority path of one captured Snapshot manifest. Paths
/// outside the authority directory are in neither group: this table says nothing about them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestClassification {
    pub declared: PathGroup,
    pub undeclared: PathGroup,
}

/// Classify every authority path of one captured Snapshot manifest.
///
/// Reads the recorded entries and nothing else, so the result is reproducible from the Snapshot
/// alone. Nothing here changes the manifest, the Snapshot's identity, or what a delivery writes.
pub fn classify_manifest(manifest: &Manifest) -> ManifestClassification {
    let mut found = ManifestClassification::default();
    for entry in &manifest.entries {
        let group = match classify_manifest_path(&entry.path) {
            Classification::Declared(_) => &mut found.declared,
            Classification::Undeclared => &mut found.undeclared,
            Classification::Outside | Classification::Root => continue,
        };
        group.paths.push(entry.path.clone());
        group.bytes = group.bytes.saturating_add(entry.size);
    }
    found
}

/// What delivery does when the source Snapshot carries authority paths this table does not name.
///
/// `warn` is the default: delivery records them and says so. `refuse` stops delivery before it
/// prepares a record or touches Git. Whichever a project chose is captured with the rest of its
/// project policy at plan time, so editing `.af/af.toml` later cannot change what an admitted
/// plan agreed to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndeclaredAfPathsPolicy {
    #[default]
    Warn,
    Refuse,
}

impl UndeclaredAfPathsPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Refuse => "refuse",
        }
    }

    /// Whether this is what a project that declares nothing gets. A captured policy that is the
    /// default is not written down, so a project that never heard of this knob keeps the exact
    /// policy identity it had.
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Warn)
    }
}

const HEADERS: [&str; 5] = ["entry", "kind", "writer", "git", "holds"];

fn cells(entry: &Entry) -> [String; 5] {
    [
        entry.name(),
        entry.kind.as_str().to_string(),
        entry.writer.as_str().to_string(),
        entry.versioning.as_str().to_string(),
        entry.purpose.to_string(),
    ]
}

fn widths() -> [usize; 5] {
    let mut widths = HEADERS.map(str::len);
    for entry in LAYOUT {
        for (width, cell) in widths.iter_mut().zip(cells(entry)) {
            *width = (*width).max(cell.chars().count());
        }
    }
    widths
}

fn plain_row(row: &[String; 5], widths: &[usize; 5]) -> String {
    let mut text = String::from("  ");
    for (index, cell) in row.iter().enumerate() {
        text.push_str(cell);
        if index + 1 < row.len() {
            let pad = widths[index] - cell.chars().count() + 2;
            text.push_str(&" ".repeat(pad));
        }
    }
    text.push('\n');
    text
}

/// The layout table as aligned plain text, indented two spaces for `af help config`.
pub fn render_table() -> String {
    let widths = widths();
    let header = HEADERS.map(str::to_string);
    let mut out = plain_row(&header, &widths);
    for entry in LAYOUT {
        out.push_str(&plain_row(&cells(entry), &widths));
    }
    out
}

/// The same entries as a Markdown table, for documentation that quotes the layout.
pub fn render_markdown_table() -> String {
    let mut out = format!("| {} |\n|---|---|---|---|---|\n", HEADERS.join(" | "));
    for entry in LAYOUT {
        let [name, kind, writer, git, holds] = cells(entry);
        let row = format!("| `{name}` | {kind} | {writer} | {git} | {holds} |\n");
        out.push_str(&row);
    }
    out
}

/// What never belongs under the authority directory, and where it lives instead. One paragraph,
/// rendered verbatim by `af help config` and by any documentation that quotes it, so they cannot drift.
pub const ELSEWHERE: &str = "\
Nothing the kernel records belongs here. Task files, run state, candidate patches, transcripts,
reviewer results, receipts, logs and measurements go to the Store under $XDG_STATE_HOME/af: af
refuses a state directory inside the checkout, and a Task file is captured into the Store at plan
time, so the copy on disk is disposable. Keep Task files outside every checkout — for example
under $XDG_STATE_HOME/af/tasks/ — because a Task file git would track rides along in every later
Snapshot this repository captures, and af warns when it finds one.";

#[cfg(test)]
mod tests {
    use review_source_git::{Entry as ManifestEntry, EntryKind as FileKind};

    use super::*;

    fn segment_of(path: &str) -> Option<&'static str> {
        classify(path).entry().map(|entry| entry.segment)
    }

    /// A manifest of regular files. Entries carry the recorded size; the content digest is
    /// irrelevant to a classification, which is exactly the point.
    fn manifest(entries: &[(&str, u64)]) -> Manifest {
        Manifest {
            entries: entries
                .iter()
                .map(|(path, size)| ManifestEntry {
                    path: (*path).to_string(),
                    kind: FileKind::File,
                    content: format!("sha256:{}", "0".repeat(64)),
                    size: *size,
                })
                .collect(),
        }
    }

    #[test]
    fn every_entry_is_declared_once_and_only_the_local_layer_is_unversioned() {
        let declared = LAYOUT.len();
        let mut segments: Vec<&str> = LAYOUT.iter().map(|entry| entry.segment).collect();
        segments.sort_unstable();
        segments.dedup();
        assert_eq!(segments.len(), declared, "a segment is declared twice");
        for entry in LAYOUT {
            assert!(!entry.segment.is_empty(), "an entry has no segment");
            assert!(!entry.segment.contains('/'), "an entry spans segments");
            assert!(!entry.purpose.is_empty(), "an entry says nothing");
        }
        let unusual: Vec<String> = LAYOUT
            .iter()
            .filter(|entry| entry.versioning != Versioning::Versioned)
            .map(Entry::name)
            .collect();
        assert_eq!(unusual, vec!["af.local.toml", "task-compat/"]);
    }

    #[test]
    fn a_synthetic_entry_declares_nothing_on_a_repository_path() {
        let synthetic: Vec<&Entry> = LAYOUT
            .iter()
            .filter(|entry| entry.versioning == Versioning::Synthetic)
            .collect();
        assert!(!synthetic.is_empty());
        for entry in synthetic {
            let root = format!(".af/{}", entry.segment);
            assert_eq!(classify(&root), Classification::Undeclared, "{root}");
            assert_eq!(
                classify(&format!("{root}/x")),
                Classification::Undeclared,
                "{root}/x"
            );
            assert!(is_undeclared(&root));
        }
        assert!(is_declared(".af/pipelines/review.toml"));
    }

    #[test]
    fn classification_covers_directories_but_never_hangs_below_a_declared_file() {
        assert_eq!(classify(".af"), Classification::Root);
        assert_eq!(classify(".af/"), Classification::Root);
        assert_eq!(classify("src/lib.rs"), Classification::Outside);
        assert_eq!(classify("docs/.af/af.toml"), Classification::Outside);
        assert_eq!(segment_of(".af/af.toml"), Some("af.toml"));
        assert_eq!(segment_of(".af/pipelines/x.toml"), Some("pipelines"));
        assert_eq!(segment_of(".af/tasks/x.json"), None);
        assert_eq!(classify(".af/af.toml/x"), Classification::Undeclared);
        assert!(is_undeclared(".af/tasks"));
        assert!(!is_undeclared("tasks/x.json"));
        assert!(is_declared(".af/workers/bugs/reviewer.toml"));
        assert!(is_declared(".af"));
    }

    #[test]
    fn both_renderings_carry_exactly_the_declared_entries() {
        let plain = render_table();
        let markdown = render_markdown_table();
        assert_eq!(plain.lines().count(), LAYOUT.len() + 1);
        assert_eq!(markdown.lines().count(), LAYOUT.len() + 2);
        for entry in LAYOUT {
            let name = entry.name();
            let row = format!("| `{name}` |");
            assert!(plain.contains(&name), "{name}");
            assert!(markdown.contains(&row), "{name}");
            assert!(plain.contains(entry.purpose), "{name}");
            assert!(markdown.contains(entry.purpose), "{name}");
        }
        // Columns, not a ragged join: two spaces separate them, every row carries five.
        for line in plain.lines() {
            let columns = line.split("  ").filter(|cell| !cell.is_empty());
            assert_eq!(columns.count(), HEADERS.len(), "{line}");
        }
    }

    #[test]
    fn a_manifest_is_grouped_by_the_table_with_counts_and_byte_totals() {
        let captured = manifest(&[
            (".af/af.toml", 10),
            (".af/pipelines/review.toml", 20),
            (".af/tasks/x/candidate.patch", 300),
            (".af/tasks/x/reviews.json", 40),
            ("src/lib.rs", 100_000),
        ]);
        let found = classify_manifest(&captured);
        let declared = [".af/af.toml", ".af/pipelines/review.toml"];
        let undeclared = [".af/tasks/x/candidate.patch", ".af/tasks/x/reviews.json"];
        assert_eq!(found.declared.paths, declared);
        assert_eq!(found.declared.bytes, 30);
        assert_eq!(found.declared.count(), 2);
        assert_eq!(found.undeclared.paths, undeclared);
        assert_eq!(found.undeclared.bytes, 340);
        assert_eq!(found.undeclared.count(), 2);
        // Twice over the same Snapshot is the same answer, and a tree the table fully
        // declares reports nothing at all.
        assert_eq!(classify_manifest(&captured), found);
        let only_declared = manifest(&[(".af/af.lock", 1), ("README.md", 2)]);
        assert!(classify_manifest(&only_declared).undeclared.is_empty());
        let nothing = classify_manifest(&Manifest::default());
        assert_eq!(nothing, ManifestClassification::default());
    }

    #[test]
    fn a_manifest_path_is_judged_by_its_decoded_bytes() {
        // `50%25-off` is one path named `50%-off`; the group records the spelling.
        let patch = classify_manifest_path(".af/tasks/50%25-off.patch");
        assert_eq!(patch, Classification::Undeclared);
        let worker = classify_manifest_path(".af/workers/50%25-off/r.toml");
        assert_eq!(worker.entry().map(|entry| entry.segment), Some("workers"));
        // A non-UTF-8 name declares nothing: every declared segment is ASCII.
        let raw = classify_manifest_path(".af/%FF");
        assert_eq!(raw, Classification::Undeclared);
        let elsewhere = classify_manifest_path("src/%FF.rs");
        assert_eq!(elsewhere, Classification::Outside);
        let found = classify_manifest(&manifest(&[(".af/%FF", 7), (".af/af.toml", 1)]));
        assert_eq!(found.undeclared.paths, [".af/%FF"]);
        // Only the first segment must be text: a declared directory covers any child bytes,
        // a declared file covers nothing beneath it, and a synthetic entry declares nothing.
        assert!(matches!(
            classify_manifest_path(".af/checks/%FF.sh"),
            Classification::Declared(entry) if entry.segment == "checks"
        ));
        assert_eq!(
            classify_manifest_path(".af/af.toml/%FF"),
            Classification::Undeclared
        );
        assert_eq!(
            classify_manifest_path(".af/task-compat/%FF"),
            Classification::Undeclared
        );
        let found = classify_manifest(&manifest(&[(".af/checks/%FF.sh", 3), (".af/%FF", 7)]));
        assert_eq!(found.undeclared.paths, [".af/%FF"]);
        assert_eq!(found.declared.paths, [".af/checks/%FF.sh"]);
        assert_eq!(found.undeclared.bytes, 7);
    }

    #[test]
    fn a_saturating_byte_total_never_panics_on_a_hostile_manifest() {
        let huge = manifest(&[(".af/tasks/a", u64::MAX), (".af/tasks/b", u64::MAX)]);
        let found = classify_manifest(&huge);
        assert_eq!(found.undeclared.count(), 2);
        assert_eq!(found.undeclared.bytes, u64::MAX);
    }

    #[test]
    fn the_delivery_policy_defaults_to_warn_and_spells_both_values() {
        use UndeclaredAfPathsPolicy::{Refuse, Warn};

        assert_eq!(UndeclaredAfPathsPolicy::default(), Warn);
        assert!(Warn.is_default(), "a silent project keeps warn");
        assert!(!Refuse.is_default());
        for policy in [Warn, Refuse] {
            let value = serde_json::to_value(policy).unwrap();
            assert_eq!(value, serde_json::json!(policy.as_str()));
            let parsed: UndeclaredAfPathsPolicy = serde_json::from_value(value).unwrap();
            assert_eq!(parsed, policy);
        }
        let unknown = serde_json::json!("strip");
        let refused = serde_json::from_value::<UndeclaredAfPathsPolicy>(unknown);
        assert!(refused.is_err(), "an unknown policy is not a setting");
    }

    #[test]
    fn a_group_round_trips_and_refuses_an_unknown_field() {
        let group = PathGroup {
            paths: vec![".af/x".into()],
            bytes: 12,
        };
        let value = serde_json::to_value(&group).unwrap();
        assert_eq!(value, serde_json::json!({"paths":[".af/x"],"bytes":12}));
        let parsed: PathGroup = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, group);
        let widened = serde_json::json!({"paths":[],"bytes":0,"removed":true});
        assert!(serde_json::from_value::<PathGroup>(widened).is_err());
    }

    #[test]
    fn the_paragraph_names_the_store_and_the_records_that_belong_there() {
        let expected = [
            "Task files",
            "run state",
            "candidate patches",
            "transcripts",
            "reviewer results",
            "receipts",
            "logs",
            "measurements",
            "$XDG_STATE_HOME/af",
        ];
        for phrase in expected {
            assert!(ELSEWHERE.contains(phrase), "{phrase} is missing");
        }
    }
}
