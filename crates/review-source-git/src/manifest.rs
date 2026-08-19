//! The ordered manifest a snapshot's identity is computed from.
//!
//! Identity is over *content*, so the manifest carries only what a reviewer could observe by
//! reading the tree: path, kind, executable bit, and the digest of the bytes. Deliberately
//! absent: mtimes, inode numbers, owner, the commit that happened to contain it, and the branch
//! it was reached by. Two captures of the same tree from different clones, at different times,
//! under different configurations, must produce the same digest — that property is what makes
//! "the reviewers all inspected the same thing" a checkable claim rather than an assumption.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Executable,
    Symlink,
}

impl EntryKind {
    /// The git mode this corresponds to, for round-tripping and for humans reading a manifest.
    pub fn mode(self) -> &'static str {
        match self {
            EntryKind::File => "100644",
            EntryKind::Executable => "100755",
            EntryKind::Symlink => "120000",
        }
    }

    pub fn from_mode(mode: &str) -> Option<EntryKind> {
        Some(match mode {
            "100644" => EntryKind::File,
            "100755" => EntryKind::Executable,
            "120000" => EntryKind::Symlink,
            _ => return None,
        })
    }
}

/// One path in a snapshot. `content` is our own digest of the bytes — never a git object ID,
/// which would tie snapshot identity to git's hash function and to whether the bytes had been
/// through a clean filter on the way in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Entry {
    /// Repository-relative path, as raw bytes rendered losslessly for JSON. Paths are not
    /// guaranteed UTF-8, and a capture that silently dropped such a path would be reviewing a
    /// tree nobody has.
    pub path: String,
    pub kind: EntryKind,
    pub content: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: Vec<Entry>,
}

impl Manifest {
    pub fn new(mut entries: Vec<Entry>) -> Manifest {
        // Sorted by raw path bytes: the one ordering that does not depend on a locale.
        entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        Manifest { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// The snapshot's content digest.
    ///
    /// Framed by length so no combination of paths and digests can be re-cut into a different
    /// manifest with the same bytes — a manifest of one file named `a\nb` must not collide with
    /// a manifest of two files.
    pub fn content_digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"review.kernel/source-manifest/v1\0");
        hasher.update((self.entries.len() as u64).to_be_bytes());
        for entry in &self.entries {
            for field in [
                entry.path.as_bytes(),
                entry.kind.mode().as_bytes(),
                entry.content.as_bytes(),
            ] {
                hasher.update((field.len() as u64).to_be_bytes());
                hasher.update(field);
            }
            hasher.update(entry.size.to_be_bytes());
        }
        format!("sha256:{:x}", hasher.finalize())
    }
}

/// Render a raw path for the manifest, losslessly.
///
/// Non-UTF-8 paths exist, and dropping or lossily converting one would change what the snapshot
/// covers without saying so. Such a path is recorded in a percent-escaped form that round-trips.
pub fn encode_path(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) if !s.contains('%') => s.to_string(),
        _ => {
            let mut out = String::with_capacity(bytes.len());
            for byte in bytes {
                if byte.is_ascii_alphanumeric()
                    || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'+' | b' ' | b'@')
                {
                    out.push(*byte as char);
                } else {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            out
        }
    }
}

/// Invert [`encode_path`]: recover the raw path bytes a manifest entry names.
///
/// `encode_path` is a JSON-safe *display* form, not a filesystem path — joining it onto a
/// directory renames `docs/50%-off.md` to `docs/50%25-off.md` and drops any non-UTF-8 path
/// entirely. Anything that turns a manifest entry back into a real path must decode first.
/// Every `%XX` becomes its byte; every other byte is itself, which is exact because
/// `encode_path` emits only allowlisted ASCII and `%XX` escapes.
pub fn decode_path(encoded: &str) -> Vec<u8> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 3 <= bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// The real filesystem path an encoded manifest path names. On unix this is the decoded bytes
/// verbatim, so a non-UTF-8 or `%`-bearing name reaches the filesystem as itself.
#[cfg(unix)]
pub fn fs_path(encoded: &str) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(&decode_path(encoded)).into()
}

#[cfg(not(unix))]
pub fn fs_path(encoded: &str) -> std::path::PathBuf {
    // Off-unix, paths are not bytes; this lossless mapping does not apply, so fall back.
    std::path::PathBuf::from(String::from_utf8_lossy(&decode_path(encoded)).into_owned())
}

/// The digest a blob is filed under.
///
/// Deliberately the CAS's own content id rather than a source-specific domain: a manifest entry
/// is a *lookup key*, and two domains for the same bytes means the manifest names something the
/// store does not have. That mismatch is invisible until materialization, which is the point it
/// is most expensive to discover.
pub fn digest_bytes(bytes: &[u8]) -> String {
    review_store::canonical::blob_content_id(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: EntryKind, content: &[u8]) -> Entry {
        Entry {
            path: path.to_string(),
            kind,
            content: digest_bytes(content),
            size: content.len() as u64,
        }
    }

    #[test]
    fn order_of_construction_does_not_change_identity() {
        let a = Manifest::new(vec![
            entry("b.txt", EntryKind::File, b"two"),
            entry("a.txt", EntryKind::File, b"one"),
        ]);
        let b = Manifest::new(vec![
            entry("a.txt", EntryKind::File, b"one"),
            entry("b.txt", EntryKind::File, b"two"),
        ]);
        assert_eq!(a.content_digest(), b.content_digest());
    }

    #[test]
    fn the_executable_bit_is_part_of_identity() {
        let plain = Manifest::new(vec![entry("s.sh", EntryKind::File, b"#!/bin/sh\n")]);
        let exec = Manifest::new(vec![entry("s.sh", EntryKind::Executable, b"#!/bin/sh\n")]);
        assert_ne!(plain.content_digest(), exec.content_digest());
    }

    #[test]
    fn fields_cannot_be_re_cut_into_a_different_manifest() {
        // Without length framing, "ab" + "c" and "a" + "bc" would hash alike.
        let a = Manifest::new(vec![entry("ab", EntryKind::File, b"c")]);
        let b = Manifest::new(vec![entry("a", EntryKind::File, b"bc")]);
        assert_ne!(a.content_digest(), b.content_digest());
    }

    #[test]
    fn a_symlink_is_not_the_file_it_points_at() {
        let link = Manifest::new(vec![entry("l", EntryKind::Symlink, b"target")]);
        let file = Manifest::new(vec![entry("l", EntryKind::File, b"target")]);
        assert_ne!(link.content_digest(), file.content_digest());
    }

    #[test]
    fn non_utf8_paths_round_trip_instead_of_being_dropped() {
        assert_eq!(encode_path(b"src/ok.rs"), "src/ok.rs");
        assert_eq!(encode_path(&[b'a', 0xff, b'b']), "a%FFb");
        // A literal '%' is escaped too, so the encoding is unambiguous.
        assert_eq!(encode_path(b"100%"), "100%25");
    }

    #[test]
    fn encode_and_decode_are_inverse() {
        for raw in [
            &b"src/ok.rs"[..],
            b"docs/50%-off.md",
            &[b'a', 0xff, b'b'],
            b"caf\xc3\xa9.rs", // UTF-8 e-acute
            b"weird#name",
            b"100%",
        ] {
            assert_eq!(
                decode_path(&encode_path(raw)),
                raw,
                "round trip failed for {raw:?}"
            );
        }
    }
}
