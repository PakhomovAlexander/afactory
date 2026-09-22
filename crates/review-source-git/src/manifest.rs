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

pub use review_core::{decode_path, encode_path};

/// Whether `encoded` is the one spelling [`encode_path`] gives the raw bytes `decoded`.
pub(crate) fn is_canonical_path_encoding(encoded: &str, decoded: &[u8]) -> bool {
    let valid_literal = std::str::from_utf8(decoded).is_ok_and(review_core::is_valid_repo_path);
    if !encoded.as_bytes().contains(&b'%') {
        return valid_literal;
    }
    if valid_literal && !decoded.contains(&b'%') {
        return false;
    }
    let bytes = encoded.as_bytes();
    let literal = |byte: u8| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'+' | b'@')
    };
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            if !literal(bytes[index]) {
                return false;
            }
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len()
            || !bytes[index + 1].is_ascii_digit() && !matches!(bytes[index + 1], b'A'..=b'F')
            || !bytes[index + 2].is_ascii_digit() && !matches!(bytes[index + 2], b'A'..=b'F')
        {
            return false;
        }
        let decoded_byte = (hex_value(bytes[index + 1]) << 4) | hex_value(bytes[index + 2]);
        if literal(decoded_byte) {
            return false;
        }
        index += 3;
    }
    true
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'A'..=b'F' => byte - b'A' + 10,
        _ => unreachable!("canonical hex was checked above"),
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    NoncanonicalPath(String),
    DuplicatePath(String),
    UnsortedPaths { previous: String, path: String },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoncanonicalPath(path) => {
                write!(formatter, "manifest path `{path}` is not canonical")
            }
            Self::DuplicatePath(path) => write!(formatter, "manifest repeats path `{path}`"),
            Self::UnsortedPaths { previous, path } => write!(
                formatter,
                "manifest path `{path}` is ordered before preceding path `{previous}`"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    pub fn new(mut entries: Vec<Entry>) -> Result<Manifest, ManifestError> {
        // Sorted by raw path bytes: the one ordering that does not depend on a locale.
        entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let manifest = Manifest { entries };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate invariants required by parallel materialization and positional diffing.
    pub fn validate(&self) -> Result<(), ManifestError> {
        for entry in &self.entries {
            let valid = if entry.path.contains('%') {
                is_canonical_path_encoding(&entry.path, &decode_path(&entry.path))
            } else {
                is_canonical_path_encoding(&entry.path, entry.path.as_bytes())
            };
            if !valid {
                return Err(ManifestError::NoncanonicalPath(entry.path.clone()));
            }
        }
        for pair in self.entries.windows(2) {
            match pair[0].path.as_bytes().cmp(pair[1].path.as_bytes()) {
                std::cmp::Ordering::Equal => {
                    return Err(ManifestError::DuplicatePath(pair[1].path.clone()));
                }
                std::cmp::Ordering::Greater => {
                    return Err(ManifestError::UnsortedPaths {
                        previous: pair[0].path.clone(),
                        path: pair[1].path.clone(),
                    });
                }
                std::cmp::Ordering::Less => {}
            }
        }
        Ok(())
    }

    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.path == path)
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
        format!("sha256:{}", review_core::hex::encode(&hasher.finalize()))
    }
}

/// The real filesystem path an encoded manifest path names. On unix this is the decoded bytes
/// verbatim, so a non-UTF-8 or `%`-bearing name reaches the filesystem as itself.
#[cfg(unix)]
pub fn fs_path(encoded: &str) -> std::path::PathBuf {
    fs_path_bytes(&decode_path(encoded))
}

#[cfg(unix)]
pub(crate) fn fs_path_bytes(decoded: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(decoded).into()
}

#[cfg(not(unix))]
pub fn fs_path(encoded: &str) -> std::path::PathBuf {
    fs_path_bytes(&decode_path(encoded))
}

#[cfg(not(unix))]
pub(crate) fn fs_path_bytes(decoded: &[u8]) -> std::path::PathBuf {
    // Off-unix, paths are not bytes; this lossless mapping does not apply, so fall back.
    std::path::PathBuf::from(String::from_utf8_lossy(decoded).into_owned())
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

/// Stream manifest content identity through the same domain as [`digest_bytes`].
pub fn digest_reader_with_buffer(
    reader: impl std::io::Read,
    buffer: &mut [u8],
) -> std::io::Result<(String, u64)> {
    review_store::canonical::blob_content_id_reader_with_buffer(reader, buffer)
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
        ])
        .unwrap();
        let b = Manifest::new(vec![
            entry("a.txt", EntryKind::File, b"one"),
            entry("b.txt", EntryKind::File, b"two"),
        ])
        .unwrap();
        assert_eq!(a.content_digest(), b.content_digest());
    }

    #[test]
    fn only_the_encode_path_spelling_is_canonical() {
        let content = b"same bytes";
        for raw in [
            &b" notes.md"[..],
            b"docs/50% off.md",
            b"a\xffb",
            b"src/ok.rs",
        ] {
            Manifest::new(vec![entry(&encode_path(raw), EntryKind::File, content)]).unwrap();
        }
        for noncanonical in [" notes.md", "docs/50%25 off.md", "%61.md", "a%ffb"] {
            assert_eq!(
                Manifest::new(vec![entry(noncanonical, EntryKind::File, content)]),
                Err(ManifestError::NoncanonicalPath(noncanonical.into())),
                "{noncanonical:?}"
            );
        }
    }

    #[test]
    fn duplicate_paths_are_not_a_manifest() {
        let manifest = Manifest {
            entries: vec![
                entry("same", EntryKind::File, b"one"),
                entry("same", EntryKind::File, b"two"),
            ],
        };
        assert_eq!(
            manifest.validate(),
            Err(ManifestError::DuplicatePath("same".into()))
        );
        assert_eq!(
            Manifest::new(manifest.entries),
            Err(ManifestError::DuplicatePath("same".into()))
        );
    }

    #[test]
    fn deserialized_manifests_must_retain_canonical_path_order() {
        let manifest = Manifest {
            entries: vec![
                entry("b", EntryKind::File, b"two"),
                entry("a", EntryKind::File, b"one"),
            ],
        };
        assert_eq!(
            manifest.validate(),
            Err(ManifestError::UnsortedPaths {
                previous: "b".into(),
                path: "a".into(),
            })
        );
        assert_eq!(
            manifest.get("a").map(|entry| entry.path.as_str()),
            Some("a")
        );
    }

    #[test]
    fn the_executable_bit_is_part_of_identity() {
        let plain = Manifest::new(vec![entry("s.sh", EntryKind::File, b"#!/bin/sh\n")]).unwrap();
        let exec =
            Manifest::new(vec![entry("s.sh", EntryKind::Executable, b"#!/bin/sh\n")]).unwrap();
        assert_ne!(plain.content_digest(), exec.content_digest());
    }

    #[test]
    fn fields_cannot_be_re_cut_into_a_different_manifest() {
        // Without length framing, "ab" + "c" and "a" + "bc" would hash alike.
        let a = Manifest::new(vec![entry("ab", EntryKind::File, b"c")]).unwrap();
        let b = Manifest::new(vec![entry("a", EntryKind::File, b"bc")]).unwrap();
        assert_ne!(a.content_digest(), b.content_digest());
    }

    #[test]
    fn a_symlink_is_not_the_file_it_points_at() {
        let link = Manifest::new(vec![entry("l", EntryKind::Symlink, b"target")]).unwrap();
        let file = Manifest::new(vec![entry("l", EntryKind::File, b"target")]).unwrap();
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
