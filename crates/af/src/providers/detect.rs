//! `af provider detect`: the official CLIs and the auth directories on this machine, what each
//! directory's CLI says about its login, and the one command that registers it (ADR-0206).
//!
//! The scan reads names and metadata only. Binaries are looked up on the sanitized `PATH`, then
//! in a fixed list of install directories; no shell, `npm prefix`, `nvm` or rc file is evaluated.
//! Candidate directories come from the CLIs' selector variables, their default directories, a
//! literal one-level list of sibling names, the registry and `--dir`. Whether a directory holds a
//! login marker is decided by `fstatat` on fixed names below a descriptor opened with
//! `O_NOFOLLOW`, so no file under a candidate is ever opened, and a directory without a marker is
//! never handed to its CLI unless `--all` asks: `codex login status` creates its home on first
//! contact. A directory with a marker is probed exactly as `af provider status` probes a
//! registered context. `--apply` registers only what that probe reported authenticated, through
//! `add`'s locked, atomic publication; it never logs in and never creates a directory.

use std::io::IsTerminal as _;

use super::installation::{BoundedRun, VersionCheck, run_bounded, version_check};
use super::*;

/// The marker names that make a directory a Claude auth directory.
const CLAUDE_MARKERS: &[&str] = &[".claude.json", ".credentials.json", "settings.json"];
/// The marker names that make a directory a Codex auth directory.
const CODEX_MARKERS: &[&str] = &["auth.json", "config.toml"];

/// Harness CLIs af does not drive yet: listed by name and version when found on `PATH`, never
/// probed, and none of their directories is scanned.
const UNSUPPORTED: &[&str] = &[
    "gemini",
    "cursor-agent",
    "opencode",
    "amp",
    "copilot",
    "aider",
    "qwen",
    "goose",
];

/// The two system install directories of the fixed list. A debug build's test fixture may
/// replace them with `AF_TEST_DETECT_SYSTEM_DIRS` (a `:`-separated list, empty for none), so that
/// no test can find the developer's own `/opt/homebrew/bin/claude`; a release binary never reads
/// the setting.
const SYSTEM_BIN_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// The last line of every human report: what the scan cannot see.
const ALIAS_NOTE: &str = "shell aliases or functions that set CLAUDE_CONFIG_DIR/CODEX_HOME are not \
                          read; pass --dir for such a directory";

/// Wrap prose here; commands are never wrapped, so they stay copyable.
const WIDTH: usize = 99;

/// What the command line asked for.
pub struct DetectOptions {
    pub dirs: Vec<PathBuf>,
    pub ids: Vec<String>,
    pub kind: Option<String>,
    pub all: bool,
    pub apply: bool,
    pub json: bool,
    pub identity: bool,
}

/// Where a candidate directory came from, in the order the table lists them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Source {
    Env,
    Default,
    Sibling,
    Registry,
    Explicit,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Default => "default",
            Self::Sibling => "sibling",
            Self::Registry => "registry",
            Self::Explicit => "explicit",
        }
    }

    /// Which source a directory found twice keeps: what the operator typed, then what the
    /// shell selects, then the CLI's own default, then a sibling name, then the registry.
    fn precedence(self) -> u8 {
        match self {
            Self::Explicit => 0,
            Self::Env => 1,
            Self::Default => 2,
            Self::Sibling => 3,
            Self::Registry => 4,
        }
    }
}

/// One directory the scan found, before any probe.
struct Candidate {
    kind: ProviderKind,
    /// The path as the scan or the operator named it.
    named: PathBuf,
    /// Its canonical path, when it exists.
    canonical: Option<PathBuf>,
    source: Source,
    /// `--id` for an explicit directory.
    explicit_id: Option<String>,
    markers: Vec<&'static str>,
    /// The owner, mode and symlink rules `setup` applies, as the reason it fails them.
    safety: Result<(), String>,
    registered_as: Option<String>,
    /// Why the directory could not be listed, when it could not: its markers were then checked
    /// by name, one path each, and it is probed all the same.
    listing: Option<String>,
}

impl Candidate {
    fn path(&self) -> &Path {
        self.canonical.as_deref().unwrap_or(&self.named)
    }
}

/// One official CLI the scan found.
struct Binary {
    kind: ProviderKind,
    /// Where it was found: on `PATH`, its canonical path; off `PATH`, the install directory's
    /// entry, so the PATH fix names that directory.
    path: PathBuf,
    on_path: bool,
    version: Option<String>,
    /// The capability check's verdict: `None` when af could not tell.
    compatible: Option<bool>,
    /// The refusal's report, for stderr and the rows of this kind.
    refusal: Option<(String, String)>,
}

/// An unsupported harness CLI found on `PATH`.
struct Unsupported {
    name: &'static str,
    path: PathBuf,
    version: Option<String>,
}

/// The closed vocabulary of a row's login, as `af provider status` reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Auth {
    Authenticated,
    NotAuthenticated,
    Unknown,
    Unavailable,
    InstallationFailed,
    Incompatible,
    ProbeDenied,
    NotProbed,
}

impl Auth {
    fn name(self) -> &'static str {
        match self {
            Self::Authenticated => "authenticated",
            Self::NotAuthenticated => "not_authenticated",
            Self::Unknown => "unknown",
            Self::Unavailable => "unavailable",
            Self::InstallationFailed => "installation_failed",
            Self::Incompatible => "incompatible",
            Self::ProbeDenied => "probe_denied",
            Self::NotProbed => "not_probed",
        }
    }

    fn of(status: &str) -> Self {
        match status {
            "authenticated" => Self::Authenticated,
            "not authenticated" => Self::NotAuthenticated,
            "unavailable" => Self::Unavailable,
            INSTALLATION_FAILED => Self::InstallationFailed,
            INCOMPATIBLE => Self::Incompatible,
            PROBE_DENIED => Self::ProbeDenied,
            UNMARKED => Self::NotProbed,
            _ => Self::Unknown,
        }
    }
}

/// What a row suggests, in af's own words.
struct Suggested {
    action: &'static str,
    id: Option<String>,
    /// The command, absolute paths, as JSON carries it.
    command: Option<String>,
    /// The same command with `~/` paths, as the table prints it.
    display: Option<String>,
}

impl Suggested {
    fn none() -> Self {
        Self {
            action: "none",
            id: None,
            command: None,
            display: None,
        }
    }
}

/// What `--apply` did with a row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Applied {
    Registered,
    AlreadyRegistered,
    RegistryConflict,
    SkippedNotAuthenticated,
    /// Its CLI is off `PATH`, where `af provider status` and Task admission do not look: a
    /// registration would name a Provider af cannot run.
    SkippedCliOffPath,
}

impl Applied {
    fn name(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::AlreadyRegistered => "already_registered",
            Self::RegistryConflict => "registry_conflict",
            Self::SkippedNotAuthenticated => "skipped_not_authenticated",
            Self::SkippedCliOffPath => "skipped_cli_off_path",
        }
    }
}

/// One candidate with what its probe said.
struct Row {
    candidate: Candidate,
    auth: Auth,
    /// The plan or method label: never an email or an organization.
    login: String,
    auth_method: &'static str,
    plan: &'static str,
    /// The harness-reported account label, kept only for `--identity` at a terminal.
    account: Option<String>,
    /// Why the row says what it says, for the human report only: it may quote the CLI.
    note: Option<String>,
    /// Why the login is not known, in af's closed vocabulary (`timeout`, `unstartable`, …), and
    /// af's own words for it: what the document carries in place of `note`.
    cause: Option<&'static str>,
    detail: Option<String>,
    suggested: Suggested,
    applied: Option<Apply>,
}

/// What `--apply` did with a row, and the registered ID a conflict met.
struct Apply {
    id: String,
    result: Applied,
    conflict_with: Option<String>,
}

impl Apply {
    fn new(id: String, result: Applied) -> Self {
        Self {
            id,
            result,
            conflict_with: None,
        }
    }
}

/// A usage error af reports itself: exit 2, and under `--json` the error document.
fn usage(json: bool, message: &str) -> Result<i32, String> {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "schema": "af/error@2",
                "command": "af provider detect",
                "error": message,
                "exit_code": 2,
            })
        );
    }
    eprintln!("af provider detect: {message}\n  try: af provider detect --help");
    Ok(2)
}

/// `af provider detect`. Returns the documented exit code: 0 the scan completed, 4 a found CLI
/// cannot start or is incompatible, 5 a registry conflict stopped `--apply`.
pub fn run(options: DetectOptions) -> Result<i32, String> {
    let only = match options.kind.as_deref() {
        Some(kind) => Some(ProviderKind::parse(kind)?),
        None => None,
    };
    let kinds: Vec<ProviderKind> = [ProviderKind::Claude, ProviderKind::Codex]
        .into_iter()
        .filter(|kind| only.is_none_or(|only| only == *kind))
        .collect();
    if !options.ids.is_empty() && options.ids.len() != options.dirs.len() {
        return usage(
            options.json,
            &format!(
                "{} --id for {} --dir: pass one --id per --dir, in the same order",
                options.ids.len(),
                options.dirs.len()
            ),
        );
    }
    for id in &options.ids {
        if let Err(error) = validate_explicit_id(id) {
            return usage(options.json, &format!("--id {id}: {error}"));
        }
    }
    let home = home_dir();
    let mut notes = Vec::new();
    // A registry that cannot be read (malformed, unreadable, mid-transaction) is unknown, never
    // empty: no row says whether it is registered, `--apply` refuses, and every output says why.
    let mut registry_unknown = None;
    let (registry, registry_specs) = match registry_path() {
        Ok(Some(path)) => match ensure_no_registry_transaction(&path).and_then(|()| {
            read_registry(&path).and_then(|text| {
                text.map(|text| parse_registry(&text, &path))
                    .transpose()
                    .map(Option::unwrap_or_default)
            })
        }) {
            Ok(specs) => (Some(path), specs),
            Err(error) => {
                notes.push(format!(
                    "the provider registry could not be read, so whether each directory is \
                     registered is unknown and --apply registers nothing: {error}; af provider \
                     status names the problem"
                ));
                registry_unknown = Some(error);
                (Some(path), Vec::new())
            }
        },
        Ok(None) => (None, Vec::new()),
        Err(error) => {
            notes.push(format!("no provider registry path is available: {error}"));
            registry_unknown = Some(error);
            (None, Vec::new())
        }
    };
    // `--apply` chooses IDs and registers against the registry: one that cannot be read refuses
    // it before anything is scanned (exit 1; `af/error@2` under `--json`).
    if options.apply
        && let Some(error) = &registry_unknown
    {
        return Err(format!(
            "--apply registered nothing: the provider registry cannot be read ({error}); fix or \
             move the file (af provider status names the problem), then run af provider detect \
             --apply again"
        ));
    }

    // Candidates: explicit first, so a directory typed and also found keeps `explicit`.
    let mut candidates = Vec::new();
    let mut explicit_kinds: BTreeMap<ProviderKind, usize> = BTreeMap::new();
    for (index, dir) in options.dirs.iter().enumerate() {
        let named = match std::path::absolute(dir) {
            Ok(path) => path,
            Err(error) => {
                return usage(
                    options.json,
                    &format!("--dir {}: cannot be made absolute: {error}", dir.display()),
                );
            }
        };
        let canonical = fs::canonicalize(&named).ok();
        let kind = match only {
            Some(kind) => kind,
            None if canonical.is_none() => {
                return usage(
                    options.json,
                    &format!("--dir {} does not exist; check the path", dir.display()),
                );
            }
            None => {
                let probe = canonical.as_deref().unwrap_or(&named);
                let mut claude = markers_in(probe, CLAUDE_MARKERS).unwrap_or_default();
                if claude.is_empty()
                    && default_claude_context(ProviderKind::Claude, probe, home.as_deref())
                {
                    claude.push(DEFAULT_CLAUDE_MARKER);
                }
                let codex = markers_in(probe, CODEX_MARKERS).unwrap_or_default();
                match (claude.is_empty(), codex.is_empty()) {
                    (false, true) => ProviderKind::Claude,
                    (true, false) => ProviderKind::Codex,
                    (both_empty, _) => {
                        let what = if both_empty {
                            "holds no login marker of either kind"
                        } else {
                            "holds login markers of both kinds"
                        };
                        return usage(
                            options.json,
                            &format!(
                                "--dir {} {what}; pass --kind claude or --kind codex",
                                dir.display()
                            ),
                        );
                    }
                }
            }
        };
        *explicit_kinds.entry(kind).or_default() += 1;
        candidates.push(candidate(
            kind,
            named,
            Source::Explicit,
            options.ids.get(index).cloned(),
        ));
    }
    if options.ids.is_empty()
        && let Some((kind, _)) = explicit_kinds.iter().find(|(_, count)| **count > 1)
    {
        return usage(
            options.json,
            &format!(
                "more than one --dir names a {} directory; pass one --id per --dir, in the same \
                 order (--dir A --id {0}-a --dir B --id {0}-b)",
                kind.name()
            ),
        );
    }
    for kind in &kinds {
        for (path, source) in scanned_dirs(*kind, home.as_deref(), &mut notes) {
            candidates.push(candidate(*kind, path, source, None));
        }
        for spec in registry_specs.iter().filter(|spec| spec.kind == *kind) {
            if let Some(auth_dir) = &spec.auth_dir {
                candidates.push(candidate(*kind, auth_dir.clone(), Source::Registry, None));
            }
        }
    }
    let mut candidates = deduplicate(candidates);
    candidates.retain(|candidate| kinds.contains(&candidate.kind));
    for candidate in &mut candidates {
        candidate.registered_as = registry_specs
            .iter()
            .find(|spec| {
                spec.kind == candidate.kind
                    && spec.auth_dir.as_deref() == candidate.canonical.as_deref()
            })
            .map(|spec| spec.id.clone());
    }
    candidates.sort_by(|left, right| {
        (left.kind, left.source, left.path()).cmp(&(right.kind, right.source, right.path()))
    });

    let binaries: Vec<Binary> = kinds
        .iter()
        .filter_map(|kind| find_binary(*kind, home.as_deref()))
        .collect();
    let unsupported = find_unsupported();
    if binaries.is_empty() && candidates.is_empty() {
        let code = EXIT_OK;
        if options.json {
            let mut all = notes.clone();
            all.extend(guidance_notes(&kinds));
            let shape = Shape {
                code,
                applied: options.apply,
                registry_unknown: registry_unknown.is_some(),
                missing: kinds.clone(),
                next_action: Some(rerun_command(&options)),
            };
            println!("{}", document(shape, &binaries, &unsupported, &[], &all));
        } else {
            for line in install_guidance_for(&kinds, home.as_deref()) {
                println!("{line}");
            }
            print_unsupported(&unsupported, home.as_deref());
            for note in &notes {
                for line in wrap(&format!("note: {note}"), "") {
                    println!("{line}");
                }
            }
        }
        return Ok(code);
    }

    let cancelled = AtomicBool::new(false);
    let (binaries, rows) = probe_all(binaries, candidates, options.all, &cancelled);
    let mut rows = rows;
    for binary in &binaries {
        if let Some((_, report)) = &binary.refusal {
            eprintln!("af provider detect: {report}");
        }
    }
    let identity = options.identity && !options.json && std::io::stdout().is_terminal();
    if options.identity && !identity {
        eprintln!(
            "af provider detect: --identity shows account labels only when stdout is a terminal \
             and --json is absent; none is printed here"
        );
    }
    let registry_ids: BTreeSet<String> =
        registry_specs.iter().map(|spec| spec.id.clone()).collect();
    suggest(
        &mut rows,
        &binaries,
        &registry_ids,
        registry_unknown.is_some(),
        home.as_deref(),
    );

    // What the scan itself could not use is said under the table too; the rest of the notes
    // repeat lines the human report already prints.
    let scan_notes = notes.clone();
    for kind in &kinds {
        if !binaries.iter().any(|binary| binary.kind == *kind) {
            notes.push(format!(
                "no {} CLI was found on PATH or in the usual install directories; install it: {}",
                kind.name(),
                install_commands(*kind)[0]
            ));
        }
    }
    for binary in binaries.iter().filter(|binary| !binary.on_path) {
        notes.push(format!(
            "the {} CLI was found off PATH at {}; add its directory to PATH: {}",
            binary.kind.name(),
            binary.path.display(),
            path_fix(&binary.path, home.as_deref())
        ));
    }
    notes.push(ALIAS_NOTE.to_string());

    let denied = binaries.iter().any(|binary| {
        binary
            .refusal
            .as_ref()
            .is_some_and(|(status, _)| status == PROBE_DENIED)
    });
    let incompatible = binaries
        .iter()
        .any(|binary| binary.compatible == Some(false));
    let mut code = if denied {
        EXIT_PROBE_DENIED
    } else if incompatible {
        EXIT_PROVIDER_CLI_MISSING
    } else {
        EXIT_OK
    };

    if !options.json {
        print_table(
            &rows,
            &kinds,
            identity,
            registry_unknown.is_some(),
            home.as_deref(),
        );
        if !scan_notes.is_empty() {
            println!();
            for note in &scan_notes {
                for line in wrap(&format!("note: {note}"), "") {
                    println!("{line}");
                }
            }
        }
        print_binaries(&binaries, &kinds, home.as_deref());
        print_unsupported(&unsupported, home.as_deref());
    }
    if options.apply {
        let path = registry.ok_or("no provider registry path is available")?;
        let end = apply(
            &mut rows,
            &path,
            options.json,
            home.as_deref(),
            &registry_ids,
        )?;
        if end.conflict {
            code = EXIT_REGISTRY_CONFLICT;
        } else if end.off_path && code == EXIT_OK {
            code = EXIT_PROVIDER_CLI_MISSING;
        }
    }
    if options.json {
        let missing: Vec<ProviderKind> = kinds
            .iter()
            .copied()
            .filter(|kind| !binaries.iter().any(|binary| binary.kind == *kind))
            .collect();
        let shape = Shape {
            code,
            applied: options.apply,
            registry_unknown: registry_unknown.is_some(),
            next_action: binaries.is_empty().then(|| rerun_command(&options)),
            missing,
        };
        println!(
            "{}",
            document(shape, &binaries, &unsupported, &rows, &notes)
        );
    } else {
        print_closing(&rows, options.apply, code, &binaries, home.as_deref());
    }
    Ok(code)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn selector(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "CLAUDE_CONFIG_DIR",
        ProviderKind::Codex => "CODEX_HOME",
    }
}

fn marker_names(kind: ProviderKind) -> &'static [&'static str] {
    match kind {
        ProviderKind::Claude => CLAUDE_MARKERS,
        ProviderKind::Codex => CODEX_MARKERS,
    }
}

/// The marker names present directly in `dir`: the directory is opened `O_NOFOLLOW` and each
/// name is examined with `fstatat` without following a link, so no file under it is opened.
fn markers_in(dir: &Path, names: &[&'static str]) -> std::io::Result<Vec<&'static str>> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, open, statat};

    let directory = open(
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut found = Vec::new();
    for name in names {
        match statat(&directory, *name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                let kind = FileType::from_raw_mode(stat.st_mode);
                if kind == FileType::RegularFile || kind == FileType::Symlink {
                    found.push(*name);
                }
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(found)
}

/// Why `spec`'s context must not be handed to its CLI: it is absent, or holds no login marker.
/// `None` when it may be probed. `af provider status` asks this before any process starts.
pub(super) fn unmarked_context(spec: &ProviderSpec) -> Option<String> {
    let auth_dir = spec.auth_dir.as_deref()?;
    let names = marker_names(spec.kind);
    let mut markers = match markers_in(auth_dir, names) {
        Ok(markers) => markers,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Some(format!("{} does not exist", auth_dir.display()));
        }
        // A directory that grants search but not read: its markers by name, as detect does.
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {
            markers_found(auth_dir, names).0
        }
        Err(error) => {
            return Some(format!(
                "{} cannot be inspected: {error}",
                auth_dir.display()
            ));
        }
    };
    if markers.is_empty() && default_claude_context(spec.kind, auth_dir, home_dir().as_deref()) {
        markers.push(DEFAULT_CLAUDE_MARKER);
    }
    if markers.is_empty() {
        return Some(format!(
            "{} holds no {}",
            auth_dir.display(),
            alternatives(names)
        ));
    }
    None
}

/// The marker a default Claude context holds beside itself rather than inside: the file the CLI
/// keeps `~/.claude`'s state in.
const DEFAULT_CLAUDE_MARKER: &str = "~/.claude.json";

/// Whether `auth_dir` is the Claude CLI's default context — `~/.claude`, by its canonical path —
/// with a regular file `~/.claude.json` beside it, where the CLI keeps that context's state. Such
/// a directory needs no marker inside it. The one predicate `af provider status` and `detect`
/// both ask, so the two agree on what a default context is.
fn default_claude_context(kind: ProviderKind, auth_dir: &Path, home: Option<&Path>) -> bool {
    let Some(home) = home else {
        return false;
    };
    if kind != ProviderKind::Claude {
        return false;
    }
    let default = home.join(".claude");
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(auth_dir) == canonical(&default)
        && fs::symlink_metadata(home.join(".claude.json")).is_ok_and(|meta| meta.is_file())
}

/// `a`, `a or b`, `a, b or c`.
fn alternatives(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

fn candidate(
    kind: ProviderKind,
    named: PathBuf,
    source: Source,
    explicit_id: Option<String>,
) -> Candidate {
    let canonical = fs::canonicalize(&named).ok();
    let (mut markers, listing) = match canonical.as_deref() {
        Some(path) => markers_found(path, marker_names(kind)),
        None => (Vec::new(), None),
    };
    if markers.is_empty()
        && let Some(path) = canonical.as_deref()
        && default_claude_context(kind, path, home_dir().as_deref())
    {
        markers.push(DEFAULT_CLAUDE_MARKER);
    }
    let safety = match (fs::symlink_metadata(&named), canonical.as_deref()) {
        (Err(error), _) if error.kind() == ErrorKind::NotFound => {
            Err(format!("{} does not exist", named.display()))
        }
        (Err(error), _) => Err(format!("{} cannot be inspected: {error}", named.display())),
        (Ok(meta), Some(target)) if meta.file_type().is_symlink() => Err(format!(
            "{} is a symlink; pass the directory it points to, {}",
            named.display(),
            target.display()
        )),
        (Ok(_), Some(canonical)) => validate_private_auth_directory(canonical),
        (Ok(_), None) => Err(format!("{} cannot be resolved", named.display())),
    };
    Candidate {
        kind,
        named,
        canonical,
        source,
        explicit_id,
        markers,
        safety,
        registered_as: None,
        listing,
    }
}

/// The marker names in `dir`, by [`markers_in`]; when the directory cannot be opened for
/// listing (a mode that grants search but not read), each name is examined by its path instead,
/// without following a link, and the second value says why. Never an empty directory for one
/// that could not be listed.
fn markers_found(dir: &Path, names: &[&'static str]) -> (Vec<&'static str>, Option<String>) {
    match markers_in(dir, names) {
        Ok(markers) => (markers, None),
        Err(error) => {
            let markers = names
                .iter()
                .copied()
                .filter(|name| {
                    fs::symlink_metadata(dir.join(name)).is_ok_and(|meta| {
                        meta.file_type().is_file() || meta.file_type().is_symlink()
                    })
                })
                .collect();
            (
                markers,
                Some(format!(
                    "listing unreadable ({error}); its login markers were checked by name"
                )),
            )
        }
    }
}

/// One candidate per kind and canonical directory, keeping the most specific source.
fn deduplicate(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut kept: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        let key = (candidate.kind, candidate.path().to_path_buf());
        match kept
            .iter_mut()
            .find(|existing| (existing.kind, existing.path().to_path_buf()) == key)
        {
            Some(existing) if candidate.source.precedence() < existing.source.precedence() => {
                *existing = candidate;
            }
            Some(_) => {}
            None => kept.push(candidate),
        }
    }
    kept
}

/// The selector variable's directory, the default directory and the sibling names, for one
/// kind: existing directories only, by name; nothing below them is read.
fn scanned_dirs(
    kind: ProviderKind,
    home: Option<&Path>,
    notes: &mut Vec<String>,
) -> Vec<(PathBuf, Source)> {
    let mut found = Vec::new();
    let variable = selector(kind);
    if let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            notes.push(format!(
                "{variable} is not an absolute path, so it was not scanned"
            ));
        } else if fs::metadata(&path).is_ok_and(|meta| meta.is_dir()) {
            found.push((path, Source::Env));
        } else {
            notes.push(format!(
                "{variable} names {}, which is not a directory",
                path.display()
            ));
        }
    }
    if let Some(home) = home {
        found.extend(home_dirs(kind, home));
    }
    found
}

/// `~/.claude` or `~/.codex`, then the literal one-level sibling names, sorted: `~/.<kind>-*`,
/// `~/.config/<kind>` and `~/.config/<kind>-*`.
fn home_dirs(kind: ProviderKind, home: &Path) -> Vec<(PathBuf, Source)> {
    let name = kind.name();
    let mut found = Vec::new();
    let default = home.join(format!(".{name}"));
    if fs::metadata(&default).is_ok_and(|meta| meta.is_dir()) {
        found.push((default, Source::Default));
    }
    let mut siblings = Vec::new();
    let prefix = format!(".{name}-");
    siblings.extend(
        directory_names(home)
            .into_iter()
            .filter(|entry| entry.starts_with(&prefix) && entry.len() > prefix.len())
            .map(|entry| home.join(entry)),
    );
    let config = home.join(".config");
    let prefix = format!("{name}-");
    siblings.extend(
        directory_names(&config)
            .into_iter()
            .filter(|entry| {
                entry == name || (entry.starts_with(&prefix) && entry.len() > prefix.len())
            })
            .map(|entry| config.join(entry)),
    );
    siblings.sort();
    found.extend(
        siblings
            .into_iter()
            .filter(|path| fs::metadata(path).is_ok_and(|meta| meta.is_dir()))
            .map(|path| (path, Source::Sibling)),
    );
    found
}

/// The UTF-8 entry names of `directory`, or none when it cannot be listed.
fn directory_names(directory: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

/// The auth directories a names-only scan finds for both kinds — the selector variables', the
/// defaults and the siblings — canonical and deduplicated. `af provider status` lists them when
/// no Provider is registered; no marker is examined and nothing is probed.
pub(super) fn names_only_scan() -> Vec<PathBuf> {
    let home = home_dir();
    let mut ignored = Vec::new();
    let mut found = Vec::new();
    for kind in [ProviderKind::Claude, ProviderKind::Codex] {
        for (path, _) in scanned_dirs(kind, home.as_deref(), &mut ignored) {
            let path = fs::canonicalize(&path).unwrap_or(path);
            if !found.contains(&path) {
                found.push(path);
            }
        }
    }
    found
}

/// The logins `af provider detect --apply` would register now, each with its kind: the scan
/// it runs without `--all`, every marked directory probed by an available CLI of its kind, kept
/// only where the login is authenticated, its directory safe and unregistered, and its CLI on
/// `PATH` (`--apply` skips one found only off it). A marker alone is no login, and a logged-out
/// or CLI-less directory registers nothing; only what this returns can clear a check that
/// offers `af provider detect --apply` (#331 D5). A registry that cannot be read is taken as
/// naming nothing: `--apply` then refuses until it is corrected, which the caller says.
pub(super) fn registrable_logins() -> Vec<(PathBuf, ProviderKind)> {
    let specs = match registry_path() {
        Ok(Some(path)) => ensure_no_registry_transaction(&path)
            .and_then(|()| {
                read_registry(&path).and_then(|text| {
                    text.map(|text| parse_registry(&text, &path))
                        .transpose()
                        .map(Option::unwrap_or_default)
                })
            })
            .unwrap_or_default(),
        Ok(None) | Err(_) => Vec::new(),
    };
    let home = home_dir();
    let kinds = [ProviderKind::Claude, ProviderKind::Codex];
    let mut ignored = Vec::new();
    let mut candidates = Vec::new();
    for kind in kinds {
        for (path, source) in scanned_dirs(kind, home.as_deref(), &mut ignored) {
            candidates.push(candidate(kind, path, source, None));
        }
    }
    let mut candidates = deduplicate(candidates);
    for candidate in &mut candidates {
        candidate.registered_as = specs
            .iter()
            .find(|spec| {
                spec.kind == candidate.kind
                    && spec.auth_dir.as_deref() == candidate.canonical.as_deref()
            })
            .map(|spec| spec.id.clone());
    }
    let binaries: Vec<Binary> = kinds
        .iter()
        .filter_map(|kind| find_binary(*kind, home.as_deref()))
        .collect();
    if binaries.is_empty() || candidates.is_empty() {
        return Vec::new();
    }
    let (binaries, mut rows) = probe_all(binaries, candidates, false, &AtomicBool::new(false));
    let ids: BTreeSet<String> = specs.iter().map(|spec| spec.id.clone()).collect();
    suggest(&mut rows, &binaries, &ids, false, home.as_deref());
    rows.into_iter()
        .filter(|row| {
            row.auth == Auth::Authenticated
                && row.candidate.safety.is_ok()
                && row.candidate.registered_as.is_none()
                && row.suggested.action == "register"
        })
        .filter_map(|row| Some((row.candidate.canonical?, row.candidate.kind)))
        .collect()
}

/// The install directories of the fixed list, in order, for one kind.
fn install_dirs(kind: ProviderKind, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        if kind == ProviderKind::Claude {
            dirs.push(home.join(".claude/local"));
        }
    }
    dirs.extend(system_bin_dirs());
    if let Some(home) = home {
        for dir in [".npm-global/bin", ".volta/bin", ".bun/bin"] {
            dirs.push(home.join(dir));
        }
    }
    dirs
}

fn system_bin_dirs() -> Vec<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(dirs) = std::env::var_os("AF_TEST_DETECT_SYSTEM_DIRS") {
        return std::env::split_paths(&dirs)
            .filter(|dir| dir.is_absolute())
            .collect();
    }
    SYSTEM_BIN_DIRS.iter().map(PathBuf::from).collect()
}

/// The kind's CLI: on the sanitized `PATH` first, as every probe resolves it, then the first
/// executable entry of the fixed install directories. Metadata only; nothing is run here.
fn locate_binary(kind: ProviderKind, home: Option<&Path>) -> Option<(PathBuf, bool)> {
    if let Some(program) = resolve_program(kind.command()) {
        return Some((program, true));
    }
    install_dirs(kind, home)
        .into_iter()
        .map(|dir| dir.join(kind.command()))
        .find(|candidate| is_executable(candidate))
        .map(|candidate| (candidate, false))
}

fn find_binary(kind: ProviderKind, home: Option<&Path>) -> Option<Binary> {
    let (path, on_path) = locate_binary(kind, home)?;
    Some(Binary {
        kind,
        path,
        on_path,
        version: None,
        compatible: None,
        refusal: None,
    })
}

/// The unsupported harness CLIs on the sanitized `PATH`, each asked only for `--version`, in the
/// probe environment, bounded like every probe.
fn find_unsupported() -> Vec<Unsupported> {
    let found: Vec<(&'static str, PathBuf)> = UNSUPPORTED
        .iter()
        .filter_map(|name| resolve_program(name).map(|path| (*name, path)))
        .collect();
    let spec = neutral_spec();
    let probe_path = sanitized_path();
    let cancelled = AtomicBool::new(false);
    thread::scope(|scope| {
        let handles: Vec<_> = found
            .into_iter()
            .map(|(name, path)| {
                let (spec, probe_path, cancelled) = (&spec, &probe_path, &cancelled);
                scope.spawn(move || {
                    let version = match run_bounded(
                        &path,
                        &["--version"],
                        spec,
                        probe_path,
                        cancelled,
                        None,
                        probe_timeout(PROBE_TIMEOUT),
                    ) {
                        BoundedRun::Exited { stdout, stderr, .. } => {
                            version_token(&String::from_utf8_lossy(&stdout))
                                .or_else(|| version_token(&String::from_utf8_lossy(&stderr)))
                        }
                        BoundedRun::Unstartable(_) | BoundedRun::GaveUp(_) => None,
                    };
                    Unsupported {
                        name,
                        path,
                        version,
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect()
    })
}

/// A probe environment that names no auth directory: for an unsupported CLI's `--version`.
fn neutral_spec() -> ProviderSpec {
    neutral_spec_of(ProviderKind::Codex)
}

/// A probe environment of `kind` that names no auth directory: what a CLI of that kind is
/// checked in when no private scratch directory can be made, so a Claude CLI is never checked
/// against Codex's capabilities.
fn neutral_spec_of(kind: ProviderKind) -> ProviderSpec {
    ProviderSpec {
        id: "detect".to_string(),
        kind,
        auth_dir: None,
        explicit_selector: false,
        registry_declared: false,
        source: "af provider detect".to_string(),
    }
}

/// The probe spec of one candidate: its directory always handed to the CLI through the selector
/// variable, exactly as a registered Provider's is, so `authenticated` here means `add` will see
/// the same login.
fn probe_spec(kind: ProviderKind, auth_dir: &Path) -> ProviderSpec {
    ProviderSpec {
        id: kind.name().to_string(),
        kind,
        auth_dir: Some(auth_dir.to_path_buf()),
        explicit_selector: true,
        registry_declared: false,
        source: "af provider detect".to_string(),
    }
}

/// The first version-shaped token of `text` — digits and dots, an optional pre-release — so a
/// version af reports is af's own reading, never the CLI's words.
fn version_token(text: &str) -> Option<String> {
    for word in text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',')) {
        let word = word.strip_prefix('v').unwrap_or(word);
        let mut parts = word.splitn(2, ['-', '+']);
        let core = parts.next().unwrap_or_default();
        let numbers: Vec<&str> = core.split('.').collect();
        if !(2..=4).contains(&numbers.len())
            || !numbers.iter().all(|part| {
                !part.is_empty() && part.len() <= 9 && part.bytes().all(|b| b.is_ascii_digit())
            })
        {
            continue;
        }
        let token = match parts.next() {
            Some(pre)
                if !pre.is_empty()
                    && pre
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') =>
            {
                word.to_string()
            }
            _ => core.to_string(),
        };
        if token.len() <= 32 {
            return Some(token);
        }
    }
    None
}

/// The version check and the capability check once per binary, then each candidate's probe, at
/// most four at a time. A candidate without a marker is probed only under `--all`; an unsafe or
/// absent one never is.
fn probe_all(
    mut binaries: Vec<Binary>,
    candidates: Vec<Candidate>,
    all: bool,
    cancelled: &AtomicBool,
) -> (Vec<Binary>, Vec<Row>) {
    let probe_path = sanitized_path();
    let probed = |candidate: &Candidate| {
        candidate.safety.is_ok()
            && candidate.canonical.is_some()
            && (all || !candidate.markers.is_empty())
    };
    // A private empty directory stands in when no candidate of a kind is probed, so the
    // capability check never runs a CLI against a directory of the operator's.
    let scratch = tempfile::tempdir().ok();
    thread::scope(|scope| {
        let handles: Vec<_> = binaries
            .iter_mut()
            .map(|binary| {
                let auth_dir = candidates
                    .iter()
                    .find(|candidate| candidate.kind == binary.kind && probed(candidate))
                    .and_then(|candidate| candidate.canonical.clone())
                    .or_else(|| scratch.as_ref().map(|dir| dir.path().to_path_buf()));
                let probe_path = &probe_path;
                scope
                    .spawn(move || check_binary(binary, auth_dir.as_deref(), probe_path, cancelled))
            })
            .collect();
        for handle in handles {
            let _ = handle.join();
        }
    });
    let mut rows = Vec::with_capacity(candidates.len());
    let mut pending = Vec::new();
    for candidate in candidates {
        let binary = binaries.iter().find(|binary| binary.kind == candidate.kind);
        match unprobed_row(&candidate, binary, probed(&candidate)) {
            Some((auth, login, note)) => rows.push((candidate, auth, login, note, None)),
            None => pending.push(candidate),
        }
    }
    for chunk in pending.chunks(MAX_CONCURRENT_PROBES) {
        thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|candidate| {
                    let binary = binaries
                        .iter()
                        .find(|binary| binary.kind == candidate.kind)
                        .expect("only a candidate whose CLI was found is probed");
                    let spec = probe_spec(candidate.kind, candidate.path());
                    let program = binary.path.clone();
                    let probe_path = &probe_path;
                    scope.spawn(move || {
                        probe_checked(
                            &program,
                            spec,
                            "",
                            probe_path,
                            cancelled,
                            UsageProbe::Skip,
                            Some(probe_timeout(DETECT_PROBE_TIMEOUT)),
                        )
                    })
                })
                .collect();
            for (candidate, handle) in chunk.iter().zip(handles) {
                let (status, answer) = handle.join().unwrap_or_else(|_| {
                    let spec = probe_spec(candidate.kind, candidate.path());
                    (
                        unavailable_status(&spec, "provider status probe panicked"),
                        String::new(),
                    )
                });
                let auth = Auth::of(&status.status);
                let login = login_label(candidate.kind, auth, &status);
                let note = match auth {
                    Auth::Authenticated | Auth::NotAuthenticated => None,
                    _ if status.detail.is_empty() => None,
                    _ => Some(format!("{}: {}", auth.name(), status.detail)),
                };
                let extras = Some((status, answer));
                rows.push((clone_candidate(candidate), auth, login, note, extras));
            }
        });
    }
    let rows = rows
        .into_iter()
        .map(|(candidate, auth, login, note, extras)| {
            let (auth_method, plan, account) = match &extras {
                Some((status, answer)) => (
                    auth_method(candidate.kind, auth, &status.auth_type),
                    plan_name(candidate.kind, auth, &status.subscription),
                    account_label(candidate.kind, auth, answer),
                ),
                None => ("unknown", "unknown", None),
            };
            let (cause, detail) = match &extras {
                Some((status, _)) => probe_cause(auth, &status.detail),
                None => unprobed_cause(auth, candidate.kind, &binaries),
            };
            // A directory that could not be listed says so beside whatever its probe said.
            let note = match (&candidate.listing, note) {
                (Some(listing), Some(note)) => Some(format!("{listing}; {note}")),
                (Some(listing), None) => Some(listing.clone()),
                (None, note) => note,
            };
            Row {
                candidate,
                auth,
                login,
                auth_method,
                plan,
                account,
                note,
                cause,
                detail,
                suggested: Suggested::none(),
                applied: None,
            }
        })
        .collect::<Vec<_>>();
    let mut rows = rows;
    rows.sort_by(|left, right| {
        let key = |row: &Row| {
            (
                row.candidate.kind,
                row.candidate.source,
                row.candidate.path().to_path_buf(),
            )
        };
        key(left).cmp(&key(right))
    });
    (binaries, rows)
}

/// Why a probed row's login is not known, in af's closed vocabulary, and af's own words for it
/// when they quote no CLI: `timeout`, `unstartable`, `output_too_large`, `unparsable`,
/// `cancelled`, `incompatible` or `other`. `None` for a login the probe answered.
fn probe_cause(auth: Auth, detail: &str) -> (Option<&'static str>, Option<String>) {
    let own = || (!detail.is_empty()).then(|| detail.to_string());
    match auth {
        Auth::Authenticated | Auth::NotAuthenticated | Auth::NotProbed => (None, None),
        // The answer was not one af understands; it may quote the CLI, so only the cause.
        Auth::Unknown => (Some("unparsable"), None),
        // The CLI's own error line goes to stderr and the table, never into the document.
        Auth::InstallationFailed => (Some("unstartable"), None),
        Auth::Incompatible => (Some("incompatible"), None),
        Auth::ProbeDenied => (Some("probe_denied"), None),
        Auth::Unavailable if detail.contains("timed out after") => (Some("timeout"), own()),
        Auth::Unavailable if detail.starts_with("cannot start provider status probe") => {
            (Some("unstartable"), own())
        }
        Auth::Unavailable if detail.contains("output exceeds") => (Some("output_too_large"), own()),
        Auth::Unavailable if detail.contains("not UTF-8") => (Some("unparsable"), own()),
        Auth::Unavailable if detail.contains("cancelled") => (Some("cancelled"), own()),
        Auth::Unavailable => (Some("other"), own()),
    }
}

/// Why a row that got no probe has no login state, from its CLI's capability check: `unchecked`
/// when af could not tell whether the CLI accepts its invocation, with af's reason.
fn unprobed_cause(
    auth: Auth,
    kind: ProviderKind,
    binaries: &[Binary],
) -> (Option<&'static str>, Option<String>) {
    match auth {
        Auth::Incompatible => (Some("incompatible"), None),
        Auth::ProbeDenied => (Some("probe_denied"), None),
        Auth::InstallationFailed => (Some("unstartable"), None),
        Auth::Unavailable => (
            Some("unchecked"),
            binaries
                .iter()
                .find(|binary| binary.kind == kind)
                .and_then(|binary| binary.refusal.as_ref())
                .map(|(_, reason)| reason.clone()),
        ),
        _ => (None, None),
    }
}

fn clone_candidate(candidate: &Candidate) -> Candidate {
    Candidate {
        kind: candidate.kind,
        named: candidate.named.clone(),
        canonical: candidate.canonical.clone(),
        source: candidate.source,
        explicit_id: candidate.explicit_id.clone(),
        markers: candidate.markers.clone(),
        safety: candidate.safety.clone(),
        registered_as: candidate.registered_as.clone(),
        listing: candidate.listing.clone(),
    }
}

/// The version check and the capability check of one binary, run once, in the probe
/// environment of `auth_dir`.
fn check_binary(
    binary: &mut Binary,
    auth_dir: Option<&Path>,
    probe_path: &std::ffi::OsStr,
    cancelled: &AtomicBool,
) {
    let spec = capability_spec(binary.kind, auth_dir);
    binary.version = match version_check(&binary.path, &spec, probe_path, cancelled, None) {
        VersionCheck::Reported(Some(line)) => version_token(&line),
        _ => None,
    };
    match check_invocation(&binary.path, &spec, probe_path, cancelled, None) {
        Ok(()) => binary.compatible = Some(true),
        Err(CliRefusal::CannotStart(failure)) => {
            binary.compatible = Some(false);
            binary.refusal = Some((INSTALLATION_FAILED.to_string(), failure.message()));
        }
        Err(CliRefusal::Incompatible(incompatibility)) => {
            binary.compatible = Some(false);
            binary.refusal = Some((INCOMPATIBLE.to_string(), incompatibility.message()));
        }
        Err(CliRefusal::Denied(denied)) => {
            binary.compatible = None;
            binary.refusal = Some((PROBE_DENIED.to_string(), denied.message()));
        }
        Err(CliRefusal::Unchecked(reason)) => {
            binary.compatible = None;
            binary.refusal = Some(("unavailable".to_string(), reason));
        }
    }
}

/// The environment a CLI of `kind` is checked in: `auth_dir`'s, or with none (no private scratch
/// directory could be made) its own kind's without one, never another kind's capabilities.
fn capability_spec(kind: ProviderKind, auth_dir: Option<&Path>) -> ProviderSpec {
    match auth_dir {
        Some(auth_dir) => probe_spec(kind, auth_dir),
        None => neutral_spec_of(kind),
    }
}

/// A row that gets no probe, and why: `None` when the candidate is probed.
fn unprobed_row(
    candidate: &Candidate,
    binary: Option<&Binary>,
    probed: bool,
) -> Option<(Auth, String, Option<String>)> {
    let kind = candidate.kind.name();
    if let Err(reason) = &candidate.safety {
        let login = if candidate.canonical.is_none() {
            "absent (not probed)"
        } else {
            "unsafe (not probed)"
        };
        return Some((
            Auth::NotProbed,
            login.to_string(),
            Some(format!("safe: no: {reason}")),
        ));
    }
    if !probed {
        return Some((
            Auth::NotProbed,
            "empty (not probed)".to_string(),
            Some(format!(
                "empty: no {}, so the {kind} CLI was not asked about it (it would create files \
                 there); af provider detect --all asks it",
                alternatives(marker_names(candidate.kind))
            )),
        ));
    }
    let Some(binary) = binary else {
        return Some((Auth::NotProbed, format!("no {kind} CLI (not probed)"), None));
    };
    match &binary.refusal {
        Some((status, _)) if binary.compatible == Some(false) || status == PROBE_DENIED => {
            let auth = Auth::of(status);
            let login = match auth {
                Auth::Incompatible => "CLI incompatible",
                Auth::ProbeDenied => "probe denied",
                _ => "CLI cannot start",
            };
            Some((
                auth,
                login.to_string(),
                Some(format!(
                    "the {kind} CLI {} cannot serve af, so no login was asked about; see stderr",
                    binary.path.display()
                )),
            ))
        }
        Some((_, reason)) => Some((
            Auth::Unavailable,
            "unavailable".to_string(),
            Some(format!("unavailable: {reason}")),
        )),
        None => None,
    }
}

/// The plan or method label of a probed row: never an email or an organization.
fn login_label(kind: ProviderKind, auth: Auth, status: &ProviderStatus) -> String {
    match auth {
        Auth::Authenticated => {}
        Auth::NotAuthenticated => return "not logged in".to_string(),
        Auth::Unknown => return "unknown answer".to_string(),
        Auth::Unavailable => return "unavailable".to_string(),
        Auth::InstallationFailed => return "CLI cannot start".to_string(),
        Auth::Incompatible => return "CLI incompatible".to_string(),
        Auth::ProbeDenied => return "probe denied".to_string(),
        Auth::NotProbed => return "not probed".to_string(),
    }
    let method = status
        .auth_type
        .split('/')
        .next()
        .unwrap_or_default()
        .trim();
    match (kind, method) {
        (ProviderKind::Claude, "api_key") | (ProviderKind::Codex, "API key") => "API key".into(),
        (ProviderKind::Codex, "ChatGPT") => "ChatGPT".into(),
        (ProviderKind::Claude, "claude.ai" | "oauth") => {
            let plan = status
                .subscription
                .strip_prefix("Claude ")
                .filter(|plan| !plan.starts_with("subscription"))
                .map(|plan| format!("Claude {plan}"))
                .unwrap_or_else(|| "Claude".to_string());
            format!("{plan} ({method})")
        }
        (ProviderKind::Claude, _) => match status.subscription.as_str() {
            "AWS Bedrock billing" => "AWS Bedrock".into(),
            "Google Vertex billing" => "Google Vertex".into(),
            _ => "Claude (other method)".into(),
        },
        (ProviderKind::Codex, _) => "Codex (other method)".into(),
    }
}

fn auth_method(kind: ProviderKind, auth: Auth, auth_type: &str) -> &'static str {
    match auth {
        Auth::Authenticated => {}
        Auth::NotAuthenticated => return "none",
        _ => return "unknown",
    }
    match credential_name(kind.name(), auth_type) {
        "api_key" => "api_key",
        "subscription" => "oauth",
        _ => "unknown",
    }
}

/// af's own plan name. The Codex plan is not part of the status probe's answer, so it stays
/// `unknown`; Claude Enterprise is the organizational plan and reads `business`.
fn plan_name(kind: ProviderKind, auth: Auth, subscription: &str) -> &'static str {
    if auth != Auth::Authenticated || kind != ProviderKind::Claude {
        return "unknown";
    }
    match subscription {
        "Claude Max" => "max",
        "Claude Team" => "team",
        "Claude Pro" => "pro",
        "Claude Enterprise" => "business",
        "Claude Free" => "free",
        _ => "unknown",
    }
}

/// The account label the harness reported, through the one credential filter; only Claude's
/// status answer carries one.
fn account_label(kind: ProviderKind, auth: Auth, answer: &str) -> Option<String> {
    if kind != ProviderKind::Claude || auth != Auth::Authenticated {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(answer).ok()?;
    ["email", "orgName", "organizationName"]
        .into_iter()
        .find_map(|field| parsed.get(field).and_then(serde_json::Value::as_str))
        .map(|label| safe_display(label, "-"))
}

/// The ID a directory registers under when no `--id` names it: `<kind>-main` for the CLI's
/// default directory; for a sibling, the kind and the directory name with the kind prefix
/// stripped; for the selector variable's directory the sibling rule when its name follows a
/// sibling pattern, else `<kind>-env`; for `--dir` the sibling rule.
fn derived_id(candidate: &Candidate, home: Option<&Path>) -> String {
    let kind = candidate.kind.name();
    let path = candidate.path();
    let default = home.map(|home| home.join(format!(".{kind}")));
    let canonical_default = default
        .as_deref()
        .and_then(|dir| fs::canonicalize(dir).ok());
    let is_default = candidate.source == Source::Default
        || default.as_deref() == Some(candidate.named.as_path())
        || canonical_default.as_deref() == Some(path);
    if is_default {
        return format!("{kind}-main");
    }
    match candidate.source {
        Source::Env if !sibling_pattern(candidate.kind, &candidate.named, home) => {
            format!("{kind}-env")
        }
        _ => sibling_id(candidate.kind, &candidate.named, home),
    }
}

/// Whether `path` is one of the sibling names the scan looks for.
fn sibling_pattern(kind: ProviderKind, path: &Path, home: Option<&Path>) -> bool {
    let (Some(home), Some(parent), Some(name)) = (
        home,
        path.parent(),
        path.file_name().and_then(|name| name.to_str()),
    ) else {
        return false;
    };
    let kind = kind.name();
    (parent == home && name.starts_with(&format!(".{kind}-")))
        || (parent == home.join(".config")
            && (name == kind || name.starts_with(&format!("{kind}-"))))
}

/// The kind, then the directory name without its leading dot and kind prefix: `~/.codex-acme`
/// is `codex-acme`, `~/.config/claude-work` is `claude-work`, `~/.config/claude` is
/// `claude-config`.
fn sibling_id(kind: ProviderKind, path: &Path, home: Option<&Path>) -> String {
    let kind = kind.name();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bare = name.trim_start_matches('.');
    let rest = if bare == kind {
        let in_config =
            home.is_some_and(|home| path.parent() == Some(home.join(".config").as_path()));
        if in_config { "config" } else { "" }
    } else {
        bare.strip_prefix(kind)
            .and_then(|rest| rest.strip_prefix(['-', '_']))
            .unwrap_or(bare)
    };
    let mut cleaned = String::new();
    for character in rest.chars() {
        let character = if character.is_ascii_alphanumeric() || character == '_' {
            character
        } else {
            '-'
        };
        if !(character == '-' && cleaned.ends_with('-')) {
            cleaned.push(character);
        }
    }
    let cleaned = cleaned.trim_matches(['-', '_']);
    let mut id = if cleaned.is_empty() {
        format!("{kind}-main")
    } else {
        format!("{kind}-{cleaned}")
    };
    id.truncate(48);
    id.trim_end_matches(['-', '_']).to_string()
}

/// `base`, or `base-2`, `base-3`, … the first that no registered or suggested ID holds; an
/// ambient label is never generated.
fn unique_id(base: &str, taken: &BTreeSet<String>) -> String {
    let free = |id: &str| !taken.contains(id) && !id.ends_with("-ambient");
    if free(base) {
        return base.to_string();
    }
    (2..)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| free(candidate))
        .expect("the finite registry cannot exhaust numeric suffixes")
}

/// `~/rest` for a path below `HOME` whose rest needs no quoting, else the shell-quoted path.
fn tilde(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
        && let Some(rest) = rest.to_str()
        && !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
    {
        return format!("~/{rest}");
    }
    path.to_str()
        .map(|path| shell_words::quote(path).into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A directory as af prints it to a person: `~/…` below `HOME`.
pub(crate) fn display_path(path: &Path) -> String {
    display_below(path, home_dir().as_deref())
}

/// [`tilde`] below `home` as given or as its canonical path: the scan's directories are
/// canonical, so one below a `home` reached through a symbolic link (macOS's `/var`, say) reads
/// `~/…` as it does below any other `home` (#331).
fn display_below(path: &Path, home: Option<&Path>) -> String {
    let shown = tilde(path, home);
    if shown.starts_with("~/") {
        return shown;
    }
    let canonical = home.and_then(|home| fs::canonicalize(home).ok());
    tilde(path, canonical.as_deref())
}

fn quoted(path: &Path) -> String {
    path.to_str()
        .map(|path| shell_words::quote(path).into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The one-line PATH fix for the operator's `$SHELL`: `fish_add_path <dir>` for fish, otherwise
/// `export PATH="<dir>:$PATH"`, `$HOME` spelled out so the line pastes into any shell.
fn path_fix(program: &Path, home: Option<&Path>) -> String {
    let dir = program.parent().unwrap_or(program);
    let fish = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .is_some_and(|shell| shell.file_name().is_some_and(|name| name == "fish"));
    path_fix_for(dir, home, fish)
}

fn path_fix_for(dir: &Path, home: Option<&Path>, fish: bool) -> String {
    let short = tilde(dir, home);
    if fish {
        return format!("fish_add_path {short}");
    }
    let spelled = match short.strip_prefix("~/") {
        Some(rest) => format!("$HOME/{rest}"),
        None => match dir.to_str() {
            Some(dir) if !dir.contains(['"', '$', '`', '\\']) => dir.to_string(),
            _ => return format!("add {} to PATH", dir.display()),
        },
    };
    format!("export PATH=\"{spelled}:$PATH\"")
}

/// The official install commands of a kind, the first being the one a note names.
fn install_commands(kind: ProviderKind) -> [&'static str; 2] {
    match kind {
        ProviderKind::Claude => [
            "npm install -g @anthropic-ai/claude-code",
            "curl -fsSL https://claude.ai/install.sh | bash",
        ],
        ProviderKind::Codex => ["npm install -g @openai/codex", "brew install codex"],
    }
}

/// The upgrade command for a CLI that cannot start or does not accept af's invocation.
fn upgrade_command(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "npm install -g @anthropic-ai/claude-code@latest",
        ProviderKind::Codex => "npm install -g @openai/codex@latest",
    }
}

/// Each row's suggestion, the IDs assigned in table order so none collides with a registered
/// or an earlier suggested one.
fn suggest(
    rows: &mut [Row],
    binaries: &[Binary],
    registry_ids: &BTreeSet<String>,
    registry_unknown: bool,
    home: Option<&Path>,
) {
    let mut taken = registry_ids.clone();
    taken.insert("claude-ambient".to_string());
    taken.insert("codex-ambient".to_string());
    for row in rows.iter_mut() {
        let candidate = &row.candidate;
        let kind = candidate.kind;
        let path = candidate.path().to_path_buf();
        let id = || -> String {
            match &candidate.explicit_id {
                Some(id) => id.clone(),
                None => unique_id(&derived_id(candidate, home), &taken),
            }
        };
        let command = |id: &str, login: bool, spell: &dyn Fn(&Path) -> String| {
            let verb = if login { "setup" } else { "add" };
            let mut line = format!(
                "af provider {verb} {} --kind {} --auth-dir {}",
                shell_words::quote(id),
                kind.name(),
                spell(&path)
            );
            if login {
                line.push_str(" --login");
            }
            line
        };
        let tilde_spell = |path: &Path| tilde(path, home);
        let suggested = match (row.auth, &candidate.registered_as, &candidate.safety) {
            (_, _, Err(reason)) if candidate.canonical.is_some() => Suggested {
                action: "fix_safety",
                id: None,
                command: reason.split_once("; fix: ").map(|(_, fix)| fix.to_string()),
                display: reason.split_once("; fix: ").map(|(_, fix)| fix.to_string()),
            },
            (_, _, Err(_)) => Suggested::none(),
            (Auth::Authenticated, Some(_), _) => Suggested::none(),
            // Whether it is registered is unknown: no ID can be chosen, and the registry first.
            (Auth::Authenticated, None, _) if registry_unknown => Suggested {
                action: "fix_registry",
                id: None,
                command: Some("af provider status".to_string()),
                display: Some("af provider status".to_string()),
            },
            // A login whose CLI is off `PATH` cannot be used once registered: the PATH first.
            (Auth::Authenticated, None, _)
                if binaries
                    .iter()
                    .any(|binary| binary.kind == kind && !binary.on_path) =>
            {
                let binary = binaries
                    .iter()
                    .find(|binary| binary.kind == kind)
                    .expect("the CLI of this kind was found");
                let fix = path_fix(&binary.path, home);
                let id = id();
                taken.insert(id.clone());
                Suggested {
                    action: "fix_path",
                    id: Some(id),
                    command: Some(fix.clone()),
                    display: Some(fix),
                }
            }
            (Auth::Authenticated, None, _) => {
                let id = id();
                taken.insert(id.clone());
                Suggested {
                    action: "register",
                    command: Some(command(&id, false, &quoted)),
                    display: Some(command(&id, false, &tilde_spell)),
                    id: Some(id),
                }
            }
            (Auth::NotAuthenticated, registered, _) => {
                let id = match registered {
                    Some(id) => id.clone(),
                    None => id(),
                };
                taken.insert(id.clone());
                Suggested {
                    action: "login",
                    command: Some(command(&id, true, &quoted)),
                    display: Some(command(&id, true, &tilde_spell)),
                    id: Some(id),
                }
            }
            (Auth::NotProbed, _, _) if !binaries.iter().any(|binary| binary.kind == kind) => {
                Suggested {
                    action: "install",
                    id: None,
                    command: Some(install_commands(kind)[0].to_string()),
                    display: Some(install_commands(kind)[0].to_string()),
                }
            }
            (Auth::NotProbed, _, _) => Suggested {
                action: "probe",
                id: None,
                command: Some("af provider detect --all".to_string()),
                display: Some("af provider detect --all".to_string()),
            },
            (Auth::InstallationFailed | Auth::Incompatible, _, _) => Suggested {
                action: "fix_cli",
                id: None,
                command: Some(upgrade_command(kind).to_string()),
                display: Some(upgrade_command(kind).to_string()),
            },
            (Auth::ProbeDenied, _, _) => Suggested::none(),
            (Auth::Unavailable | Auth::Unknown, _, _) => Suggested {
                action: "retry",
                id: None,
                command: Some("af provider detect".to_string()),
                display: Some("af provider detect".to_string()),
            },
        };
        row.suggested = suggested;
    }
}

/// How `--apply` ended, beyond the rows it registered.
#[derive(Default)]
struct ApplyEnd {
    /// A registry conflict stopped it at a row; the rows before that one stay published.
    conflict: bool,
    /// A logged-in row was left unregistered because its CLI is off `PATH`.
    off_path: bool,
}

/// `--apply`: register every authenticated, unregistered row in table order through `add`'s
/// locked, atomic publication and safety checks, printing each line it ran. A row the scan read
/// as registered is checked again under the registry lock: an entry removed since is registered
/// again (a logged-in row) or said to be gone, never reported as still there. A logged-out row is
/// never touched; its `setup … --login` line is printed instead. A logged-in row whose CLI is
/// off `PATH` is not registered: neither `af provider status` nor a Task could run it.
fn apply(
    rows: &mut [Row],
    registry: &Path,
    json: bool,
    home: Option<&Path>,
    registry_ids: &BTreeSet<String>,
) -> Result<ApplyEnd, String> {
    let say = |line: String| {
        if !json {
            for line in wrap(&line, "") {
                println!("{line}");
            }
        }
    };
    if !json {
        println!();
    }
    let mut end = ApplyEnd::default();
    let mut registered = Vec::new();
    for index in 0..rows.len() {
        let row = &mut rows[index];
        let kind = row.candidate.kind;
        let path = row.candidate.path().to_path_buf();
        let was_registered = row.candidate.registered_as.clone();
        if row.auth == Auth::Authenticated && row.suggested.action == "fix_path" {
            let id = row
                .suggested
                .id
                .clone()
                .unwrap_or_else(|| kind.name().to_string());
            row.applied = Some(Apply::new(id, Applied::SkippedCliOffPath));
            end.off_path = true;
            say(format!(
                "not registered: {}: its {} CLI is off PATH, so neither af provider status nor a \
                 Task could run it; fix: {}, then run af provider detect --apply",
                tilde(&path, home),
                kind.name(),
                row.suggested.display.clone().unwrap_or_default()
            ));
            continue;
        }
        let id = match (&was_registered, &row.suggested.id) {
            (Some(id), _) => Some(id.clone()),
            (None, Some(id)) if row.auth == Auth::Authenticated => Some(id.clone()),
            _ => None,
        };
        if row.auth == Auth::Authenticated
            && row.candidate.safety.is_ok()
            && let Some(id) = id
        {
            let line = match &was_registered {
                Some(_) => format!(
                    "af provider add {} --kind {} --auth-dir {}",
                    shell_words::quote(&id),
                    kind.name(),
                    tilde(&path, home)
                ),
                None => row.suggested.display.clone().unwrap_or_default(),
            };
            let outcome = validate_explicit_id(&id)
                .and_then(|()| resolve_auth_dir(kind, Some(&path), MissingAuthDir::Inspect))
                .and_then(|auth_dir| add_to_registry_inner(registry, &id, kind, &auth_dir, true));
            match outcome {
                Ok(RegistryAdd::Added(_)) => {
                    row.applied = Some(Apply::new(id.clone(), Applied::Registered));
                    registered.push(id);
                    say(match was_registered {
                        Some(_) => format!("registered again, its entry was gone: {line}"),
                        None => format!("registered: {line}"),
                    });
                }
                Ok(RegistryAdd::AlreadyPresent) => {
                    row.applied = Some(Apply::new(id.clone(), Applied::AlreadyRegistered));
                    say(format!("already registered: {id} ({})", tilde(&path, home)));
                }
                Ok(RegistryAdd::Conflict(conflict)) => {
                    let taken = registry_holding(registry, kind, &path, &id);
                    conflicted(
                        rows,
                        index,
                        (id, conflict, taken),
                        registry,
                        json,
                        home,
                        registry_ids,
                    );
                    end.conflict = true;
                    return Ok(end);
                }
                Err(error) => {
                    let before = if registered.is_empty() {
                        "nothing was registered before it".to_string()
                    } else {
                        format!("registered before it: {}", registered.join(", "))
                    };
                    return Err(format!("{line} failed: {error}; {before}"));
                }
            }
            continue;
        }
        if let Some(id) = &was_registered {
            match registered_now(registry, kind, &path)? {
                Some(now) => {
                    row.applied = Some(Apply::new(now.clone(), Applied::AlreadyRegistered));
                    say(format!(
                        "already registered: {now} ({})",
                        tilde(&path, home)
                    ));
                    continue;
                }
                None => {
                    row.candidate.registered_as = None;
                    say(format!(
                        "no longer registered: {id} ({}) was removed since the scan",
                        tilde(&path, home)
                    ));
                }
            }
        }
        if let (Auth::NotAuthenticated, Some(id)) = (row.auth, row.suggested.id.clone()) {
            row.applied = Some(Apply::new(id, Applied::SkippedNotAuthenticated));
            say(format!(
                "not logged in, so not registered: {}; run this yourself at a private terminal:",
                tilde(&path, home)
            ));
            if !json {
                println!("  {}", row.suggested.display.clone().unwrap_or_default());
            }
        }
    }
    Ok(end)
}

/// The registry entry of `kind` whose auth directory is `path`, read under the registry lock:
/// what `--apply` checks again before it says a row is registered.
fn registered_now(
    registry: &Path,
    kind: ProviderKind,
    path: &Path,
) -> Result<Option<String>, String> {
    let _lock = registry_lock(registry)?;
    let specs = match read_registry(registry)? {
        Some(text) => parse_registry(&text, registry)?,
        None => Vec::new(),
    };
    Ok(specs
        .into_iter()
        .find(|spec| spec.kind == kind && spec.auth_dir.as_deref() == Some(path))
        .map(|spec| spec.id))
}

/// The registered ID a conflict at `id` for `path` met: `id` itself when another directory
/// holds it, the ID `path` is registered under otherwise, none when the registry is full or
/// cannot be read now.
fn registry_holding(registry: &Path, kind: ProviderKind, path: &Path, id: &str) -> Option<String> {
    let _lock = registry_lock(registry).ok()?;
    let specs = parse_registry(&read_registry(registry).ok()??, registry).ok()?;
    if specs.iter().any(|spec| spec.id == id) {
        return Some(id.to_string());
    }
    specs
        .into_iter()
        .find(|spec| spec.kind == kind && spec.auth_dir.as_deref() == Some(path))
        .map(|spec| spec.id)
}

/// A registry conflict at row `index`: the row says what it met (`conflict_with`) and suggests
/// the retry that works, `af provider detect --dir D --id OTHER --apply` with the first ID nothing
/// holds, or `af provider status` when the directory is registered under another ID meanwhile;
/// the message says how many later logins it left.
fn conflicted(
    rows: &mut [Row],
    index: usize,
    (id, conflict, taken): (String, String, Option<String>),
    registry: &Path,
    json: bool,
    home: Option<&Path>,
    registry_ids: &BTreeSet<String>,
) {
    let later = rows[index + 1..]
        .iter()
        .filter(|row| row.auth == Auth::Authenticated && row.candidate.registered_as.is_none())
        .count();
    let mut held: BTreeSet<String> = registry_ids.clone();
    held.extend(rows.iter().filter_map(|row| row.suggested.id.clone()));
    if let Ok(Some(text)) = read_registry(registry)
        && let Ok(specs) = parse_registry(&text, registry)
    {
        held.extend(specs.into_iter().map(|spec| spec.id));
    }
    held.insert(id.clone());
    let row = &mut rows[index];
    let path = row.candidate.path().to_path_buf();
    let retry = match &taken {
        Some(holder) if *holder != id => {
            row.suggested = Suggested {
                action: "none",
                id: None,
                command: Some("af provider status".to_string()),
                display: Some("af provider status".to_string()),
            };
            format!("it is registered as {holder} now; af provider status lists it")
        }
        Some(_) => {
            let other = unique_id(&id, &held);
            let retry = |spelled: String| {
                format!(
                    "af provider detect --dir {spelled} --id {} --apply",
                    shell_words::quote(&other)
                )
            };
            row.suggested = Suggested {
                action: "retry_apply",
                id: Some(other.clone()),
                command: Some(retry(quoted(&path))),
                display: Some(retry(tilde(&path, home))),
            };
            format!(
                "choose another ID: {}",
                row.suggested.display.as_deref().unwrap_or_default()
            )
        }
        None => {
            row.suggested = Suggested {
                action: "none",
                id: None,
                command: Some("af provider status".to_string()),
                display: Some("af provider status".to_string()),
            };
            "af provider status lists the registry".to_string()
        }
    };
    let line = format!(
        "af provider add {} --kind {} --auth-dir {}",
        shell_words::quote(&id),
        row.candidate.kind.name(),
        tilde(&path, home)
    );
    row.applied = Some(Apply {
        id,
        result: Applied::RegistryConflict,
        conflict_with: taken,
    });
    let message = format!(
        "registry conflict at {line}: {conflict}; stopped here, so {later} later login{} {} not \
         registered; {retry}",
        if later == 1 { "" } else { "s" },
        if later == 1 { "was" } else { "were" },
    );
    if !json {
        for line in wrap(&message, "") {
            println!("{line}");
        }
    }
    eprintln!("af provider detect: {message}");
}

/// What a whole `af/provider-detect@4` document says beside its rows.
struct Shape {
    code: i32,
    /// Whether `--apply` was given, whatever it found to do.
    applied: bool,
    /// Whether the registry could not be read, so no row's registration is known.
    registry_unknown: bool,
    /// The kinds scanned for whose CLI none was found: one `install` entry each.
    missing: Vec<ProviderKind>,
    /// The scan to run again once a CLI is installed, when none was found.
    next_action: Option<String>,
}

/// `af provider detect` with the scan's own `--kind`, `--dir`, `--id` and `--all`: what to run
/// again once a CLI is installed, so it looks where this scan looked.
fn rerun_command(options: &DetectOptions) -> String {
    let mut line = "af provider detect".to_string();
    if let Some(kind) = &options.kind {
        line.push_str(&format!(" --kind {}", shell_words::quote(kind)));
    }
    for (index, dir) in options.dirs.iter().enumerate() {
        line.push_str(&format!(" --dir {}", quoted(dir)));
        if let Some(id) = options.ids.get(index) {
            line.push_str(&format!(" --id {}", shell_words::quote(id)));
        }
    }
    if options.all {
        line.push_str(" --all");
    }
    line
}

/// The official install documentation of a kind, beside its install command.
fn install_docs(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "https://code.claude.com/docs/en/setup",
        ProviderKind::Codex => "https://developers.openai.com/codex/cli",
    }
}

/// One `af/provider-detect@4` document: closed states, af's own labels and paths, never an
/// email, an organization, a credential or the CLI's words. Next actions are typed fields
/// (`install`, `next_action`, each row's `suggested`); `notes` repeat them for a person.
fn document(
    shape: Shape,
    binaries: &[Binary],
    unsupported: &[Unsupported],
    rows: &[Row],
    notes: &[String],
) -> serde_json::Value {
    let binaries: Vec<_> = binaries
        .iter()
        .map(|binary| {
            serde_json::json!({
                "kind": binary.kind.name(),
                "path": binary.path.display().to_string(),
                "version": binary.version,
                "on_path": binary.on_path,
                "compatible": binary.compatible,
                "probe": binary.refusal.as_ref().map_or("compatible", |(status, _)| match status.as_str() {
                    PROBE_DENIED => "denied",
                    INCOMPATIBLE => "incompatible",
                    INSTALLATION_FAILED => "installation_failed",
                    _ => "unchecked",
                }),
            })
        })
        .collect();
    let unsupported: Vec<_> = unsupported
        .iter()
        .map(|found| {
            serde_json::json!({
                "name": found.name,
                "path": found.path.display().to_string(),
                "version": found.version,
            })
        })
        .collect();
    let candidates: Vec<_> = rows
        .iter()
        .map(|row| {
            let candidate = &row.candidate;
            serde_json::json!({
                "kind": candidate.kind.name(),
                "auth_dir": candidate.path().display().to_string(),
                "source": candidate.source.name(),
                "markers": candidate.markers,
                "safe": candidate.safety.is_ok(),
                "auth": row.auth.name(),
                "auth_cause": row.cause,
                "auth_detail": row.detail,
                "auth_method": row.auth_method,
                "plan": row.plan,
                "registration": match (&candidate.registered_as, shape.registry_unknown) {
                    (Some(_), _) => "registered",
                    (None, true) => "unknown",
                    (None, false) => "unregistered",
                },
                "registered_as": candidate.registered_as,
                "listing_unreadable": candidate.listing.is_some(),
                "suggested": {
                    "action": row.suggested.action,
                    "id": row.suggested.id,
                    "command": row.suggested.command,
                },
                "applied": row.applied.as_ref().map(|applied| serde_json::json!({
                    "id": applied.id,
                    "result": applied.result.name(),
                    "conflict_with": applied.conflict_with,
                })),
            })
        })
        .collect();
    let install: Vec<_> = shape
        .missing
        .iter()
        .map(|kind| {
            serde_json::json!({
                "kind": kind.name(),
                "command": install_commands(*kind)[0],
                "docs": install_docs(*kind),
            })
        })
        .collect();
    serde_json::json!({
        "schema": "af/provider-detect@4",
        "exit_code": shape.code,
        "applied": shape.applied,
        "binaries": binaries,
        "unsupported": unsupported,
        "candidates": candidates,
        "install": install,
        "next_action": shape.next_action,
        "notes": notes,
    })
}

/// Word-wrap prose at [`WIDTH`], continuation lines indented by `indent` plus two spaces.
pub(super) fn wrap(text: &str, indent: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = indent.to_string();
    let continuation = format!("{indent}  ");
    for word in text.split_whitespace() {
        let fresh = line.trim().is_empty();
        if !fresh && line.chars().count() + 1 + word.chars().count() > WIDTH {
            lines.push(std::mem::replace(&mut line, continuation.clone()));
        }
        if !line.trim().is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.trim().is_empty() {
        lines.push(line);
    }
    lines
}

/// `text` cut to `width` characters, a cut cell ending in `…`.
fn cell(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return format!("{text:<width$}");
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}\u{2026}")
}

/// A cut path keeps its distinguishing end.
fn path_cell(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return format!("{text:<width$}");
    }
    let kept: String = text.chars().skip(count - (width - 1)).collect();
    format!("\u{2026}{kept}")
}

fn table_line(columns: [&str; 6]) -> String {
    let [kind, dir, source, login, registered, suggested] = columns;
    format!(
        "{} {} {} {} {} {}",
        cell(kind, 6),
        path_cell(dir, 26),
        cell(source, 8),
        cell(login, 22),
        cell(registered, 16),
        cell(suggested, 16)
    )
    .trim_end()
    .to_string()
}

fn print_table(
    rows: &[Row],
    kinds: &[ProviderKind],
    identity: bool,
    registry_unknown: bool,
    home: Option<&Path>,
) {
    if rows.is_empty() {
        println!("No auth directory was found. Log in to a new one at a private terminal:");
        for kind in kinds {
            let kind = kind.name();
            println!("  af provider setup {kind}-main --kind {kind} --login");
        }
        return;
    }
    println!(
        "{}",
        table_line([
            "KIND",
            "AUTH DIR",
            "SOURCE",
            "LOGIN",
            "REGISTERED AS",
            "SUGGESTED"
        ])
    );
    for row in rows {
        let candidate = &row.candidate;
        let dir = tilde(candidate.path(), home);
        println!(
            "{}",
            table_line([
                candidate.kind.name(),
                &dir,
                candidate.source.name(),
                &row.login,
                candidate
                    .registered_as
                    .as_deref()
                    .unwrap_or(if registry_unknown { "unknown" } else { "-" }),
                row.suggested.id.as_deref().unwrap_or("-"),
            ])
        );
        if identity && let Some(account) = &row.account {
            println!("       account {account}");
        }
    }
    let notes: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            row.note
                .as_ref()
                .map(|note| format!("{}: {note}", tilde(row.candidate.path(), home)))
        })
        .collect();
    if !notes.is_empty() {
        println!();
        for note in notes {
            for line in wrap(&note, "") {
                println!("{line}");
            }
        }
    }
}

fn print_binaries(binaries: &[Binary], kinds: &[ProviderKind], home: Option<&Path>) {
    println!();
    let found: Vec<String> = binaries
        .iter()
        .map(|binary| {
            let version = binary.version.as_deref().unwrap_or("version unknown");
            let state = match binary.compatible {
                Some(true) => "",
                Some(false) => ", cannot serve af",
                None => ", unchecked",
            };
            format!("{} {version}{state}", binary.kind.name())
        })
        .collect();
    let line = if found.is_empty() {
        "binaries: none found".to_string()
    } else {
        format!("binaries: {}", found.join(", "))
    };
    for line in wrap(&line, "") {
        println!("{line}");
    }
    for binary in binaries {
        if binary.on_path {
            println!("  {} {}", binary.kind.name(), tilde(&binary.path, home));
        } else {
            println!(
                "  {} found off PATH: {}",
                binary.kind.name(),
                tilde(&binary.path, home)
            );
            println!("    fix: {}", path_fix(&binary.path, home));
        }
        if binary.compatible == Some(false) {
            println!("    fix: {}", upgrade_command(binary.kind));
        }
    }
    for kind in kinds.iter().copied() {
        if !binaries.is_empty() && !binaries.iter().any(|binary| binary.kind == kind) {
            let [first, second] = install_commands(kind);
            println!("  {} not found: {first}", kind.name());
            println!("    or: {second}");
        }
    }
}

fn print_unsupported(unsupported: &[Unsupported], home: Option<&Path>) {
    for found in unsupported {
        println!(
            "{} {} ({}): not supported by af yet",
            found.name,
            found.version.as_deref().unwrap_or("version unknown"),
            tilde(&found.path, home)
        );
    }
}

fn print_closing(rows: &[Row], applied: bool, code: i32, binaries: &[Binary], home: Option<&Path>) {
    let lines: Vec<&str> = rows
        .iter()
        .filter(|row| row.suggested.display.is_some())
        .filter(|row| !applied || !matches!(row.suggested.action, "register" | "login"))
        .map(|row| row.suggested.display.as_deref().unwrap_or_default())
        .collect();
    if !lines.is_empty() {
        println!();
        println!("next, one line per directory:");
        for line in &lines {
            println!("  {line}");
        }
        if rows.iter().any(|row| row.suggested.action == "login") && !applied {
            for line in wrap(
                "a setup ... --login line starts a login: run it yourself at a private terminal, \
                 never through an agent or a pipe",
                "",
            ) {
                println!("{line}");
            }
        }
    }
    println!();
    // The one next step follows the outcome: the retry after a conflict, the CLI's fix after
    // exit 4, `af provider status` only after an apply that succeeded.
    let conflict = rows
        .iter()
        .find(|row| {
            row.applied
                .as_ref()
                .is_some_and(|applied| applied.result == Applied::RegistryConflict)
        })
        .and_then(|row| row.suggested.display.clone());
    let cli_fix = binaries
        .iter()
        .find(|binary| binary.compatible == Some(false))
        .map(|binary| upgrade_command(binary.kind).to_string())
        .or_else(|| {
            binaries
                .iter()
                .find(|binary| !binary.on_path)
                .map(|binary| path_fix(&binary.path, home))
        });
    if code == EXIT_REGISTRY_CONFLICT
        && let Some(retry) = conflict
    {
        println!("next: {retry}");
    } else if code == EXIT_PROVIDER_CLI_MISSING
        && let Some(fix) = cli_fix
    {
        println!("next: {fix}");
    } else if code == EXIT_PROBE_DENIED {
        println!("next: retry outside the sandbox or with host-approved permissions");
    } else if applied {
        println!("next: af provider status");
    } else {
        let waiting = rows
            .iter()
            .filter(|row| row.suggested.action == "register")
            .count();
        // Worded from the authenticated rows: a directory that exists but is logged out is no
        // registered login (#394 finding 3).
        let authenticated = rows
            .iter()
            .filter(|row| row.auth == Auth::Authenticated)
            .count();
        let registered = rows
            .iter()
            .filter(|row| row.auth == Auth::Authenticated && row.candidate.registered_as.is_some())
            .count();
        let login = rows
            .iter()
            .find(|row| row.suggested.action == "login")
            .and_then(|row| row.suggested.id.as_deref());
        let summary = match waiting {
            0 if rows.is_empty() => "No login was found to register.".to_string(),
            0 if authenticated == 0 => match login {
                Some(id) => format!(
                    "No authenticated login was found. Log in privately: run the af provider \
                     setup {id} ... --login line above yourself at a private terminal, never \
                     through an agent, then run af provider detect again"
                ),
                None => "No authenticated login was found.".to_string(),
            },
            0 if registered == authenticated => {
                "every authenticated login found here is registered".to_string()
            }
            0 => format!(
                "{} of {authenticated} authenticated logins cannot be registered yet: fix what \
                 the lines above name, then run af provider detect again",
                authenticated - registered
            ),
            1 => "1 login is not registered: run af provider detect --apply to register it with \
                  the ID above, or copy a line"
                .to_string(),
            count => format!(
                "{count} logins are not registered: run af provider detect --apply to register \
                 them with the IDs above, or copy a line"
            ),
        };
        for line in wrap(&summary, "") {
            println!("{line}");
        }
    }
    for line in wrap(ALIAS_NOTE, "") {
        println!("{line}");
    }
}

fn guidance_notes(kinds: &[ProviderKind]) -> Vec<String> {
    let mut notes: Vec<String> = kinds
        .iter()
        .map(|kind| {
            let [first, second] = install_commands(*kind);
            format!(
                "no {} CLI was found on PATH or in the usual install directories; install it: \
                 {first} (or: {second}), then run: af provider detect",
                kind.name()
            )
        })
        .collect();
    notes.push(ALIAS_NOTE.to_string());
    notes
}

/// What a machine with no supported CLI is told: the official install commands, where af
/// looked, and the command to run next. `af provider status` prints the same lines.
pub(super) fn install_guidance() -> Vec<String> {
    install_guidance_for(
        &[ProviderKind::Claude, ProviderKind::Codex],
        home_dir().as_deref(),
    )
}

fn install_guidance_for(kinds: &[ProviderKind], home: Option<&Path>) -> Vec<String> {
    let mut lines = Vec::new();
    let off_path: Vec<(ProviderKind, PathBuf)> = kinds
        .iter()
        .filter_map(|kind| locate_binary(*kind, home).map(|(path, _)| (*kind, path)))
        .collect();
    let missing: Vec<ProviderKind> = kinds
        .iter()
        .copied()
        .filter(|kind| !off_path.iter().any(|(found, _)| found == kind))
        .collect();
    for (kind, path) in &off_path {
        lines.push(format!(
            "{} found off PATH: {}",
            kind.name(),
            path.display()
        ));
        lines.push(format!("  fix: {}", path_fix(path, home)));
    }
    if !missing.is_empty() {
        lines.push(
            "No supported Provider CLI was found (a Provider is a login of the official claude or \
             codex CLI; see af help providers). Install one:"
                .to_string(),
        );
        for kind in &missing {
            let [first, second] = install_commands(*kind);
            lines.push(format!("  {:<7} {first}", kind.name()));
            lines.push(format!("  {:<7} or: {second}", ""));
        }
    }
    lines.push("Searched the directories on PATH:".to_string());
    let path = sanitized_path();
    let on_path: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if on_path.is_empty() {
        lines.push("  (PATH names no directory)".to_string());
    }
    for dir in on_path {
        lines.push(format!("  {}", tilde(&dir, home)));
    }
    lines.push("and the usual install directories:".to_string());
    let mut listed = Vec::new();
    for kind in kinds {
        for dir in install_dirs(*kind, home) {
            if !listed.contains(&dir) {
                listed.push(dir);
            }
        }
    }
    for dir in listed {
        lines.push(format!("  {}", tilde(&dir, home)));
    }
    lines.push("then run: af provider detect".to_string());
    lines
        .into_iter()
        .flat_map(|line| {
            if line.chars().count() > WIDTH {
                wrap(&line, "")
            } else {
                vec![line]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn found(kind: ProviderKind, path: &Path, source: Source) -> Candidate {
        candidate(kind, path.to_path_buf(), source, None)
    }

    fn row(candidate: Candidate, auth: Auth) -> Row {
        Row {
            candidate,
            auth,
            login: String::new(),
            auth_method: "unknown",
            plan: "unknown",
            account: None,
            note: None,
            cause: None,
            detail: None,
            suggested: Suggested::none(),
            applied: None,
        }
    }

    /// C3 item 9: a row the scan read as registered is checked again under the registry lock.
    /// An entry removed since is registered again for a logged-in row, and never reported as
    /// still there for one that is not; a present entry is `already_registered`.
    #[test]
    fn apply_checks_a_registered_row_again_under_the_registry_lock() {
        let root = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(root.path()).unwrap();
        let dir = home.join("claude-work");
        private(&dir);
        fs::write(dir.join(".claude.json"), "{}").unwrap();
        let config = home.join("config");
        private(&config);
        let registry = config.join("providers.toml");
        let scanned = || {
            let mut candidate = found(ProviderKind::Claude, &dir, Source::Registry);
            candidate.registered_as = Some("claude-work".into());
            candidate
        };
        // The entry the scan read is gone: the logged-in row is registered again.
        let mut rows = vec![row(scanned(), Auth::Authenticated)];
        let end = apply(&mut rows, &registry, true, Some(&home), &BTreeSet::new()).unwrap();
        assert!(!end.conflict && !end.off_path);
        let applied = rows[0].applied.as_ref().unwrap();
        assert_eq!(applied.id, "claude-work");
        assert_eq!(applied.result, Applied::Registered);
        // It is there now: already registered, as the locked read found it.
        let mut rows = vec![row(scanned(), Auth::Authenticated)];
        apply(&mut rows, &registry, true, Some(&home), &BTreeSet::new()).unwrap();
        assert_eq!(
            rows[0].applied.as_ref().unwrap().result,
            Applied::AlreadyRegistered
        );
        let mut rows = vec![row(scanned(), Auth::NotAuthenticated)];
        apply(&mut rows, &registry, true, Some(&home), &BTreeSet::new()).unwrap();
        assert_eq!(
            rows[0].applied.as_ref().unwrap().result,
            Applied::AlreadyRegistered
        );
        // Removed again: a logged-out row is no longer called registered.
        fs::write(&registry, "version = 1\n").unwrap();
        let mut rows = vec![row(scanned(), Auth::NotAuthenticated)];
        apply(&mut rows, &registry, true, Some(&home), &BTreeSet::new()).unwrap();
        assert!(rows[0].applied.is_none());
        assert_eq!(rows[0].candidate.registered_as, None);
    }

    /// C3 item 17: with no scratch directory, each CLI is checked with its own kind's spec.
    #[test]
    fn a_cli_without_a_scratch_directory_is_checked_as_its_own_kind() {
        for kind in [ProviderKind::Claude, ProviderKind::Codex] {
            let spec = capability_spec(kind, None);
            assert_eq!(spec.kind, kind);
            assert_eq!(spec.auth_dir, None);
            let dir = Path::new("/scratch");
            assert_eq!(
                capability_spec(kind, Some(dir)).auth_dir.as_deref(),
                Some(dir)
            );
        }
    }

    /// C3 item 8: a directory that cannot be listed has its markers checked by name, and says so.
    #[test]
    fn an_unlistable_directory_has_its_markers_checked_by_name() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("claude-work");
        private(&dir);
        fs::write(dir.join(".claude.json"), "{}").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o100)).unwrap();
        let (markers, listing) = markers_found(&dir, CLAUDE_MARKERS);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(markers, [".claude.json"]);
        assert!(listing.unwrap().starts_with("listing unreadable"));
        let (markers, listing) = markers_found(&dir, CLAUDE_MARKERS);
        assert_eq!((markers, listing), (vec![".claude.json"], None));
    }

    #[test]
    fn markers_are_read_by_name_without_opening_a_file_below_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("claude-work");
        private(&dir);
        assert!(markers_in(&dir, CLAUDE_MARKERS).unwrap().is_empty());
        fs::write(dir.join("settings.json"), "{}").unwrap();
        // A marker af may not read is still a marker: only its name is examined.
        fs::write(dir.join(".credentials.json"), "").unwrap();
        fs::set_permissions(
            dir.join(".credentials.json"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        std::os::unix::fs::symlink("/nonexistent", dir.join(".claude.json")).unwrap();
        assert_eq!(
            markers_in(&dir, CLAUDE_MARKERS).unwrap(),
            [".claude.json", ".credentials.json", "settings.json"]
        );
        // A directory named like a marker is not one.
        let codex = root.path().join("codex");
        private(&codex.join("auth.json"));
        assert!(markers_in(&codex, CODEX_MARKERS).unwrap().is_empty());
        // The candidate itself is never followed through a link.
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(markers_in(&link, CLAUDE_MARKERS).is_err());
    }

    #[test]
    fn ids_follow_the_source_and_the_directory_name() {
        let root = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(root.path()).unwrap();
        for dir in [
            ".claude",
            ".claude-personal",
            ".config/claude-work",
            ".config/claude",
            ".codex-gridgain",
            ".config/codex-Acme Corp",
            "elsewhere/team",
            "elsewhere/.claude",
        ] {
            private(&home.join(dir));
        }
        let id = |kind, dir: &str, source| {
            derived_id(&found(kind, &home.join(dir), source), Some(&home))
        };
        use ProviderKind::{Claude, Codex};
        assert_eq!(id(Claude, ".claude", Source::Default), "claude-main");
        assert_eq!(
            id(Claude, ".claude-personal", Source::Sibling),
            "claude-personal"
        );
        assert_eq!(
            id(Claude, ".config/claude-work", Source::Sibling),
            "claude-work"
        );
        assert_eq!(
            id(Claude, ".config/claude", Source::Sibling),
            "claude-config"
        );
        assert_eq!(
            id(Codex, ".codex-gridgain", Source::Sibling),
            "codex-gridgain"
        );
        assert_eq!(
            id(Codex, ".config/codex-Acme Corp", Source::Sibling),
            "codex-Acme-Corp"
        );
        // The selector variable's directory: the sibling rule when its name follows a sibling
        // pattern, else `<kind>-env`, and `-main` when it is the default directory.
        assert_eq!(
            id(Claude, ".config/claude-work", Source::Env),
            "claude-work"
        );
        assert_eq!(id(Claude, "elsewhere/team", Source::Env), "claude-env");
        assert_eq!(id(Claude, ".claude", Source::Env), "claude-main");
        // `--dir`: the sibling rule on its name.
        assert_eq!(
            id(Claude, "elsewhere/team", Source::Explicit),
            "claude-team"
        );
        assert_eq!(
            id(Claude, "elsewhere/.claude", Source::Explicit),
            "claude-main"
        );

        let mut taken: BTreeSet<String> = ["claude-work", "claude-work-2"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(unique_id("claude-work", &taken), "claude-work-3");
        taken.insert("claude-ambient".into());
        assert_eq!(unique_id("claude-ambient", &taken), "claude-ambient-2");
        assert_eq!(
            unique_id("claude-ambient", &BTreeSet::new()),
            "claude-ambient-2"
        );
    }

    #[test]
    fn the_scan_lists_literal_sibling_names_one_level_deep() {
        let root = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(root.path()).unwrap();
        for dir in [
            ".claude",
            ".claude-work",
            ".claude-",
            ".config/claude",
            ".config/claude-x",
            ".config/claude-x/.claude-nested",
            ".config/claudette",
            ".codex-acme",
            ".config/codex",
        ] {
            private(&home.join(dir));
        }
        fs::write(home.join(".claude.json"), "{}").unwrap();
        fs::write(home.join(".claude-file"), "").unwrap();
        let listed = |kind| -> Vec<(String, Source)> {
            home_dirs(kind, &home)
                .into_iter()
                .map(|(path, source)| {
                    (
                        path.strip_prefix(&home).unwrap().display().to_string(),
                        source,
                    )
                })
                .collect()
        };
        assert_eq!(
            listed(ProviderKind::Claude),
            [
                (".claude".to_string(), Source::Default),
                (".claude-work".to_string(), Source::Sibling),
                (".config/claude".to_string(), Source::Sibling),
                (".config/claude-x".to_string(), Source::Sibling),
            ]
        );
        assert_eq!(
            listed(ProviderKind::Codex),
            [
                (".codex-acme".to_string(), Source::Sibling),
                (".config/codex".to_string(), Source::Sibling),
            ]
        );
    }

    #[test]
    fn a_directory_found_twice_keeps_its_most_specific_source() {
        let root = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(root.path()).unwrap().join("claude-work");
        private(&dir);
        let kept = deduplicate(vec![
            found(ProviderKind::Claude, &dir, Source::Sibling),
            found(ProviderKind::Claude, &dir, Source::Registry),
            found(ProviderKind::Claude, &dir, Source::Env),
            found(ProviderKind::Codex, &dir, Source::Sibling),
        ]);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].source, Source::Env);
        assert_eq!(kept[1].kind, ProviderKind::Codex);
    }

    #[test]
    fn unsafe_and_absent_candidates_say_why() {
        let root = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(root.path()).unwrap();
        let shared = base.join("shared");
        private(&shared);
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        let error = found(ProviderKind::Codex, &shared, Source::Sibling)
            .safety
            .unwrap_err();
        assert!(error.contains("writable by another user"), "{error}");
        let link = base.join("link");
        std::os::unix::fs::symlink(&shared, &link).unwrap();
        let error = found(ProviderKind::Codex, &link, Source::Sibling)
            .safety
            .unwrap_err();
        assert!(error.contains("is a symlink"), "{error}");
        let absent = found(ProviderKind::Codex, &base.join("absent"), Source::Explicit);
        assert!(absent.canonical.is_none());
        assert!(absent.safety.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn versions_are_af_s_own_reading_of_the_cli_line() {
        assert_eq!(
            version_token("2.1.299 (Claude Code)").as_deref(),
            Some("2.1.299")
        );
        assert_eq!(
            version_token("codex-cli 0.162.1").as_deref(),
            Some("0.162.1")
        );
        assert_eq!(version_token("aider v0.86.1").as_deref(), Some("0.86.1"));
        assert_eq!(
            version_token("gemini 0.9.0-preview.1").as_deref(),
            Some("0.9.0-preview.1")
        );
        assert_eq!(version_token("version unknown"), None);
        assert_eq!(version_token("token sk-ant-api03 1"), None);
    }

    #[test]
    fn the_path_fix_names_the_directory_for_the_operator_s_shell() {
        let home = Path::new("/home/someone");
        let dir = Path::new("/home/someone/.local/bin");
        assert_eq!(
            path_fix_for(dir, Some(home), false),
            "export PATH=\"$HOME/.local/bin:$PATH\""
        );
        assert_eq!(
            path_fix_for(dir, Some(home), true),
            "fish_add_path ~/.local/bin"
        );
        assert_eq!(
            path_fix_for(Path::new("/opt/homebrew/bin"), Some(home), false),
            "export PATH=\"/opt/homebrew/bin:$PATH\""
        );
        assert_eq!(
            tilde(Path::new("/home/someone/My Tools"), Some(home)),
            "'/home/someone/My Tools'"
        );
    }

    /// #331: a canonical directory below a HOME reached through a symbolic link reads `~/…`,
    /// as it does below a canonical HOME; one outside HOME is printed whole.
    #[test]
    fn a_directory_below_a_linked_home_reads_as_below_home() {
        let root = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(root.path()).unwrap().join("real-home");
        fs::create_dir_all(real.join(".claude")).unwrap();
        let linked = root.path().join("linked-home");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let found = real.join(".claude");
        for home in [&real, &linked] {
            assert_eq!(display_below(&found, Some(home)), "~/.claude", "{home:?}");
        }
        let outside = Path::new("/opt/elsewhere/.claude");
        assert_eq!(
            display_below(outside, Some(&linked)),
            "/opt/elsewhere/.claude"
        );
        assert_eq!(display_below(&found, None), found.to_str().unwrap());
    }

    #[test]
    fn prose_wraps_below_a_hundred_columns_and_commands_do_not() {
        let text = "2 logins are not registered: run af provider detect --apply to register them \
                    with the IDs above, or copy a line";
        let lines = wrap(text, "");
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.chars().count() < 100));
        assert_eq!(
            lines.join(" ").split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
        let row = table_line([
            "claude",
            "~/.config/a-very-long-directory-name-indeed",
            "sibling",
            "Claude Enterprise (claude.ai)",
            "claude-a-long-registered-id",
            "claude-a-long-suggested-id",
        ]);
        assert!(row.chars().count() < 100, "{row}");
        assert!(
            row.contains("directory-name-indeed ") && row.contains("\u{2026}"),
            "{row}"
        );
    }

    #[test]
    fn the_status_marker_gate_names_what_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(root.path()).unwrap().join("codex-new");
        let spec = probe_spec(ProviderKind::Codex, &dir);
        assert_eq!(
            unmarked_context(&spec).unwrap(),
            format!("{} does not exist", dir.display())
        );
        private(&dir);
        assert_eq!(
            unmarked_context(&spec).unwrap(),
            format!("{} holds no auth.json or config.toml", dir.display())
        );
        fs::write(dir.join("config.toml"), "").unwrap();
        assert_eq!(unmarked_context(&spec), None);
    }

    /// GA final fix item 1: `~/.claude` with no marker inside is the default context when a
    /// regular `~/.claude.json` sits beside it, for both kinds of probe, and only then.
    #[test]
    fn the_default_claude_context_is_one_predicate() {
        let root = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(root.path()).unwrap();
        let default = home.join(".claude");
        private(&default);
        let other = home.join(".claude-work");
        private(&other);
        assert!(!default_claude_context(
            ProviderKind::Claude,
            &default,
            Some(&home)
        ));
        fs::create_dir(home.join(".claude.json")).unwrap();
        assert!(
            !default_claude_context(ProviderKind::Claude, &default, Some(&home)),
            "a directory named .claude.json is no marker"
        );
        fs::remove_dir(home.join(".claude.json")).unwrap();
        fs::write(home.join(".claude.json"), "{}").unwrap();
        assert!(default_claude_context(
            ProviderKind::Claude,
            &default,
            Some(&home)
        ));
        assert!(!default_claude_context(
            ProviderKind::Claude,
            &other,
            Some(&home)
        ));
        assert!(!default_claude_context(
            ProviderKind::Codex,
            &default,
            Some(&home)
        ));
        assert!(!default_claude_context(
            ProviderKind::Claude,
            &default,
            None
        ));
    }
}
