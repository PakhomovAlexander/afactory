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

/// One source line (its body without the `\n`) redacted: a body longer than `max` bytes is
/// withheld whole as [`redact::OVERSIZED_LINE`] before it is converted or judged at all.
/// Returns the redacted body, whether it was withheld whole, and how many lines were withheld.
fn redact_line(body: &[u8], max: usize, redactor: &Redactor) -> (String, bool, u32) {
    if body.len() > max {
        return (redact::OVERSIZED_LINE.to_string(), true, 1);
    }
    let (line, count) = redactor.text(&String::from_utf8_lossy(body));
    (line, false, count)
}

/// The lines of `source`: each body and whether a `\n` ends it.
fn source_lines(source: &[u8]) -> impl Iterator<Item = (&[u8], bool)> {
    source
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| match line.strip_suffix(b"\n") {
            Some(body) => (body, true),
            None => (line, false),
        })
}

/// Text redacted per line, then cut to `max` bytes head and tail. A line longer than `max` is
/// withheld whole as [`redact::OVERSIZED_LINE`]. Nothing proportional to the source is
/// allocated: a first pass measures the redacted text line by line, and a second keeps only
/// what the head and the tail need, so what is held besides the source is bounded by `max` and
/// one line at most `max` long.
fn bounded_text(source: &[u8], max: usize, redactor: &Redactor) -> (String, bool, bool, u32) {
    let mut total = 0;
    let mut withheld = 0;
    let mut withheld_whole = false;
    for (body, newline) in source_lines(source) {
        let (line, whole, count) = redact_line(body, max, redactor);
        total += line.len() + usize::from(newline);
        withheld += count;
        withheld_whole |= whole;
    }
    if total <= max {
        let mut redacted = String::with_capacity(total);
        for (body, newline) in source_lines(source) {
            redacted.push_str(&redact_line(body, max, redactor).0);
            if newline {
                redacted.push('\n');
            }
        }
        return (redacted, false, withheld_whole, withheld);
    }
    // Room for the marker, with the widest count it can carry.
    let marker_room = truncation_marker(total).len();
    let keep = max.saturating_sub(marker_room);
    let head_budget = keep / 2;
    let tail_from = total - (keep - keep / 2);
    let (mut head, mut tail) = (String::new(), String::new());
    let (mut head_end, mut tail_start) = (0, total);
    let mut offset = 0;
    for (body, newline) in source_lines(source) {
        let mut line = redact_line(body, max, redactor).0;
        if newline {
            line.push('\n');
        }
        let (start, end) = (offset, offset + line.len());
        offset = end;
        if start >= head_budget && end <= tail_from {
            continue;
        }
        if start < head_budget {
            let at = floor_boundary(&line, head_budget - start);
            head.push_str(&line[..at]);
            head_end = start + at;
        }
        if end > tail_from {
            let at = ceil_boundary(&line, tail_from.saturating_sub(start));
            if tail.is_empty() {
                tail_start = start + at;
            }
            tail.push_str(&line[at..]);
        }
    }
    let bounded = format!("{head}{}{tail}", truncation_marker(tail_start - head_end));
    (bounded, true, withheld_whole, withheld)
}

/// The retained bytes of a JSON source withheld whole: the JSON string [`redact::WITHHELD`].
fn withheld_json() -> Vec<u8> {
    serde_json::to_vec(redact::WITHHELD).expect("a string serializes")
}

/// Redact, bound, digest and compress `source` under the current redaction version.
///
/// The bound is judged before anything is parsed: a JSON source longer than `max_blob_bytes`
/// is one document too long to be judged whole and becomes the withheld value at once, whether
/// it is well-formed or not, with nothing deserialized or allocated for it.
pub fn prepare(source: &[u8], media: Media, max_blob_bytes: u64, redactor: &Redactor) -> Prepared {
    let max = usize::try_from(max_blob_bytes).unwrap_or(usize::MAX);
    let (retained, truncated, withheld_whole, redacted, media) = match media {
        Media::Json if source.len() > max => (withheld_json(), false, true, 1, Media::Json),
        Media::Json => match serde_json::from_slice::<serde_json::Value>(source) {
            Ok(mut document) => {
                let redacted = redactor.json(&mut document);
                let bytes = serde_json::to_vec(&document).expect("a value serializes");
                if bytes.len() > max {
                    (withheld_json(), false, true, redacted + 1, Media::Json)
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
    // Never through a link: a prefix directory linked elsewhere refuses the blob.
    layout.create_dirs(parent)?;
    let temp = parent.join(layout::temp_name(layout::BLOB_TEMP_LABEL));
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

/// How a staged blob came to be in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    /// This blob's temporary file was renamed into place.
    Renamed,
    /// A file of the digest with the same size was already there and is reused.
    Reused,
}

/// Put a staged blob in place: an existing file of the digest with the same size is reused and
/// the temporary file removed; anything else is replaced by the rename.
pub fn place(staged: &Staged) -> std::io::Result<Placed> {
    // Judged without following a link: a link in a blob's place is replaced, never reused.
    match std::fs::symlink_metadata(&staged.path) {
        Ok(existing) if existing.is_file() && existing.len() == staged.stored_bytes => {
            std::fs::remove_file(&staged.temp)?;
            Ok(Placed::Reused)
        }
        _ => {
            std::fs::rename(&staged.temp, &staged.path)?;
            Ok(Placed::Renamed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text bound as one whole redacted string would give it: the two-pass bound must
    /// retain exactly these bytes.
    fn whole_then_cut(source: &[u8], max: usize, redactor: &Redactor) -> String {
        let mut redacted = String::new();
        for (body, newline) in source_lines(source) {
            redacted.push_str(&redact_line(body, max, redactor).0);
            if newline {
                redacted.push('\n');
            }
        }
        if redacted.len() <= max {
            return redacted;
        }
        let keep = max.saturating_sub(truncation_marker(redacted.len()).len());
        let head_end = floor_boundary(&redacted, keep / 2);
        let tail_start = ceil_boundary(&redacted, redacted.len() - (keep - keep / 2));
        format!(
            "{}{}{}",
            &redacted[..head_end],
            truncation_marker(tail_start - head_end),
            &redacted[tail_start..]
        )
    }

    #[test]
    fn the_two_pass_text_bound_retains_what_the_whole_text_would() {
        let redactor = Redactor::new().with_home("/home/violet");
        let mut source = String::new();
        for line in 0..300 {
            match line % 5 {
                0 => source.push_str(&format!("é line {line} ünïcode\n")),
                1 => source.push_str("password = pw\n"),
                2 => source.push_str("/home/violet/project/src\n"),
                3 => source.push_str(&"x".repeat(line)),
                _ => source.push('\n'),
            }
        }
        for max in [64, 100, 257, 1000, 4096, 9000, 100_000] {
            let (bounded, truncated, _, _) = bounded_text(source.as_bytes(), max, &redactor);
            assert_eq!(
                bounded,
                whole_then_cut(source.as_bytes(), max, &redactor),
                "{max}"
            );
            assert_eq!(truncated, bounded.contains("bytes truncated"), "{max}");
            assert!(!bounded.contains("pw\n"), "{max}");
            assert!(bounded.len() <= max.max(64), "{max}: {}", bounded.len());
        }
    }

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
