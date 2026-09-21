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

/// The lossless JSON spelling used for manifest entry paths.
///
/// Artifacts written before this field existed deserialize as `legacy_v1`. New captures emit
/// `percent_v2`, whose percent alphabet can represent leading/trailing whitespace without
/// colliding with the canonical live Report spelling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathEncoding {
    #[default]
    LegacyV1,
    PercentV2,
}

impl PathEncoding {
    fn is_legacy(&self) -> bool {
        *self == Self::LegacyV1
    }
}

pub(crate) fn encode_path_for(encoding: PathEncoding, bytes: &[u8]) -> String {
    if encoding == PathEncoding::PercentV2 {
        return encode_path(bytes);
    }
    match std::str::from_utf8(bytes) {
        Ok(path) if !path.contains('%') => path.to_string(),
        _ => {
            let mut encoded = String::with_capacity(bytes.len());
            for byte in bytes {
                if byte.is_ascii_alphanumeric()
                    || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'+' | b' ' | b'@')
                {
                    encoded.push(*byte as char);
                } else {
                    encoded.push_str(&format!("%{byte:02X}"));
                }
            }
            encoded
        }
    }
}

pub(crate) fn is_canonical_path_encoding(
    encoding: PathEncoding,
    encoded: &str,
    decoded: &[u8],
) -> bool {
    let valid_literal = std::str::from_utf8(decoded).is_ok_and(|path| {
        encoding == PathEncoding::LegacyV1 || review_core::is_valid_repo_path(path)
    });
    if !encoded.as_bytes().contains(&b'%') {
        return valid_literal;
    }
    if valid_literal && !decoded.contains(&b'%') {
        return false;
    }
    let bytes = encoded.as_bytes();
    let literal = |byte: u8| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'+' | b'@')
            || encoding == PathEncoding::LegacyV1 && byte == b' '
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
    #[serde(default, skip_serializing_if = "PathEncoding::is_legacy")]
    pub path_encoding: PathEncoding,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    NoncanonicalPath {
        path: String,
        encoding: PathEncoding,
    },
    DuplicatePath(String),
    UnsortedPaths {
        previous: String,
        path: String,
    },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoncanonicalPath { path, encoding } => write!(
                formatter,
                "manifest path `{path}` is not canonical for {encoding:?}"
            ),
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
        // Keep the historical wire form when the two alphabets coincide. The generation marker
        // is needed only when at least one v2 spelling would be rejected by the legacy alphabet,
        // avoiding gratuitous CAS-id churn for ordinary trees.
        let path_encoding = if entries.iter().all(|entry| {
            !entry.path.contains('%')
                || encode_path_for(PathEncoding::LegacyV1, &decode_path(&entry.path)) == entry.path
        }) {
            PathEncoding::LegacyV1
        } else {
            PathEncoding::PercentV2
        };
        let manifest = Manifest {
            path_encoding,
            entries,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn new_with_encoding(
        mut entries: Vec<Entry>,
        path_encoding: PathEncoding,
    ) -> Result<Manifest, ManifestError> {
        entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let manifest = Manifest {
            path_encoding,
            entries,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn encode_key(&self, raw_path: &[u8]) -> String {
        encode_path_for(self.path_encoding, raw_path)
    }

    /// Validate invariants required by parallel materialization and positional diffing.
    pub fn validate(&self) -> Result<(), ManifestError> {
        for entry in &self.entries {
            let valid = if entry.path.contains('%') {
                let decoded = decode_path(&entry.path);
                is_canonical_path_encoding(self.path_encoding, &entry.path, &decoded)
            } else {
                is_canonical_path_encoding(self.path_encoding, &entry.path, entry.path.as_bytes())
            };
            if !valid {
                return Err(ManifestError::NoncanonicalPath {
                    path: entry.path.clone(),
                    encoding: self.path_encoding,
                });
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
        fn hash_entry(hasher: &mut Sha256, path: &[u8], entry: &Entry) {
            for field in [path, entry.kind.mode().as_bytes(), entry.content.as_bytes()] {
                hasher.update((field.len() as u64).to_be_bytes());
                hasher.update(field);
            }
            hasher.update(entry.size.to_be_bytes());
        }

        let mut hasher = Sha256::new();
        hasher.update(b"review.kernel/source-manifest/v1\0");
        hasher.update((self.entries.len() as u64).to_be_bytes());
        if self.path_encoding == PathEncoding::LegacyV1 {
            for entry in &self.entries {
                hash_entry(&mut hasher, entry.path.as_bytes(), entry);
            }
        } else {
            // Snapshot content identity predates the explicit encoding generation. Normalize a
            // v2 storage spelling back to the legacy spelling and order so the same raw tree
            // retains its digest across the representation upgrade.
            let mut entries: Vec<_> = self
                .entries
                .iter()
                .map(|entry| {
                    (
                        encode_path_for(PathEncoding::LegacyV1, &decode_path(&entry.path)),
                        entry,
                    )
                })
                .collect();
            entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            for (identity_path, entry) in entries {
                hash_entry(&mut hasher, identity_path.as_bytes(), entry);
            }
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
    fn path_encoding_generation_preserves_raw_tree_identity() {
        let content = b"same bytes";
        let legacy = Manifest {
            path_encoding: PathEncoding::LegacyV1,
            entries: vec![entry("docs/50%25 off.md", EntryKind::File, content)],
        };
        let current =
            Manifest::new(vec![entry("docs/50%25%20off.md", EntryKind::File, content)]).unwrap();

        legacy.validate().unwrap();
        assert_eq!(current.path_encoding, PathEncoding::PercentV2);
        assert_eq!(legacy.content_digest(), current.content_digest());
        let encoded = serde_json::to_value(&legacy).unwrap();
        assert!(encoded.get("path_encoding").is_none());
        assert_eq!(
            serde_json::from_value::<Manifest>(encoded)
                .unwrap()
                .path_encoding,
            PathEncoding::LegacyV1
        );
    }

    #[test]
    fn duplicate_paths_are_not_a_manifest() {
        let manifest = Manifest {
            path_encoding: PathEncoding::LegacyV1,
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
            path_encoding: PathEncoding::LegacyV1,
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
