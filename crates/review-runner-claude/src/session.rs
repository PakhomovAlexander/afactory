//! Worker warm layers, package P4: the Claude half of the Session Snapshot protocol.
//!
//! The pinned `claude` 2.1.273 CLI accepts `--session-id`, `--resume` and `--fork-session` in
//! print mode, and writes each session's transcript as one JSONL file under its harness
//! directory:
//!
//! ```text
//! <config dir>/projects/<slug of the working directory>/<session id>.jsonl
//! ```
//!
//! where the config directory is `$CLAUDE_CONFIG_DIR` when the operator selected a profile and
//! `<HOME>/.claude` otherwise, and the slug replaces every byte of the absolute working
//! directory that is not ASCII alphanumeric with `-`. That layout is provider surface, pinned
//! here by fixture exactly as the security flags are; a CLI that changes it makes the capture
//! find nothing, which drops the layer rather than corrupting anything.
//!
//! Every operation is located by the *kernel-assigned session identity alone*. Nothing here
//! reconstructs the Attempt's working directory, because a kernel recovering from a crash no
//! longer has the sandbox that produced the transcript — and because a durable record must
//! never carry a host path. The search is bounded and refuses anything that is not the regular
//! file a transcript is.
//!
//! The granted root is opened as the operator gave it: a grant is a path the operator chose,
//! and refusing it because their home directory is reached through a link would refuse ordinary
//! machines. Everything *below* that root — the projects directory, each project directory and
//! the transcript itself — is opened descriptor-relative with `O_NOFOLLOW`, because that is the
//! part of the tree a candidate process, a stale harness or a replaced project directory could
//! have changed. The directory a transcript was validated in stays open from the search through
//! the unlink, so the file removed is the file that was checked.

use std::path::{Path, PathBuf};

use review_core::SessionCleanupRefusalV1;
use review_runner::{CapturedSession, SessionCapture, SessionDeletion, SessionLayer};

/// The adapter kind recorded as a captured session's source.
pub const CLAUDE_PROVIDER_KIND: &str = "claude";

/// The directory the harness keys sessions by working directory under.
const PROJECTS_DIRECTORY: &str = "projects";

/// Transcript file extension the pinned CLI writes.
const TRANSCRIPT_SUFFIX: &str = ".jsonl";

/// How many project directories one search may scan before refusing. A harness home with more
/// than this is not a place the kernel will hunt through on every seal.
const MAX_PROJECT_DIRECTORIES: usize = 4096;

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

    /// Where the harness writes `session_id` for an invocation in `working_directory`.
    pub fn transcript_path(&self, working_directory: &Path, session_id: &str) -> PathBuf {
        self.root
            .join(PROJECTS_DIRECTORY)
            .join(Self::project_slug(working_directory))
            .join(format!("{session_id}{TRANSCRIPT_SUFFIX}"))
    }

    fn transcript_name(session_id: &str) -> String {
        format!("{session_id}{TRANSCRIPT_SUFFIX}")
    }

    /// The content identity of a harness path, as the durable source record names it.
    fn path_digest(path: &Path) -> String {
        review_store::canonical::blob_content_id(path.as_os_str().as_encoded_bytes())
    }
}

impl SessionLayer for ClaudeSessionStore {
    fn provider_kind(&self) -> &'static str {
        CLAUDE_PROVIDER_KIND
    }

    fn capture(&self, session_id: &str, max_bytes: u64) -> Result<SessionCapture, String> {
        located(self, session_id).map(|found| match found {
            Located::Missing => SessionCapture::Absent,
            Located::Refused(reason) => SessionCapture::Refused(reason),
            Located::Found {
                path, bytes, read, ..
            } => {
                if bytes > max_bytes {
                    SessionCapture::OverBound { bytes }
                } else {
                    match read() {
                        Ok(transcript) if transcript.len() as u64 == bytes => {
                            SessionCapture::Captured(CapturedSession {
                                transcript,
                                path_digest: ClaudeSessionStore::path_digest(&path),
                            })
                        }
                        // A transcript that changed size under the read is not the file the
                        // record would describe. Nothing is captured and nothing is deleted.
                        Ok(_) => SessionCapture::Refused(SessionCleanupRefusalV1::NotRegularFile),
                        Err(_) => SessionCapture::Refused(SessionCleanupRefusalV1::Unreadable),
                    }
                }
            }
        })
    }

    fn delete(&self, session_id: &str, expected_path_digest: Option<&str>) -> SessionDeletion {
        match located(self, session_id) {
            Err(_) => SessionDeletion::Refused(SessionCleanupRefusalV1::Unreadable),
            Ok(Located::Missing) => SessionDeletion::AlreadyAbsent,
            Ok(Located::Refused(reason)) => SessionDeletion::Refused(reason),
            Ok(Located::Found { handles, path, .. }) => {
                // A transcript found under another path is not the one the capture recorded.
                // Recovery deletes what its own record describes, never what it stumbles on.
                if expected_path_digest
                    .is_some_and(|digest| digest != ClaudeSessionStore::path_digest(&path))
                {
                    return SessionDeletion::Refused(SessionCleanupRefusalV1::NotRegularFile);
                }
                unlink_located(handles, &ClaudeSessionStore::transcript_name(session_id))
            }
        }
    }

    fn materialize(
        &self,
        working_directory: &Path,
        source_session_id: &str,
        transcript: &[u8],
    ) -> Result<String, String> {
        write_no_follow(
            &self.root,
            &ClaudeSessionStore::project_slug(working_directory),
            &ClaudeSessionStore::transcript_name(source_session_id),
            transcript,
        )?;
        Ok(ClaudeSessionStore::path_digest(
            &self.transcript_path(working_directory, source_session_id),
        ))
    }

    fn store_root(&self) -> Option<&Path> {
        Some(&self.root)
    }

    /// Credential shapes the harness could have echoed into a message or a tool record. A
    /// transcript carrying one is refused rather than filed: the bound is deliberately coarse,
    /// because a refused capture costs one cold Round and a filed secret costs more.
    fn credential_markers(&self) -> &'static [&'static [u8]] {
        &[
            b"sk-ant-",
            b"sk-proj-",
            b"ghp_",
            b"gho_",
            b"github_pat_",
            b"AKIA",
            b"ASIA",
            b"-----BEGIN ",
            b"xoxb-",
            b"xoxp-",
        ]
    }
}

/// The directories a search validated, kept open so the deletion acts on what was checked
/// rather than on a pathname that could name something else by then.
struct ProjectHandles {
    /// The store's `projects` directory.
    projects: nix::dir::Dir,
    /// The project directory the transcript was found in.
    project: std::os::fd::OwnedFd,
    /// Its name below `projects`, for removing it once it holds nothing.
    project_name: String,
}

/// What a bounded search for one session identity found.
enum Located {
    Missing,
    Refused(SessionCleanupRefusalV1),
    Found {
        handles: ProjectHandles,
        path: PathBuf,
        bytes: u64,
        read: Box<dyn FnOnce() -> std::io::Result<Vec<u8>>>,
    },
}

fn located(store: &ClaudeSessionStore, session_id: &str) -> Result<Located, String> {
    use std::io::Read;

    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::{Mode as NixMode, SFlag, fstat};

    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
    let projects = store.root.join(PROJECTS_DIRECTORY);
    let mut directory = match open_projects(&store.root) {
        Ok(Some(directory)) => directory,
        Ok(None) => return Ok(Located::Missing),
        Err(reason) => return Ok(Located::Refused(reason)),
    };
    let wanted = ClaudeSessionStore::transcript_name(session_id);
    let mut names = Vec::new();
    for entry in directory.iter() {
        let entry = entry.map_err(|error| format!("reading the harness session store: {error}"))?;
        let raw = entry.file_name().to_bytes();
        if matches!(raw, b"." | b"..") {
            continue;
        }
        if names.len() >= MAX_PROJECT_DIRECTORIES {
            return Ok(Located::Refused(SessionCleanupRefusalV1::Unreadable));
        }
        let Ok(name) = std::str::from_utf8(raw) else {
            continue;
        };
        names.push(name.to_string());
    }
    names.sort();
    for name in names {
        let Ok(project) = openat(
            &directory,
            name.as_str(),
            flags | OFlag::O_DIRECTORY,
            NixMode::empty(),
        ) else {
            // A project entry that is a symlink or not a directory is skipped, never followed.
            continue;
        };
        let Ok(descriptor) = openat(&project, wanted.as_str(), flags, NixMode::empty()) else {
            continue;
        };
        let path = projects.join(&name).join(&wanted);
        let stat = match fstat(&descriptor) {
            Ok(stat) => stat,
            Err(_) => return Ok(Located::Refused(SessionCleanupRefusalV1::Unreadable)),
        };
        if SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT != SFlag::S_IFREG {
            return Ok(Located::Refused(SessionCleanupRefusalV1::NotRegularFile));
        }
        let Ok(bytes) = u64::try_from(stat.st_size) else {
            return Ok(Located::Refused(SessionCleanupRefusalV1::NotRegularFile));
        };
        let read = Box::new(move || {
            let mut file = std::fs::File::from(descriptor);
            let mut transcript = Vec::with_capacity(bytes as usize);
            // One byte past the admitted size: a transcript that grew under the read is
            // refused rather than filed under a size it no longer has.
            file.by_ref()
                .take(bytes.saturating_add(1))
                .read_to_end(&mut transcript)?;
            Ok(transcript)
        });
        return Ok(Located::Found {
            handles: ProjectHandles {
                projects: directory,
                project,
                project_name: name,
            },
            path,
            bytes,
            read,
        });
    }
    Ok(Located::Missing)
}

/// The store's `projects` directory, opened `O_NOFOLLOW` below the granted root. `None` when
/// the harness has written no session at all.
fn open_projects(root: &Path) -> Result<Option<nix::dir::Dir>, SessionCleanupRefusalV1> {
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
            return Err(SessionCleanupRefusalV1::NotRegularFile);
        }
        Err(_) => return Err(SessionCleanupRefusalV1::Unreadable),
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
            return Err(SessionCleanupRefusalV1::SymlinkedParent);
        }
        Err(_) => return Err(SessionCleanupRefusalV1::Unreadable),
    };
    Dir::from_fd(projects)
        .map(Some)
        .map_err(|_| SessionCleanupRefusalV1::Unreadable)
}

/// Unlink the transcript in the exact directory the search validated. Nothing is resolved by
/// pathname again, so a project directory replaced between lookup and unlink cannot redirect
/// the deletion.
fn unlink_located(handles: ProjectHandles, name: &str) -> SessionDeletion {
    use nix::unistd::{UnlinkatFlags, unlinkat};

    let ProjectHandles {
        projects,
        project,
        project_name,
    } = handles;
    match unlinkat(&project, name, UnlinkatFlags::NoRemoveDir) {
        Ok(()) => {
            // Best effort: a project directory the harness keyed to a sandbox that no longer
            // exists holds nothing once its transcript is gone, and removing it keeps the
            // bounded search over the operator's harness directory bounded in practice too.
            drop(project);
            let _ = unlinkat(&projects, project_name.as_str(), UnlinkatFlags::RemoveDir);
            SessionDeletion::Deleted
        }
        Err(nix::errno::Errno::ENOENT) => SessionDeletion::AlreadyAbsent,
        Err(_) => SessionDeletion::Refused(SessionCleanupRefusalV1::Unreadable),
    }
}

/// Write one re-materialized transcript below the granted root. Only the root is created by
/// path; the projects directory, the project directory and the file are opened or created
/// descriptor-relative with `O_NOFOLLOW`, so nothing under the store can redirect the write.
fn write_no_follow(
    root: &Path,
    project_name: &str,
    name: &str,
    transcript: &[u8],
) -> Result<(), String> {
    use std::io::Write;
    use std::os::fd::{AsFd, OwnedFd};

    use nix::fcntl::{OFlag, open, openat};
    use nix::sys::stat::{Mode as NixMode, mkdirat};

    fn directory(parent: impl AsFd, name: &str) -> Result<OwnedFd, String> {
        let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW;
        match openat(&parent, name, flags, NixMode::empty()) {
            Ok(descriptor) => Ok(descriptor),
            Err(nix::errno::Errno::ENOENT) => {
                mkdirat(&parent, name, NixMode::from_bits_truncate(0o700))
                    .map_err(|error| format!("preparing the harness session store: {error}"))?;
                openat(&parent, name, flags, NixMode::empty())
                    .map_err(|error| format!("preparing the harness session store: {error}"))
            }
            Err(error) => Err(format!("preparing the harness session store: {error}")),
        }
    }

    std::fs::create_dir_all(root)
        .map_err(|error| format!("preparing the harness session store: {error}"))?;
    let root = open(
        root,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY,
        NixMode::empty(),
    )
    .map_err(|error| format!("opening the harness session store: {error}"))?;
    let projects = directory(&root, PROJECTS_DIRECTORY)?;
    let project = directory(&projects, project_name)?;
    let descriptor = openat(
        &project,
        name,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        NixMode::from_bits_truncate(0o600),
    )
    .map_err(|error| format!("writing the resumed session transcript: {error}"))?;
    let mut file = std::fs::File::from(descriptor);
    file.write_all(transcript)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("writing the resumed session transcript: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            default.transcript_path(Path::new("/w"), "0123456789abcdef0123456789ab"),
            Path::new("/home/operator/.claude/projects/-w/0123456789abcdef0123456789ab.jsonl")
        );
        let profile = ClaudeSessionStore::from_grants(Some("/profiles/review"), "/home/operator");
        assert_eq!(
            profile.transcript_path(Path::new("/w"), "s"),
            Path::new("/profiles/review/projects/-w/s.jsonl")
        );
    }
}
