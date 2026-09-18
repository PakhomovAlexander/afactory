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
//! never carry a host path. The search is bounded, opens every component `O_NOFOLLOW`, and
//! refuses anything that is not the regular file a transcript is.

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
            Located::Found { path, bytes, read } => {
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
            Ok(Located::Found { path, .. }) => {
                // A transcript found under another path is not the one the capture recorded.
                // Recovery deletes what its own record describes, never what it stumbles on.
                if expected_path_digest
                    .is_some_and(|digest| digest != ClaudeSessionStore::path_digest(&path))
                {
                    return SessionDeletion::Refused(SessionCleanupRefusalV1::NotRegularFile);
                }
                unlink_no_follow(&path)
            }
        }
    }

    fn materialize(
        &self,
        working_directory: &Path,
        source_session_id: &str,
        transcript: &[u8],
    ) -> Result<String, String> {
        let path = self.transcript_path(working_directory, source_session_id);
        write_no_follow(&path, transcript)?;
        Ok(ClaudeSessionStore::path_digest(&path))
    }
}

/// What a bounded search for one session identity found.
enum Located {
    Missing,
    Refused(SessionCleanupRefusalV1),
    Found {
        path: PathBuf,
        bytes: u64,
        read: Box<dyn FnOnce() -> std::io::Result<Vec<u8>>>,
    },
}

#[cfg(unix)]
fn located(store: &ClaudeSessionStore, session_id: &str) -> Result<Located, String> {
    use std::io::Read;

    use nix::dir::Dir;
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::{Mode as NixMode, SFlag, fstat};

    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
    let projects = store.root.join(PROJECTS_DIRECTORY);
    let mut directory = match Dir::open(&projects, flags | OFlag::O_DIRECTORY, NixMode::empty()) {
        Ok(directory) => directory,
        Err(nix::errno::Errno::ENOENT) => return Ok(Located::Missing),
        Err(nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR) => {
            return Ok(Located::Refused(SessionCleanupRefusalV1::SymlinkedParent));
        }
        Err(_) => return Ok(Located::Refused(SessionCleanupRefusalV1::Unreadable)),
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
        return Ok(Located::Found { path, bytes, read });
    }
    Ok(Located::Missing)
}

#[cfg(unix)]
fn unlink_no_follow(path: &Path) -> SessionDeletion {
    use nix::dir::Dir;
    use nix::fcntl::OFlag;
    use nix::sys::stat::Mode as NixMode;
    use nix::unistd::{UnlinkatFlags, unlinkat};

    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY;
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return SessionDeletion::Refused(SessionCleanupRefusalV1::Unreadable);
    };
    let directory = match Dir::open(parent, flags, NixMode::empty()) {
        Ok(directory) => directory,
        Err(nix::errno::Errno::ENOENT) => return SessionDeletion::AlreadyAbsent,
        Err(nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR) => {
            return SessionDeletion::Refused(SessionCleanupRefusalV1::SymlinkedParent);
        }
        Err(_) => return SessionDeletion::Refused(SessionCleanupRefusalV1::Unreadable),
    };
    match unlinkat(&directory, name, UnlinkatFlags::NoRemoveDir) {
        Ok(()) => {
            // Best effort: a project directory the harness keyed to a sandbox that no longer
            // exists holds nothing once its transcript is gone, and removing it keeps the
            // bounded search over the operator's harness directory bounded in practice too.
            drop(directory);
            let _ = std::fs::remove_dir(parent);
            SessionDeletion::Deleted
        }
        Err(nix::errno::Errno::ENOENT) => SessionDeletion::AlreadyAbsent,
        Err(_) => SessionDeletion::Refused(SessionCleanupRefusalV1::Unreadable),
    }
}

#[cfg(unix)]
fn write_no_follow(path: &Path, transcript: &[u8]) -> Result<(), String> {
    use std::io::Write;

    use nix::dir::Dir;
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode as NixMode;

    let parent = path
        .parent()
        .ok_or_else(|| "a session transcript needs a project directory".to_string())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "a session transcript needs a portable name".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("preparing the harness session store: {error}"))?;
    let directory = Dir::open(
        parent,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY,
        NixMode::empty(),
    )
    .map_err(|error| format!("opening the harness session store: {error}"))?;
    let descriptor = openat(
        &directory,
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

#[cfg(not(unix))]
fn located(_store: &ClaudeSessionStore, _session_id: &str) -> Result<Located, String> {
    Ok(Located::Refused(SessionCleanupRefusalV1::Unreadable))
}

#[cfg(not(unix))]
fn unlink_no_follow(_path: &Path) -> SessionDeletion {
    SessionDeletion::Refused(SessionCleanupRefusalV1::Unreadable)
}

#[cfg(not(unix))]
fn write_no_follow(_path: &Path, _transcript: &[u8]) -> Result<(), String> {
    Err("session transcripts are captured only on unix hosts".into())
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
