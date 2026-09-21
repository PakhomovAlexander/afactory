//! `.af/af.lock`: a reviewer pinned by content digest, never `latest`; since v0.8.0 also the
//! `af` release that wrote the lock, pinned by its archive digests.
//!
//! A reviewer is a package — a directory holding `reviewer.toml` (name, version, the runner
//! command) and whatever prompt or support files it needs. Packages live in registries searched
//! in order (project, then user, then system), and the lockfile pins each reviewer the pipeline
//! may use to an exact version *and* a content digest over every byte in the package.
//!
//! Three rules, each the refusal of a specific failure:
//!
//! 1. **Not locked, not run.** A reviewer absent from the lockfile does not resolve, whatever
//!    the registries contain. There is no "use whatever is installed" path, because that path
//!    is `latest` wearing a different name.
//! 2. **The digest decides, and there is no fall-through.** Registry search stops at the first
//!    root that *has* the name; if that copy's digest does not match the pin, resolution fails
//!    loudly. Falling through to a later root that happens to match would let a tampered copy
//!    earlier in the chain hide behind a clean one — the operator must learn the project's copy
//!    changed, not silently review with a different one.
//! 3. **Verify before interpreting.** The package is read once into memory, the digest is
//!    checked over those bytes, and only then is `reviewer.toml` parsed — from the verified
//!    bytes, not from a second read the filesystem could have changed in between.
//!
//! Versions are exact (`major.minor.patch`, digits only). `latest`, ranges, and wildcards are
//! refused at parse time so a floating pin cannot even be *written*, let alone resolved.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::CommandSpec;

#[derive(Debug)]
pub enum LockError {
    Parse(String),
    InvalidName {
        name: String,
    },
    /// A version that does not name exactly one release.
    Floating {
        name: String,
        version: String,
    },
    MalformedDigest {
        name: String,
        digest: String,
    },
    /// The refusal that keeps `latest` out: an unlocked reviewer never runs.
    NotLocked {
        name: String,
    },
    NotFound {
        name: String,
        searched: Vec<PathBuf>,
    },
    DigestMismatch {
        name: String,
        root: PathBuf,
        locked: String,
        found: String,
    },
    VersionMismatch {
        name: String,
        locked: String,
        manifest: String,
    },
    NameMismatch {
        requested: String,
        manifest: String,
    },
    /// A symlink can point outside the package, so its target would be content the digest
    /// silently depends on. Refused rather than followed or skipped.
    Symlink {
        name: String,
        path: PathBuf,
    },
    UnsupportedFileType {
        name: String,
        path: PathBuf,
    },
    UnsupportedPath {
        name: String,
        path: PathBuf,
    },
    UnsupportedSubject {
        name: String,
        subject: review_core::SubjectKind,
    },
    MissingManifest {
        name: String,
        root: PathBuf,
    },
    Io {
        path: PathBuf,
        error: String,
    },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Parse(e) => write!(f, "lockfile: {e}"),
            LockError::InvalidName { name } => {
                write!(f, "reviewer name `{name}` is not a safe registry component")
            }
            LockError::Floating { name, version } => write!(
                f,
                "reviewer `{name}` pins version `{version}`, which does not name exactly one \
                 release; a pin must be `major.minor.patch` — a run never resolves `latest`"
            ),
            LockError::MalformedDigest { name, digest } => write!(
                f,
                "reviewer `{name}` pins digest `{digest}`, which is not `sha256:` plus 64 hex \
                 digits; a malformed digest pins nothing"
            ),
            LockError::NotLocked { name } => write!(
                f,
                "reviewer `{name}` is not in the lockfile; an unlocked reviewer does not run"
            ),
            LockError::NotFound { name, searched } => write!(
                f,
                "reviewer `{name}` is locked but present in no registry (searched {})",
                searched
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            LockError::DigestMismatch {
                name,
                root,
                locked,
                found,
            } => write!(
                f,
                "reviewer `{name}` at {} does not match its pin: locked {locked}, found {found}; \
                 the package changed since it was locked",
                root.display()
            ),
            LockError::VersionMismatch {
                name,
                locked,
                manifest,
            } => write!(
                f,
                "reviewer `{name}` manifest says version {manifest} but the lock pins {locked}"
            ),
            LockError::NameMismatch {
                requested,
                manifest,
            } => write!(
                f,
                "package resolved for `{requested}` declares itself `{manifest}`"
            ),
            LockError::Symlink { name, path } => write!(
                f,
                "reviewer `{name}` contains a symlink at {}; a package is regular files only",
                path.display()
            ),
            LockError::UnsupportedFileType { name, path } => write!(
                f,
                "reviewer `{name}` contains a non-regular file at {}; a package is regular files only",
                path.display()
            ),
            LockError::UnsupportedPath { name, path } => write!(
                f,
                "reviewer `{name}` contains a path that is not valid UTF-8 at {}; refusing a lossy digest name",
                path.display()
            ),
            LockError::UnsupportedSubject { name, subject } => {
                write!(f, "reviewer `{name}` does not accept `{subject}` subjects")
            }
            LockError::MissingManifest { name, root } => write!(
                f,
                "reviewer `{name}` at {} has no reviewer.toml",
                root.display()
            ),
            LockError::Io { path, error } => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for LockError {}

/// `reviewer.toml` at a package root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    /// Packages predating Subject capabilities reviewed whole trees. Preserving that exact
    /// capability keeps them readable without letting them silently claim diff support.
    #[serde(default = "legacy_subjects")]
    pub subjects: Vec<review_core::SubjectKind>,
    pub runner: CommandSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerBackend {
    Codex,
    Claude,
}

impl std::fmt::Display for ReviewerBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Codex => write!(f, "codex"),
            Self::Claude => write!(f, "claude"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerRunnerSettings {
    pub backend: ReviewerBackend,
    pub model: String,
    pub effort: String,
}

/// Read the model settings from a typed package manifest. Only the two adapter flag shapes the
/// kernel owns are recognized; ambiguity is refused rather than guessed around.
pub fn reviewer_runner_settings_from_manifest(
    manifest: &PackageManifest,
) -> Result<ReviewerRunnerSettings, LockError> {
    let basename = Path::new(&manifest.runner.program)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| LockError::Parse("reviewer runner program has no basename".into()))?;
    let backend = match basename {
        "codex" => ReviewerBackend::Codex,
        "claude" => ReviewerBackend::Claude,
        other => {
            return Err(LockError::Parse(format!(
                "reviewer runner `{other}` has no configurable model profile"
            )));
        }
    };
    let model_index = unique_option_value(&manifest.runner.args, "--model")?;
    let effort_index = match backend {
        ReviewerBackend::Claude => unique_option_value(&manifest.runner.args, "--effort")?,
        ReviewerBackend::Codex => unique_codex_effort(&manifest.runner.args)?,
    };
    let effort = match backend {
        ReviewerBackend::Claude => manifest.runner.args[effort_index].value.clone(),
        ReviewerBackend::Codex => manifest.runner.args[effort_index]
            .value
            .strip_prefix("model_reasoning_effort=")
            .expect("the index was selected by this prefix")
            .trim_matches(['\'', '"'])
            .to_string(),
    };
    Ok(ReviewerRunnerSettings {
        backend,
        model: manifest.runner.args[model_index].value.clone(),
        effort,
    })
}

fn unique_option_value(args: &[crate::ArgSpec], option: &str) -> Result<usize, LockError> {
    let matches: Vec<usize> = args
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument.value == option).then_some(index + 1))
        .collect();
    let [index] = matches.as_slice() else {
        return Err(LockError::Parse(format!(
            "reviewer runner must contain exactly one `{option}` option"
        )));
    };
    if *index >= args.len() {
        return Err(LockError::Parse(format!(
            "reviewer runner `{option}` has no value"
        )));
    }
    Ok(*index)
}

fn unique_codex_effort(args: &[crate::ArgSpec]) -> Result<usize, LockError> {
    let matches: Vec<usize> = args
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            (pair[0].value == "-c" && pair[1].value.starts_with("model_reasoning_effort="))
                .then_some(index + 1)
        })
        .collect();
    let [index] = matches.as_slice() else {
        return Err(LockError::Parse(
            "Codex runner must contain exactly one model_reasoning_effort override".into(),
        ));
    };
    Ok(*index)
}

fn legacy_subjects() -> Vec<review_core::SubjectKind> {
    vec![review_core::SubjectKind::WholeTree]
}

/// One pinned reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub version: String,
    pub digest: String,
}

/// The `af` release a lock pins, and the bytes that release has: one archive digest per target,
/// copied from the release's signed `SHA256SUMS` when the lock was written.
///
/// The version alone would pin a *name*; the digests pin the *bytes*. Dispatch installs a pinned
/// release on demand only when the archive it downloads matches the digest recorded here for the
/// running machine's target, so a re-uploaded or substituted asset never runs under a lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AfPin {
    pub version: String,
    /// `<target triple>` → `sha256:<hex>` of `af-v<version>-<target>.tar.gz`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub digests: BTreeMap<String, String>,
}

impl AfPin {
    pub fn version_only(version: &str) -> AfPin {
        AfPin {
            version: version.to_string(),
            digests: BTreeMap::new(),
        }
    }

    pub fn digest_for(&self, target: &str) -> Option<&str> {
        self.digests.get(target).map(String::as_str)
    }
}

/// The lockfile itself — `.af/af.lock`, TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
    pub version: u32,
    /// The pin as release 0.7.1 wrote it: the version only. Read for compatibility, normalised
    /// into `af` on parse, never written again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    af_version: Option<String>,
    /// The `af` release that wrote this lock (`af onboard --apply`, `--migrate --apply`, or
    /// `--refresh-lock`) and its per-target archive digests. A newer `af` proceeds and notes the
    /// difference; an older `af` refuses, because it cannot know what the newer release meant by
    /// these pins; inside the project, any `af` on `PATH` dispatches to the pinned release.
    /// Absent on locks written before the pin existed, and on locks written by a build that has
    /// no install receipt (a source build pins nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub af: Option<AfPin>,
    #[serde(default)]
    pub reviewers: BTreeMap<String, Pin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workers: BTreeMap<String, Pin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pipelines: BTreeMap<String, Pin>,
}

/// A reviewer that survived resolution. The type itself lives with the adapters
/// (`review-runner`), which consume it; this module is the resolver that mints it.
pub use review_runner::ResolvedReviewer;

/// Registries searched in order. Typically project, user, system.
pub struct Registry {
    roots: Vec<PathBuf>,
    captured: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

impl Registry {
    pub fn new(roots: impl IntoIterator<Item = impl Into<PathBuf>>) -> Registry {
        Registry {
            roots: roots.into_iter().map(Into::into).collect(),
            captured: BTreeMap::new(),
        }
    }

    /// A registry reconstructed from a Campaign's immutable package artifacts.
    pub fn captured(packages: BTreeMap<String, BTreeMap<String, Vec<u8>>>) -> Registry {
        Registry {
            roots: Vec::new(),
            captured: packages,
        }
    }

    fn read(&self, name: &str) -> Result<(PathBuf, BTreeMap<String, Vec<u8>>), LockError> {
        if let Some(files) = self.captured.get(name) {
            return Ok((
                PathBuf::from("<captured-authority>").join(name),
                files.clone(),
            ));
        }
        let root = self.locate(name)?;
        let files = collect(name, &root)?;
        Ok((root, files))
    }

    /// The first root that has the package directory. Search stops here: whether that copy
    /// verifies is the next question, and a failure there must not be papered over by a copy
    /// further down the chain.
    fn locate(&self, name: &str) -> Result<PathBuf, LockError> {
        if name.is_empty()
            || !name.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
            })
        {
            return Err(LockError::InvalidName {
                name: name.to_string(),
            });
        }
        for root in &self.roots {
            let candidate = root.join(name);
            match std::fs::symlink_metadata(&candidate) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(LockError::Symlink {
                        name: name.to_string(),
                        path: candidate,
                    });
                }
                Ok(metadata) if metadata.is_dir() => return Ok(candidate),
                Ok(_) => {
                    return Err(LockError::UnsupportedFileType {
                        name: name.to_string(),
                        path: candidate,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(LockError::Io {
                        path: candidate,
                        error: error.to_string(),
                    });
                }
            }
        }
        Err(LockError::NotFound {
            name: name.to_string(),
            searched: self.roots.clone(),
        })
    }
}

/// Exactly one release: `major.minor.patch`, digits only.
fn exact_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn well_formed_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn validate_pin(name: &str, pin: &Pin) -> Result<(), LockError> {
    if !exact_version(&pin.version) {
        return Err(LockError::Floating {
            name: name.to_string(),
            version: pin.version.clone(),
        });
    }
    if !well_formed_digest(&pin.digest) {
        return Err(LockError::MalformedDigest {
            name: name.to_string(),
            digest: pin.digest.clone(),
        });
    }
    Ok(())
}

/// Read every regular file in the package into memory, keyed by `/`-separated relative path.
///
/// One read serves both the digest and the manifest parse, so what was verified is what is
/// interpreted — a second read from disk could race with a writer.
fn collect(name: &str, root: &Path) -> Result<BTreeMap<String, Vec<u8>>, LockError> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    let io = |path: &Path, error: std::io::Error| LockError::Io {
        path: path.to_path_buf(),
        error: error.to_string(),
    };

    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| io(&dir, e))? {
            let entry = entry.map_err(|e| io(&dir, e))?;
            let path = entry.path();
            let kind = std::fs::symlink_metadata(&path).map_err(|e| io(&path, e))?;
            if kind.file_type().is_symlink() {
                return Err(LockError::Symlink {
                    name: name.to_string(),
                    path,
                });
            }
            if kind.is_dir() {
                pending.push(path);
            } else {
                if !kind.is_file() {
                    return Err(LockError::UnsupportedFileType {
                        name: name.to_string(),
                        path,
                    });
                }
                let relative_path = path
                    .strip_prefix(root)
                    .expect("walked paths live under the root");
                let mut components = Vec::new();
                for component in relative_path.components() {
                    let component = component.as_os_str().to_str().ok_or_else(|| {
                        LockError::UnsupportedPath {
                            name: name.to_string(),
                            path: path.clone(),
                        }
                    })?;
                    components.push(component);
                }
                let relative = components.join("/");
                let bytes = std::fs::read(&path).map_err(|e| io(&path, e))?;
                files.insert(relative, bytes);
            }
        }
    }
    Ok(files)
}

pub fn package_digest_from_files(files: &BTreeMap<String, Vec<u8>>) -> String {
    let listed: BTreeMap<&str, String> = files
        .iter()
        .map(|(path, bytes)| {
            (
                path.as_str(),
                review_store::canonical::blob_content_id(bytes),
            )
        })
        .collect();
    let payload = serde_json::json!({ "kind": "reviewer-package@1", "files": listed });
    review_store::canonical::content_id(&payload).expect("digest map is admissible JSON")
}

/// The content digest of a package on disk — what a pin records.
pub fn package_digest(name: &str, root: &Path) -> Result<String, LockError> {
    Ok(package_digest_from_files(&collect(name, root)?))
}

/// Only the `af` pin, read leniently: dispatch must find the pin in any lock a *newer* release
/// wrote, so it ignores every field it does not know instead of refusing the whole file.
pub fn pinned_af(text: &str) -> Option<AfPin> {
    let value: toml::Value = toml::from_str(text).ok()?;
    let table = value.as_table()?;
    if let Some(af) = table.get("af").and_then(toml::Value::as_table) {
        let version = af.get("version")?.as_str()?.to_string();
        let digests = af
            .get("digests")
            .and_then(toml::Value::as_table)
            .map(|digests| {
                digests
                    .iter()
                    .filter_map(|(target, digest)| {
                        digest
                            .as_str()
                            .map(|digest| (target.clone(), digest.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Some(AfPin { version, digests });
    }
    table
        .get("af_version")
        .and_then(toml::Value::as_str)
        .map(AfPin::version_only)
}

fn validate_af_pin(pin: &AfPin) -> Result<(), LockError> {
    if semver::Version::parse(&pin.version).is_err() {
        return Err(LockError::Parse(format!(
            "af pin `{}` is not a semantic version",
            pin.version
        )));
    }
    for (target, digest) in &pin.digests {
        if target.is_empty()
            || !target
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(LockError::Parse(format!(
                "af pin digest target `{target}` is not a target triple"
            )));
        }
        if !well_formed_digest(digest) {
            return Err(LockError::MalformedDigest {
                name: format!("af ({target})"),
                digest: digest.clone(),
            });
        }
    }
    Ok(())
}

impl Lockfile {
    pub fn empty() -> Lockfile {
        Lockfile {
            version: 1,
            af_version: None,
            af: None,
            reviewers: BTreeMap::new(),
            workers: BTreeMap::new(),
            pipelines: BTreeMap::new(),
        }
    }

    /// The pinned `af` version, whichever shape wrote it.
    pub fn af_version(&self) -> Option<&str> {
        self.af.as_ref().map(|pin| pin.version.as_str())
    }

    /// Parse and validate. A floating version or malformed digest is refused *here*, so a bad
    /// pin cannot sit latent in a file that parses.
    pub fn from_toml(text: &str) -> Result<Lockfile, LockError> {
        let mut lockfile: Lockfile =
            toml::from_str(text).map_err(|e| LockError::Parse(e.to_string()))?;
        if lockfile.version != 1 {
            return Err(LockError::Parse(format!(
                "unsupported lockfile version {}; this kernel understands version 1",
                lockfile.version
            )));
        }
        match (lockfile.af_version.take(), &lockfile.af) {
            (Some(_), Some(_)) => {
                return Err(LockError::Parse(
                    "lock carries both `af_version` and `[af]`; a pin has one home".into(),
                ));
            }
            (Some(version), None) => lockfile.af = Some(AfPin::version_only(&version)),
            (None, _) => {}
        }
        if let Some(pin) = &lockfile.af {
            validate_af_pin(pin)?;
        }
        for pins in [&lockfile.reviewers, &lockfile.workers, &lockfile.pipelines] {
            for (name, pin) in pins {
                validate_pin(name, pin)?;
            }
        }
        Ok(lockfile)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("a lockfile serializes")
    }

    /// Resolve one locked reviewer: locate, verify the digest, then read the manifest from the
    /// verified bytes.
    fn resolve_package(
        &self,
        name: &str,
        registry: &Registry,
    ) -> Result<(ResolvedReviewer, Vec<review_core::SubjectKind>), LockError> {
        let pin = self
            .reviewers
            .get(name)
            .or_else(|| self.workers.get(name))
            .ok_or_else(|| LockError::NotLocked {
                name: name.to_string(),
            })?;
        validate_pin(name, pin)?;
        let (root, files) = registry.read(name)?;

        let found = package_digest_from_files(&files);
        if found != pin.digest {
            return Err(LockError::DigestMismatch {
                name: name.to_string(),
                root,
                locked: pin.digest.clone(),
                found,
            });
        }

        let manifest = Self::manifest(name, &root, &files)?;
        if manifest.name != name {
            return Err(LockError::NameMismatch {
                requested: name.to_string(),
                manifest: manifest.name,
            });
        }
        if manifest.version != pin.version {
            return Err(LockError::VersionMismatch {
                name: name.to_string(),
                locked: pin.version.clone(),
                manifest: manifest.version,
            });
        }

        let subjects = manifest.subjects;
        let reviewer = ResolvedReviewer::new(
            name,
            manifest.version,
            found,
            root,
            manifest.runner.build(),
            files,
        );
        Ok((reviewer, subjects))
    }

    pub fn resolve_for_subject(
        &self,
        name: &str,
        registry: &Registry,
        subject: review_core::SubjectKind,
    ) -> Result<ResolvedReviewer, LockError> {
        let (reviewer, subjects) = self.resolve_package(name, registry)?;
        if !subjects.contains(&subject) {
            return Err(LockError::UnsupportedSubject {
                name: name.to_string(),
                subject,
            });
        }
        Ok(reviewer)
    }

    /// Compute the pin for a package as it stands — what lock generation records.
    ///
    /// The same validation as resolution: a manifest with a floating version cannot be pinned,
    /// so the refusal happens when the lock is written rather than on the first run after.
    pub fn pin(name: &str, registry: &Registry) -> Result<Pin, LockError> {
        let (root, files) = registry.read(name)?;
        Self::pin_from_files(name, &root, &files)
    }

    /// Compute one exact package pin from already captured bytes.
    ///
    /// Generators use this path before anything exists on disk. It applies the same manifest,
    /// name, version, path, and digest validation as registry-backed locking; callers cannot
    /// mint a syntactically plausible pin around malformed package bytes.
    pub fn pin_package_files(
        name: &str,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Pin, LockError> {
        Self::pin_from_files(name, Path::new("<generated-package>"), files)
    }

    fn pin_from_files(
        name: &str,
        root: &Path,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Pin, LockError> {
        let manifest = Self::manifest(name, root, files)?;
        if manifest.name != name {
            return Err(LockError::NameMismatch {
                requested: name.to_string(),
                manifest: manifest.name,
            });
        }
        if !exact_version(&manifest.version) {
            return Err(LockError::Floating {
                name: name.to_string(),
                version: manifest.version,
            });
        }
        Ok(Pin {
            version: manifest.version,
            digest: package_digest_from_files(files),
        })
    }

    fn manifest(
        name: &str,
        root: &Path,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<PackageManifest, LockError> {
        let bytes = files
            .get("reviewer.toml")
            .ok_or_else(|| LockError::MissingManifest {
                name: name.to_string(),
                root: root.to_path_buf(),
            })?;
        let text = std::str::from_utf8(bytes)
            .map_err(|e| LockError::Parse(format!("reviewer.toml for `{name}`: {e}")))?;
        let manifest: PackageManifest = toml::from_str(text)
            .map_err(|e| LockError::Parse(format!("reviewer.toml for `{name}`: {e}")))?;
        if manifest.subjects.is_empty() {
            return Err(LockError::Parse(format!(
                "reviewer.toml for `{name}` accepts no Subject kind"
            )));
        }
        let unique: BTreeSet<_> = manifest.subjects.iter().collect();
        if unique.len() != manifest.subjects.len() {
            return Err(LockError::Parse(format!(
                "reviewer.toml for `{name}` contains duplicate Subject kinds"
            )));
        }
        Ok(manifest)
    }
}
