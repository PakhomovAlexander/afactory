//! `.review/review.lock`: a reviewer pinned by content digest, never `latest`.
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

/// The lockfile itself — `.review/review.lock`, TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
    pub version: u32,
    #[serde(default)]
    pub reviewers: BTreeMap<String, Pin>,
}

/// A reviewer that survived resolution. The type itself lives with the adapters
/// (`review-runner`), which consume it; this module is the resolver that mints it.
pub use review_runner::ResolvedReviewer;

/// Registries searched in order. Typically project, user, system.
pub struct Registry {
    roots: Vec<PathBuf>,
}

impl Registry {
    pub fn new(roots: impl IntoIterator<Item = impl Into<PathBuf>>) -> Registry {
        Registry {
            roots: roots.into_iter().map(Into::into).collect(),
        }
    }

    /// The first root that has the package directory. Search stops here: whether that copy
    /// verifies is the next question, and a failure there must not be papered over by a copy
    /// further down the chain.
    fn locate(&self, name: &str) -> Result<PathBuf, LockError> {
        for root in &self.roots {
            let candidate = root.join(name);
            if candidate.is_dir() {
                return Ok(candidate);
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
                let relative = path
                    .strip_prefix(root)
                    .expect("walked paths live under the root")
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                let bytes = std::fs::read(&path).map_err(|e| io(&path, e))?;
                files.insert(relative, bytes);
            }
        }
    }
    Ok(files)
}

fn digest_of(files: &BTreeMap<String, Vec<u8>>) -> String {
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
    Ok(digest_of(&collect(name, root)?))
}

impl Lockfile {
    pub fn empty() -> Lockfile {
        Lockfile {
            version: 1,
            reviewers: BTreeMap::new(),
        }
    }

    /// Parse and validate. A floating version or malformed digest is refused *here*, so a bad
    /// pin cannot sit latent in a file that parses.
    pub fn from_toml(text: &str) -> Result<Lockfile, LockError> {
        let lockfile: Lockfile =
            toml::from_str(text).map_err(|e| LockError::Parse(e.to_string()))?;
        if lockfile.version != 1 {
            return Err(LockError::Parse(format!(
                "unsupported lockfile version {}; this kernel understands version 1",
                lockfile.version
            )));
        }
        for (name, pin) in &lockfile.reviewers {
            if !exact_version(&pin.version) {
                return Err(LockError::Floating {
                    name: name.clone(),
                    version: pin.version.clone(),
                });
            }
            if !well_formed_digest(&pin.digest) {
                return Err(LockError::MalformedDigest {
                    name: name.clone(),
                    digest: pin.digest.clone(),
                });
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
            .ok_or_else(|| LockError::NotLocked {
                name: name.to_string(),
            })?;
        let root = registry.locate(name)?;
        let files = collect(name, &root)?;

        let found = digest_of(&files);
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

    pub fn resolve(&self, name: &str, registry: &Registry) -> Result<ResolvedReviewer, LockError> {
        self.resolve_package(name, registry)
            .map(|(reviewer, _)| reviewer)
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
        let root = registry.locate(name)?;
        let files = collect(name, &root)?;
        let manifest = Self::manifest(name, &root, &files)?;
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
            digest: digest_of(&files),
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
