//! Content-addressed blobs (design §6): the shared object and its occurrences.
//!
//! A blob's identity is the digest of what is stored. The source is redacted whole under the
//! current redaction version first — line by line for text, as one document for JSON — and only
//! then, when needed, truncated to `content_max_blob_bytes` (head and tail halves), so a bound
//! never removes the assignment name or the JSON structure a credential is judged by. A single
//! line or JSON document longer than `content_max_blob_bytes` cannot be judged whole and is
//! withheld whole. The digest is `SHA-256` of the retained bytes before compression, so the same
//! source under another bound or redaction version is another object, never a wrong reuse.
//!
//! Files are written to a temporary file under `blobs/` and renamed into place; an existing file
//! of the digest is reused after its size is checked. The publication protocol that orders the
//! rename against the reference is the writer's ([`crate::segment::writer`]); readers hold
//! `gc.lock` shared ([`reader`]); the collector takes it exclusively ([`collect`]).

pub mod collect;
pub mod reader;

use std::io::Write as _;
use std::path::PathBuf;

use flate2::Compression;
use flate2::GzBuilder;

use crate::layout::{self, Layout};
use crate::redact::{self, Redactor};
use crate::trace::sha256_digest;

/// How content is redacted: per line, or as one JSON document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Text,
    Json,
}

impl Media {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Text => "text/plain; charset=utf-8",
            Self::Json => "application/json",
        }
    }
}

/// Content redacted, bounded, digested and compressed, ready to stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The retained, redacted bytes.
    pub retained: Vec<u8>,
    /// `sha256:` over `retained`: the object's identity.
    pub digest: String,
    /// `sha256:` over the original bytes.
    pub source_digest: String,
    pub original_bytes: u64,
    /// Cut head and tail to the bound.
    pub truncated: bool,
    /// Withheld whole: a line or document too long to be judged.
    pub withheld_whole: bool,
    /// How many lines or values the redaction withheld.
    pub redacted: u32,
    pub media_type: &'static str,
    /// `retained`, gzip-compressed, deterministic for the same bytes.
    pub compressed: Vec<u8>,
}

/// The marker between the head and the tail of truncated text.
pub fn truncation_marker(cut: usize) -> String {
    format!("\n[… {cut} bytes truncated …]\n")
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// Text redacted per line, then cut to `max` bytes head and tail. A line longer than `max` is
/// withheld whole as [`redact::OVERSIZED_LINE`].
fn bounded_text(source: &[u8], max: usize, redactor: &Redactor) -> (String, bool, bool, u32) {
    let text = String::from_utf8_lossy(source);
    let mut redacted = String::with_capacity(text.len().min(max + 64));
    let mut withheld = 0;
    let mut withheld_whole = false;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        let end = &line[body.len()..];
        if body.len() > max {
            redacted.push_str(redact::OVERSIZED_LINE);
            withheld += 1;
            withheld_whole = true;
        } else {
            let (line, count) = redactor.text(body);
            redacted.push_str(&line);
            withheld += count;
        }
        redacted.push_str(end);
    }
    if redacted.len() <= max {
        return (redacted, false, withheld_whole, withheld);
    }
    // Room for the marker, with the widest count it can carry.
    let marker_room = truncation_marker(redacted.len()).len();
    let keep = max.saturating_sub(marker_room);
    let head_end = floor_boundary(&redacted, keep / 2);
    let tail_start = ceil_boundary(&redacted, redacted.len() - (keep - keep / 2));
    let cut = tail_start - head_end;
    let bounded = format!(
        "{}{}{}",
        &redacted[..head_end],
        truncation_marker(cut),
        &redacted[tail_start..]
    );
    (bounded, true, withheld_whole, withheld)
}

/// Redact, bound, digest and compress `source` under the current redaction version.
pub fn prepare(source: &[u8], media: Media, max_blob_bytes: u64, redactor: &Redactor) -> Prepared {
    let max = usize::try_from(max_blob_bytes).unwrap_or(usize::MAX);
    let (retained, truncated, withheld_whole, redacted, media) = match media {
        Media::Json => match serde_json::from_slice::<serde_json::Value>(source) {
            Ok(_) if source.len() > max => (
                serde_json::to_vec(redact::WITHHELD).expect("a string serializes"),
                false,
                true,
                1,
                Media::Json,
            ),
            Ok(mut document) => {
                let redacted = redactor.json(&mut document);
                let bytes = serde_json::to_vec(&document).expect("a value serializes");
                if bytes.len() > max {
                    (
                        serde_json::to_vec(redact::WITHHELD).expect("a string serializes"),
                        false,
                        true,
                        redacted + 1,
                        Media::Json,
                    )
                } else {
                    (bytes, false, false, redacted, Media::Json)
                }
            }
            // Not JSON after all: judged as text, per line.
            Err(_) => {
                let (text, truncated, whole, redacted) = bounded_text(source, max, redactor);
                (text.into_bytes(), truncated, whole, redacted, Media::Text)
            }
        },
        Media::Text => {
            let (text, truncated, whole, redacted) = bounded_text(source, max, redactor);
            (text.into_bytes(), truncated, whole, redacted, Media::Text)
        }
    };
    let compressed = compress(&retained);
    Prepared {
        digest: sha256_digest(&retained),
        source_digest: sha256_digest(source),
        original_bytes: source.len() as u64,
        truncated,
        withheld_whole,
        redacted,
        media_type: media.media_type(),
        compressed,
        retained,
    }
}

/// gzip with no name and no time in the header, so equal bytes compress to equal files.
pub fn compress(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzBuilder::new().mtime(0).write(
        Vec::with_capacity(bytes.len() / 2 + 64),
        Compression::default(),
    );
    encoder.write_all(bytes).expect("writing to memory");
    encoder.finish().expect("writing to memory")
}

/// A blob written to its temporary file, not yet in place.
#[derive(Debug)]
pub struct Staged {
    pub temp: PathBuf,
    pub path: PathBuf,
    pub stored_bytes: u64,
}

/// Write a prepared blob to a temporary file beside its final place.
pub fn stage(layout: &Layout, prepared: &Prepared) -> std::io::Result<Staged> {
    let path = layout.blob(&prepared.digest);
    let parent = path.parent().expect("a blob has a directory");
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(layout::temp_name("blob"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(&prepared.compressed)?;
    Ok(Staged {
        temp,
        path,
        stored_bytes: prepared.compressed.len() as u64,
    })
}

/// Put a staged blob in place: an existing file of the digest with the same size is reused and
/// the temporary file removed; anything else is replaced by the rename.
pub fn place(staged: &Staged) -> std::io::Result<()> {
    match std::fs::metadata(&staged.path) {
        Ok(existing) if existing.is_file() && existing.len() == staged.stored_bytes => {
            std::fs::remove_file(&staged.temp)
        }
        _ => std::fs::rename(&staged.temp, &staged.path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_bytes_compress_to_equal_files() {
        assert_eq!(compress(b"same"), compress(b"same"));
    }

    #[test]
    fn truncated_text_keeps_head_and_tail_within_the_bound() {
        let source: String = (0..2000).map(|line| format!("line {line}\n")).collect();
        let prepared = prepare(source.as_bytes(), Media::Text, 1024, &Redactor::new());
        assert!(prepared.truncated);
        assert!(
            prepared.retained.len() <= 1024,
            "{}",
            prepared.retained.len()
        );
        let text = String::from_utf8(prepared.retained).unwrap();
        assert!(text.starts_with("line 0\n"));
        assert!(text.ends_with("line 1999\n"));
        assert!(text.contains("bytes truncated"));
    }
}
