//! The Claude CLI's per-working-directory history, and af's cleanup of it.
//!
//! The pinned `claude` CLI keeps what it records for one working directory under its harness
//! directory:
//!
//! ```text
//! <config dir>/projects/<slug of the working directory>/
//! ```
//!
//! where the config directory is `$CLAUDE_CONFIG_DIR` when the operator selected a profile and
//! `<HOME>/.claude` otherwise, and the slug replaces every byte of the absolute working
//! directory that is not ASCII alphanumeric with `-`. That layout is provider surface, pinned
//! here by fixture exactly as the security flags are.
//!
//! af removes the project directory of a working directory it created for one Attempt alone
//! once that Attempt's process has exited (ADR-0144), and nothing else. The granted root is
//! opened as the operator gave it: a grant is a path the operator chose, and refusing it because
//! their home directory is reached through a link would refuse ordinary machines. Everything
//! *below* that root is opened descriptor-relative without following a link.

use std::path::{Path, PathBuf};

/// The directory the harness keys its history by working directory under.
const PROJECTS_DIRECTORY: &str = "projects";

/// Why the `projects` directory below the granted root could not be opened safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectsRefusal {
    /// Something other than a directory stands where the store or `projects` should be.
    NotADirectory,
    /// `projects` is a symlink; nothing below the granted root is followed.
    SymlinkedParent,
    /// The directory could not be opened at all.
    Unreadable,
}

/// The operator's harness directory, as the adapter's explicit grants describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSessionStore {
    root: PathBuf,
}

impl ClaudeSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store the adapter's own auth grants imply: the operator-selected profile directory
    /// when there is one, otherwise the default under the real `HOME`. This crate never reads
    /// the environment itself, so what it addresses is exactly what the caller granted.
    pub fn from_grants(config_dir: Option<&str>, home: &str) -> Self {
        match config_dir {
            Some(directory) => Self::new(directory),
            None => Self::new(Path::new(home).join(".claude")),
        }
    }

    /// The harness's directory name for one working directory: the absolute path with every
    /// byte that is not ASCII alphanumeric replaced by `-`. Pinned provider surface.
    pub fn project_slug(working_directory: &Path) -> String {
        working_directory
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() {
                    char::from(*byte)
                } else {
                    '-'
                }
            })
            .collect()
    }

    /// Where the harness keeps its history for an invocation in `working_directory`.
    #[cfg(test)]
    fn project_path(&self, working_directory: &Path) -> PathBuf {
        self.root
            .join(PROJECTS_DIRECTORY)
            .join(Self::project_slug(working_directory))
    }
}

/// The store's `projects` directory, opened `O_NOFOLLOW` below the granted root. `None` when
/// the harness has written no history at all.
fn open_projects(root: &Path) -> Result<Option<nix::dir::Dir>, ProjectsRefusal> {
    use nix::dir::Dir;
    use nix::fcntl::{OFlag, open, openat};
    use nix::sys::stat::Mode as NixMode;

    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY;
    // The granted root is the operator's own path: opened as given, because a grant is what
    // the operator chose to hand over. Every component below it is no-follow.
    let root = match open(root, flags, NixMode::empty()) {
        Ok(root) => root,
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(nix::errno::Errno::ENOTDIR) => {
            return Err(ProjectsRefusal::NotADirectory);
        }
        Err(_) => return Err(ProjectsRefusal::Unreadable),
    };
    let projects = match openat(
        &root,
        PROJECTS_DIRECTORY,
        flags | OFlag::O_NOFOLLOW,
        NixMode::empty(),
    ) {
        Ok(projects) => projects,
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR) => {
            return Err(ProjectsRefusal::SymlinkedParent);
        }
        Err(_) => return Err(ProjectsRefusal::Unreadable),
    };
    Dir::from_fd(projects)
        .map(Some)
        .map_err(|_| ProjectsRefusal::Unreadable)
}

/// The name prefixes af gives a directory it creates for one Attempt, a probe or a check:
/// `af-sandbox-<pid>-<random>` and `af-check-<pid>-<random>` (`review_sandbox::stale`).
const AF_DIRECTORY_PREFIXES: [&str; 2] = ["af-sandbox-", "af-check-"];

/// Whether `name` is the name af gives such a directory: a prefix, a decimal pid, `-` and a
/// non-empty random part.
fn is_af_directory_name(name: &str) -> bool {
    AF_DIRECTORY_PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.split_once('-'))
            .is_some_and(|(pid, random)| {
                !pid.is_empty()
                    && pid.len() <= 10
                    && pid.bytes().all(|byte| byte.is_ascii_digit())
                    && !random.is_empty()
            })
    })
}

/// Whether `working_directory` is a directory af created for one Attempt alone: an absolute
/// `…/af-sandbox-<pid>-<random>` (or `af-check-…`) directory, or the `tree` below one. No other
/// working directory's harness history is ever af's to remove.
pub fn is_af_attempt_directory(working_directory: &Path) -> bool {
    if !working_directory.is_absolute() {
        return false;
    }
    let name = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    };
    match name(working_directory) {
        Some(last) if is_af_directory_name(&last) => true,
        Some(last) if last == "tree" => working_directory
            .parent()
            .and_then(name)
            .is_some_and(|parent| is_af_directory_name(&parent)),
        _ => false,
    }
}

/// Whether a harness project directory name is the slug of a path af created for an Attempt,
/// a probe or a check: an `af-sandbox-<pid>-<random>` or `af-check-<pid>-<random>` directory
/// directly below one of `temp_slugs`, the slugs of the temporary roots af used (possibly with
/// a deeper component such as `-tree` after it), or the earlier `<temp>/.tmp<random>/tree` form
/// below one of them. The temporary root is required: a project whose path merely contains such
/// a component elsewhere (`~/src/af-sandbox-42-demo`) is the user's, never af's. Used only
/// where no working directory is known any more (`af self uninstall --purge`).
pub fn is_af_project_slug(slug: &str, temp_slugs: &[String]) -> bool {
    temp_slugs.iter().any(|temp| {
        let Some(rest) = slug.strip_prefix(temp.as_str()) else {
            return false;
        };
        let directory = rest.strip_prefix('-').is_some_and(|rest| {
            AF_DIRECTORY_PREFIXES.iter().any(|prefix| {
                rest.strip_prefix(prefix).is_some_and(|after| {
                    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
                    digits > 0
                        && after[digits..].strip_prefix('-').is_some_and(|random| {
                            random
                                .bytes()
                                .next()
                                .is_some_and(|b| b.is_ascii_alphanumeric())
                                && random
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        })
                })
            })
        });
        directory
            || rest
                .strip_prefix("--tmp")
                .and_then(|rest| rest.strip_suffix("-tree"))
                .is_some_and(|random| {
                    !random.is_empty() && random.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
    })
}

impl ClaudeSessionStore {
    /// Remove the harness's project directory for `working_directory` — every transcript and
    /// anything else the CLI kept for it — when that directory is one af created for this
    /// Attempt alone ([`is_af_attempt_directory`]). Both spellings of the path count (as given
    /// and resolved, since the harness keys by the process's working directory); nothing is
    /// followed below the granted root. Returns how many project directories were removed.
    pub fn remove_attempt_project(&self, working_directory: &Path) -> Result<usize, String> {
        if !is_af_attempt_directory(working_directory) {
            return Ok(0);
        }
        let mut slugs = vec![Self::project_slug(working_directory)];
        if let Ok(resolved) = working_directory.canonicalize()
            && is_af_attempt_directory(&resolved)
        {
            slugs.push(Self::project_slug(&resolved));
        }
        slugs.sort();
        slugs.dedup();
        self.remove_projects(&slugs)
    }

    /// Remove the named project directories below `projects/`, without following a link.
    /// Absent ones are skipped. Returns how many were removed.
    pub fn remove_projects(&self, slugs: &[String]) -> Result<usize, String> {
        let projects = match open_projects(&self.root) {
            Ok(Some(projects)) => projects,
            Ok(None) => return Ok(0),
            Err(reason) => {
                return Err(format!(
                    "the Claude projects directory cannot be opened safely ({reason:?})"
                ));
            }
        };
        let mut removed = 0;
        for slug in slugs {
            if slug.is_empty() || slug.contains('/') || slug == "." || slug == ".." {
                continue;
            }
            // Measured now, once the Attempt has exited, and removed only while it is still
            // that directory: claimed, checked and emptied through descriptors (ADR-0144).
            let identity = match nix::sys::stat::fstatat(
                &projects,
                slug.as_str(),
                nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => review_sandbox::Identity::of_stat(&stat),
                Err(nix::errno::Errno::ENOENT) => continue,
                Err(errno) => {
                    return Err(format!("inspecting a Claude project directory: {errno}"));
                }
            };
            match review_sandbox::remove_tree_at(
                &projects,
                std::ffi::OsStr::new(slug),
                Some(identity),
            ) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("removing a Claude project directory: {error}"));
                }
            }
        }
        Ok(removed)
    }

    /// Every project directory name below `projects/`, for a caller that selects af's own.
    pub fn project_names(&self) -> Result<Vec<String>, String> {
        let mut projects = match open_projects(&self.root) {
            Ok(Some(projects)) => projects,
            Ok(None) => return Ok(Vec::new()),
            Err(reason) => {
                return Err(format!(
                    "the Claude projects directory cannot be opened safely ({reason:?})"
                ));
            }
        };
        let mut names = Vec::new();
        for entry in projects.iter() {
            let entry = entry.map_err(|errno| format!("listing Claude projects: {errno}"))?;
            if let Ok(name) = entry.file_name().to_str()
                && name != "."
                && name != ".."
            {
                names.push(name.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_directory_af_made_for_an_attempt_is_an_attempt_directory() {
        for made in [
            "/tmp/af-sandbox-42-Ab3xYz/tree",
            "/var/folders/x/T/af-sandbox-7-q/tree",
            "/tmp/af-sandbox-42-Ab3xYz",
            "/tmp/af-check-42-Ab3xYz",
        ] {
            assert!(is_af_attempt_directory(Path::new(made)), "{made}");
        }
        for other in [
            "/",
            "/home/me/project",
            "/tmp/.tmpAb3xYz/tree",
            "/tmp/af-sandbox--x/tree",
            "/tmp/af-sandbox-42-/tree",
            "/tmp/af-sandbox-4x2-y/tree",
            "/tmp/af-sandbox-42-y/tree/sub",
            "af-sandbox-42-y/tree",
        ] {
            assert!(!is_af_attempt_directory(Path::new(other)), "{other}");
        }
    }

    #[test]
    fn an_af_project_slug_names_an_af_component_or_the_earlier_temporary_form() {
        let temp = vec![ClaudeSessionStore::project_slug(Path::new(
            "/var/folders/x/T",
        ))];
        for slug in [
            "-var-folders-x-T-af-sandbox-42-Ab3xYz",
            "-var-folders-x-T-af-sandbox-42-Ab3xYz-tree",
            "-var-folders-x-T-af-check-9-q",
            "-var-folders-x-T--tmpAb3xYz-tree",
        ] {
            assert!(is_af_project_slug(slug, &temp), "{slug}");
        }
        for slug in [
            "-",
            "-Users-me-src-afactory",
            // A user's own directory that only looks like af's is never af's.
            "-Users-alice-src-af-sandbox-42-demo",
            "-tmp-af-sandbox-42-Ab3xYz-tree",
            "-var-folders-x-T-sub-af-sandbox-1-a",
            "-tmp--tmpAb3xYz-tree",
            "-var-folders-x-T-af-sandbox--tree",
            "-var-folders-x-T-af-sandbox-42-",
            "-var-folders-x-T--tmpAb3xYz-tree-more",
        ] {
            assert!(!is_af_project_slug(slug, &temp), "{slug}");
        }
    }

    #[test]
    fn an_attempt_project_is_removed_and_every_other_project_is_untouched() {
        let config = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().join("af-sandbox-42-Ab3xYz");
        let tree = sandbox.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let store = ClaudeSessionStore::new(config.path());
        let attempt = config
            .path()
            .join("projects")
            .join(ClaudeSessionStore::project_slug(&tree));
        std::fs::create_dir_all(attempt.join("sub")).unwrap();
        std::fs::write(attempt.join("one.jsonl"), b"{}").unwrap();
        let other = config.path().join("projects").join("-Users-me-project");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("keep.jsonl"), b"{}").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), attempt.join("escape")).unwrap();
        std::fs::write(outside.path().join("precious"), b"x").unwrap();

        assert_eq!(store.remove_attempt_project(&tree).unwrap(), 1);
        assert!(!attempt.exists());
        assert!(other.join("keep.jsonl").exists());
        assert!(outside.path().join("precious").exists());
        // Idempotent, and never for a directory af did not make.
        assert_eq!(store.remove_attempt_project(&tree).unwrap(), 0);
        assert_eq!(
            store
                .remove_attempt_project(Path::new("/Users/me/project"))
                .unwrap(),
            0
        );
        assert!(other.exists());
        assert_eq!(store.project_names().unwrap(), ["-Users-me-project"]);
    }

    #[test]
    fn a_project_slug_is_the_pinned_harness_encoding_of_a_working_directory() {
        assert_eq!(
            ClaudeSessionStore::project_slug(Path::new("/tmp/af.sandbox/x-1")),
            "-tmp-af-sandbox-x-1"
        );
        assert_eq!(ClaudeSessionStore::project_slug(Path::new("/")), "-");
        assert_eq!(
            ClaudeSessionStore::project_slug(Path::new("/a/b")),
            ClaudeSessionStore::project_slug(Path::new("/a/b")),
            "the slug is a function of the path alone"
        );
        assert_ne!(
            ClaudeSessionStore::project_slug(Path::new("/a/b")),
            ClaudeSessionStore::project_slug(Path::new("/a/c"))
        );
    }

    #[test]
    fn a_store_addresses_the_profile_the_grants_selected() {
        let default = ClaudeSessionStore::from_grants(None, "/home/operator");
        assert_eq!(
            default.project_path(Path::new("/w")),
            Path::new("/home/operator/.claude/projects/-w")
        );
        let profile = ClaudeSessionStore::from_grants(Some("/profiles/review"), "/home/operator");
        assert_eq!(
            profile.project_path(Path::new("/w")),
            Path::new("/profiles/review/projects/-w")
        );
    }

    #[test]
    fn a_symlinked_projects_directory_is_refused_and_nothing_behind_it_is_removed() {
        let config = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let tree = temp.path().join("af-sandbox-42-Ab3xYz").join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let victim = outside.path().join(ClaudeSessionStore::project_slug(&tree));
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep.jsonl"), b"{}").unwrap();
        std::os::unix::fs::symlink(outside.path(), config.path().join("projects")).unwrap();
        let store = ClaudeSessionStore::new(config.path());
        let error = store.remove_attempt_project(&tree).unwrap_err();
        assert!(error.contains("cannot be opened safely"), "{error}");
        assert!(store.project_names().is_err());
        assert!(victim.join("keep.jsonl").exists());
    }
}
