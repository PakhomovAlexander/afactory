//! Untimed, bounded evidence handoff from a verified-empty sample namespace.

use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use serde_json::{Value, json};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub const FILE_BYTES_MAX: usize = 1024 * 1024;
pub const TOTAL_BYTES_MAX: usize = 16 * FILE_BYTES_MAX;
pub const FILES_MAX: usize = 64;

pub fn check_paths(paths: &[String]) -> Result<(), String> {
    if paths.len() > FILES_MAX
        || paths.windows(2).any(|pair| pair[0] >= pair[1])
        || paths.iter().any(|path| {
            path.len() > 512
                || path.split('/').any(|part| {
                    part.is_empty()
                        || part == "."
                        || part == ".."
                        || !part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                })
        })
    {
        return Err("evidence_paths holds at most 64 sorted unique relative file paths".into());
    }
    Ok(())
}

pub struct Evidence {
    pub report: Value,
    pub bytes: Vec<u8>,
}

/// Called only after supervision and before scratch cleanup. A refusal transfers no bytes,
/// even when earlier files were readable; it does not prevent the normal scratch cleanup.
pub fn capture(scratch: &Path, paths: &[String], verified_empty: bool) -> Evidence {
    if paths.is_empty() {
        return Evidence {
            report: Value::Null,
            bytes: Vec::new(),
        };
    }
    let collected = if verified_empty {
        collect(scratch, paths)
    } else {
        Err("the PID namespace was not verified empty; no evidence was read".into())
    };
    match collected {
        Ok((files, bytes)) => Evidence {
            report: json!({"status": "complete", "detail": "read declared regular files from \
                the verified-empty PID namespace before clearing scratch", "files": files}),
            bytes,
        },
        Err(reason) => Evidence {
            report: json!({"status": "refused", "detail": reason.chars().take(1024).collect::<String>(),
                           "files": []}),
            bytes: Vec::new(),
        },
    }
}

fn collect(scratch: &Path, paths: &[String]) -> Result<(Vec<Value>, Vec<u8>), String> {
    check_paths(paths)?;
    let directory = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let root = open(scratch, directory, Mode::empty()).map_err(|error| error.to_string())?;
    let mut files = Vec::new();
    let mut bytes = Vec::new();
    for path in paths {
        let read = || -> Result<Vec<u8>, String> {
            let mut parent = root.try_clone().map_err(|error| error.to_string())?;
            let mut parts = path.split('/').peekable();
            while let Some(part) = parts.next() {
                let flags = if parts.peek().is_some() {
                    directory
                } else {
                    OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC
                };
                parent = openat(&parent, part, flags, Mode::empty())
                    .map_err(|error| error.to_string())?;
            }
            let file = File::from(parent);
            let info = file.metadata().map_err(|error| error.to_string())?;
            if !info.is_file() || info.nlink() != 1 {
                return Err("not a singly linked regular evidence file".into());
            }
            let room = FILE_BYTES_MAX.min(TOTAL_BYTES_MAX - bytes.len());
            if info.len() > room as u64 {
                return Err("evidence exceeds its per-file or aggregate byte bound".into());
            }
            let mut content = Vec::new();
            file.take(room as u64 + 1)
                .read_to_end(&mut content)
                .map_err(|error| error.to_string())?;
            if content.len() != info.len() as usize || content.len() > room {
                return Err("evidence length changed or exceeded its byte bound".into());
            }
            Ok(content)
        };
        let content = read().map_err(|error| format!("{path}: {error}"))?;
        files.push(json!({"path": path, "bytes": content.len()}));
        bytes.extend_from_slice(&content);
    }
    Ok((files, bytes))
}
