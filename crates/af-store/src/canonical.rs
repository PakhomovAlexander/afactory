//! Canonical JSON (RFC 8785) and the two domain-separated digests.
//!
//! Identity is the whole point: two producers that emit the same content must land on the same
//! `content_id`, and the same content stored twice under different provenance must still yield
//! two distinct `artifact_id`s. Both fall out of hashing *canonical* bytes under distinct domain
//! separators.
//!
//! Scope of the number rules: payloads are admitted through [`af_contracts::json::admit`] first,
//! so integers are already inside the interoperable safe range and negative zero is already
//! refused. What remains is ECMAScript number formatting, and this implementation deliberately
//! **refuses** the range where that requires exponential notation (|x| >= 1e21, or nonzero
//! |x| < 1e-6) rather than guessing at it. A digest that is subtly wrong for large magnitudes is
//! worse than one that fails loudly, and the contracts represent such values as strings anyway.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::Read;

/// Domain separator for `content_id` — hashes the payload alone.
const DOMAIN_CONTENT: &[u8] = b"review.kernel/content-id/v1\0";
/// Domain separator for `artifact_id` — hashes the envelope, excluding `artifact_id` itself.
const DOMAIN_ARTIFACT: &[u8] = b"review.kernel/artifact-id/v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    /// A number outside the range this canonicalizer will format exactly.
    UnrepresentableNumber(String),
    /// Non-finite, or otherwise not admissible JSON.
    InvalidNumber(String),
}

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanonicalError::UnrepresentableNumber(n) => write!(
                f,
                "{n}: outside the exactly-formattable range; represent it as a canonical string"
            ),
            CanonicalError::InvalidNumber(n) => write!(f, "{n}: not an admissible JSON number"),
        }
    }
}

impl std::error::Error for CanonicalError {}

/// Serialize a value to RFC 8785 canonical JSON bytes.
pub fn canonicalize(value: &Value) -> Result<Vec<u8>, CanonicalError> {
    let _span = af_contracts::perf_trace::span(af_contracts::perf_trace::Phase::Canonicalize);
    let mut out = Vec::new();
    write_value(value, &mut out)?;
    Ok(out)
}

/// Infallible sinks keep canonical errors independent of the destination. Hashing and
/// comparison never allocate a second serialized document.
trait Sink {
    fn extend_from_slice(&mut self, bytes: &[u8]);
    fn push(&mut self, byte: u8) {
        self.extend_from_slice(&[byte]);
    }
}

impl Sink for Vec<u8> {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        Vec::extend_from_slice(self, bytes);
    }
}

struct FormatSink<'a, S>(&'a mut S);
impl<S: Sink> std::fmt::Write for FormatSink<'_, S> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0.extend_from_slice(text.as_bytes());
        Ok(())
    }
}

struct HashSink {
    hasher: Sha256,
    buffer: [u8; 1024],
    used: usize,
}
impl HashSink {
    fn new(domain: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain);
        Self {
            hasher,
            buffer: [0; 1024],
            used: 0,
        }
    }
    fn finish(mut self) -> String {
        self.hasher.update(&self.buffer[..self.used]);
        format!(
            "sha256:{}",
            af_contracts::hex::encode(&self.hasher.finalize())
        )
    }
}
impl Sink for HashSink {
    fn extend_from_slice(&mut self, mut bytes: &[u8]) {
        if self.used + bytes.len() > self.buffer.len() {
            self.hasher.update(&self.buffer[..self.used]);
            self.used = 0;
            if bytes.len() >= self.buffer.len() {
                self.hasher.update(bytes);
                bytes = &[];
            }
        }
        self.buffer[self.used..self.used + bytes.len()].copy_from_slice(bytes);
        self.used += bytes.len();
    }
}

struct CompareSink<'a> {
    remaining: &'a [u8],
    equal: bool,
}
impl Sink for CompareSink<'_> {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        if self.remaining.starts_with(bytes) {
            self.remaining = &self.remaining[bytes.len()..];
        } else {
            self.equal = false;
        }
    }
}

pub(crate) fn is_canonical(value: &Value, bytes: &[u8]) -> Result<bool, CanonicalError> {
    let mut sink = CompareSink {
        remaining: bytes,
        equal: true,
    };
    write_value(value, &mut sink)?;
    Ok(sink.equal && sink.remaining.is_empty())
}

// Static envelope fields are already in UTF-16 order. Borrow the payload rather than
// converting the whole envelope to a second Value just to omit artifact_id.
fn write_envelope(
    envelope: &af_contracts::ArtifactEnvelope,
    out: &mut impl Sink,
    include_id: bool,
) -> Result<(), CanonicalError> {
    out.push(b'{');
    if include_id {
        out.extend_from_slice(b"\"artifact_id\":");
        write_string(&envelope.artifact_id, out);
        out.push(b',');
    }
    out.extend_from_slice(b"\"content_id\":");
    write_string(&envelope.content_id, out);
    out.extend_from_slice(b",\"input_artifacts\":[");
    for (index, id) in envelope.input_artifacts.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        write_string(id, out);
    }
    out.extend_from_slice(b"],\"payload\":");
    write_value(&envelope.payload, out)?;
    out.extend_from_slice(b",\"producer\":");
    write_value(
        &serde_json::to_value(&envelope.producer).expect("producer serializes"),
        out,
    )?;
    if let Some(subject) = &envelope.subject_snapshot_id {
        out.extend_from_slice(b",\"subject_snapshot_id\":");
        write_string(subject, out);
    }
    out.extend_from_slice(b",\"type\":");
    write_string(&envelope.artifact_type, out);
    out.push(b'}');
    Ok(())
}

pub(crate) fn canonicalize_envelope(
    envelope: &af_contracts::ArtifactEnvelope,
) -> Result<Vec<u8>, CanonicalError> {
    let _span = af_contracts::perf_trace::span(af_contracts::perf_trace::Phase::Canonicalize);
    let mut bytes = Vec::new();
    write_envelope(envelope, &mut bytes, true)?;
    Ok(bytes)
}

fn write_value(value: &Value, out: &mut impl Sink) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => write_number(n, out)?,
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_value(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(fields) => {
            // Most parsed maps already have canonical order. Check it without assuming a
            // serde_json map implementation or allocating keys on that common path.
            if fields
                .keys()
                .zip(fields.keys().skip(1))
                .all(|(a, b)| utf16_cmp(a, b).is_le())
            {
                write_fields(fields.iter(), out)?;
            } else {
                // RFC 8785 orders by UTF-16, not UTF-8/scalar order for non-BMP keys.
                let mut keys: Vec<&String> = fields.keys().collect();
                keys.sort_by(|a, b| utf16_cmp(a, b));
                write_fields(keys.into_iter().map(|key| (key, &fields[key])), out)?;
            }
        }
    }
    Ok(())
}

fn write_fields<'a>(
    fields: impl IntoIterator<Item = (&'a String, &'a Value)>,
    out: &mut impl Sink,
) -> Result<(), CanonicalError> {
    out.push(b'{');
    for (index, (key, value)) in fields.into_iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        write_string(key, out);
        out.push(b':');
        write_value(value, out)?;
    }
    out.push(b'}');
    Ok(())
}

fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.cmp(b);
    }
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_number(n: &serde_json::Number, out: &mut impl Sink) -> Result<(), CanonicalError> {
    // Canonical sinks cannot return I/O errors, and integer Display is infallible.
    if let Some(i) = n.as_i64() {
        write!(FormatSink(out), "{i}").expect("integer formatting into canonical sink cannot fail");
        return Ok(());
    }
    if let Some(u) = n.as_u64() {
        write!(FormatSink(out), "{u}").expect("integer formatting into canonical sink cannot fail");
        return Ok(());
    }
    let f = n
        .as_f64()
        .ok_or_else(|| CanonicalError::InvalidNumber(n.to_string()))?;
    if !f.is_finite() {
        return Err(CanonicalError::InvalidNumber(n.to_string()));
    }
    if f == 0.0 {
        out.extend_from_slice(b"0");
        return Ok(());
    }
    let magnitude = f.abs();
    if !(1e-6..1e21).contains(&magnitude) {
        return Err(CanonicalError::UnrepresentableNumber(n.to_string()));
    }
    // Rust's Display for f64 is shortest-round-trip, and inside this range it agrees with
    // ECMAScript's Number::toString: no exponent, no trailing zeros, no "+".
    let text = format!("{f}");
    debug_assert!(!text.contains('e') && !text.contains('E'));
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

fn write_string(s: &str, out: &mut impl Sink) {
    out.push(b'"');
    // JSON escapes only these ASCII bytes. Preserve whole intervening UTF-8 spans instead
    // of decoding, encoding and extending the output separately for every scalar value.
    let bytes = s.as_bytes();
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte >= 0x20 && byte != b'"' && byte != b'\\' {
            continue;
        }
        out.extend_from_slice(&bytes[start..index]);
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            control => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(&[
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[usize::from(control >> 4)],
                    HEX[usize::from(control & 0x0f)],
                ]);
            }
        }
        start = index + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

fn digest(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    format!("sha256:{}", af_contracts::hex::encode(&hasher.finalize()))
}

/// `content_id` — the identity of the payload bytes alone.
pub fn content_id(payload: &Value) -> Result<String, CanonicalError> {
    let mut sink = HashSink::new(DOMAIN_CONTENT);
    write_value(payload, &mut sink)?;
    Ok(sink.finish())
}

/// `content_id` for an opaque blob (a diff, a log, a test artifact).
pub fn blob_content_id(bytes: &[u8]) -> String {
    digest(DOMAIN_CONTENT, bytes)
}

/// Stream an opaque blob into the same domain-separated identity without materializing it.
/// The caller owns the scratch, so a worker can reuse one allocation across every file it hashes.
pub fn blob_content_id_reader_with_buffer(
    mut reader: impl Read,
    buffer: &mut [u8],
) -> std::io::Result<(String, u64)> {
    if buffer.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "blob hash buffer must not be empty",
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_CONTENT);
    let mut size = 0u64;
    loop {
        let read = reader.read(buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("blob size exceeds u64"))?;
    }
    Ok((
        format!("sha256:{}", af_contracts::hex::encode(&hasher.finalize())),
        size,
    ))
}

/// `artifact_id` — the identity of the record: type, content, producer, exact inputs, subject.
///
/// Deliberately excludes `artifact_id` itself, and is computed over the same canonical form the
/// envelope serializes to, so an envelope round-tripped through JSON keeps its identity.
pub fn artifact_id(envelope: &af_contracts::ArtifactEnvelope) -> Result<String, CanonicalError> {
    let mut sink = HashSink::new(DOMAIN_ARTIFACT);
    write_envelope(envelope, &mut sink, false)?;
    Ok(sink.finish())
}

/// Validate both identities carried by a decoded artifact envelope.
pub fn validate_envelope(envelope: &af_contracts::ArtifactEnvelope) -> Result<(), String> {
    let _span = af_contracts::perf_trace::span(af_contracts::perf_trace::Phase::EnvelopeValidate);
    envelope.validate()?;
    let content = content_id(&envelope.payload).map_err(|error| error.to_string())?;
    if content != envelope.content_id {
        return Err("artifact envelope content_id disagrees with its payload".into());
    }
    let artifact = artifact_id(envelope).map_err(|error| error.to_string())?;
    if artifact != envelope.artifact_id {
        return Err("artifact envelope artifact_id disagrees with its provenance".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn streaming_sinks_match_materialized_bytes_and_borrowed_envelope_identity() {
        let values = [
            json!({"z": [null, true, false, -42, 0.000001, 0.9], "a": "é\n𐀀\u{0}\\\""}),
            json!({"\u{FF3A}": {"\u{1D400}": "nested"}, "\u{1D400}": [1, 2]}),
            json!({"padding": "x".repeat(32_769)}),
        ];
        for payload in values {
            let bytes = canonicalize(&payload).unwrap();
            assert_eq!(
                content_id(&payload).unwrap(),
                digest(DOMAIN_CONTENT, &bytes)
            );
            assert!(is_canonical(&payload, &bytes).unwrap());
            for damaged in [
                bytes[..bytes.len() - 1].to_vec(),
                [bytes.as_slice(), b" "].concat(),
                vec![b'x'; bytes.len()],
            ] {
                assert!(!is_canonical(&payload, &damaged).unwrap());
            }
            for producer in [
                af_contracts::Producer::KernelOperation {
                    run_id: "run".into(),
                    node_id: None,
                    operation_id: "op".into(),
                },
                af_contracts::Producer::KernelOperation {
                    run_id: "run".into(),
                    node_id: Some("node".into()),
                    operation_id: "op".into(),
                },
                af_contracts::Producer::Attempt {
                    run_id: "run".into(),
                    node_id: "node".into(),
                    attempt_id: "0".repeat(26),
                },
            ] {
                for subject in [None, Some(blob_content_id(b"subject"))] {
                    let envelope = af_contracts::ArtifactEnvelope {
                        artifact_type: "af/SinkProbe@1".into(),
                        artifact_id: blob_content_id(b"id"),
                        content_id: content_id(&payload).unwrap(),
                        producer: producer.clone(),
                        input_artifacts: vec![blob_content_id(b"input")],
                        subject_snapshot_id: subject,
                        payload: payload.clone(),
                    };
                    let mut value = serde_json::to_value(&envelope).unwrap();
                    let mut full = Vec::new();
                    write_envelope(&envelope, &mut full, true).unwrap();
                    assert_eq!(full, canonicalize(&value).unwrap());
                    value.as_object_mut().unwrap().remove("artifact_id");
                    assert_eq!(
                        artifact_id(&envelope).unwrap(),
                        digest(DOMAIN_ARTIFACT, &canonicalize(&value).unwrap())
                    );
                }
            }
        }
        let invalid = serde_json::from_str::<Value>("1e21").unwrap();
        assert_eq!(
            content_id(&invalid).unwrap_err(),
            canonicalize(&invalid).unwrap_err()
        );
        assert_eq!(
            is_canonical(&invalid, b"1e21").unwrap_err(),
            canonicalize(&invalid).unwrap_err()
        );
    }

    #[test]
    fn signed_and_unsigned_integers_retain_exact_canonical_bytes() {
        for text in [
            "0",
            "1",
            "-1",
            "9",
            "10",
            "-10",
            "255",
            "-9223372036854775808",
            "9223372036854775807",
            "9223372036854775808",
            "18446744073709551615",
        ] {
            let value: Value = serde_json::from_str(text).unwrap();
            assert_eq!(canonicalize(&value).unwrap(), text.as_bytes(), "{text}");
        }
        let nested = json!({"z": [0, -1, i64::MIN, {"u": u64::MAX}], "a": i64::MAX});
        assert_eq!(
            canonicalize(&nested).unwrap(),
            br#"{"a":9223372036854775807,"z":[0,-1,-9223372036854775808,{"u":18446744073709551615}]}"#
        );
    }

    #[test]
    fn every_unicode_scalar_retains_the_json_string_encoding() {
        let all: String = (0..=0x10ffff).filter_map(char::from_u32).collect();
        let value = Value::String(all);
        assert_eq!(
            canonicalize(&value).unwrap(),
            serde_json::to_vec(&value).unwrap()
        );
        let mixed = Value::String("é\n𐀀\u{0}中文\\\"\u{2028}\u{2029}/\tend".into());
        assert_eq!(
            canonicalize(&mixed).unwrap(),
            serde_json::to_vec(&mixed).unwrap()
        );
    }

    #[test]
    fn streaming_blob_identity_matches_the_in_memory_form() {
        let bytes = vec![0x5a; 200_000];
        let mut buffer = [0u8; 64 * 1024];
        let (streamed, size) =
            blob_content_id_reader_with_buffer(std::io::Cursor::new(&bytes), &mut buffer).unwrap();
        assert_eq!(streamed, blob_content_id(&bytes));
        assert_eq!(size, bytes.len() as u64);
    }

    #[test]
    fn members_are_ordered_and_whitespace_is_gone() {
        let value = json!({ "b": 1, "a": { "d": [1, 2], "c": true } });
        assert_eq!(
            String::from_utf8(canonicalize(&value).unwrap()).unwrap(),
            r#"{"a":{"c":true,"d":[1,2]},"b":1}"#
        );
    }

    #[test]
    fn member_order_follows_utf16_not_bytes() {
        // U+FF3A (BMP) vs U+1D400 (non-BMP, surrogate pair D835 DC00). In byte/scalar order the
        // non-BMP key sorts last; in UTF-16 code units it sorts FIRST, because 0xD835 < 0xFF3A.
        let value = json!({ "\u{FF3A}": 1, "\u{1D400}": 2 });
        let text = String::from_utf8(canonicalize(&value).unwrap()).unwrap();
        assert!(
            text.find('\u{1D400}') < text.find('\u{FF3A}'),
            "UTF-16 order violated: {text}"
        );
    }

    #[test]
    fn same_content_different_field_order_is_one_id() {
        let a: Value = serde_json::from_str(r#"{"x":1,"y":[2,3]}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"y":[2,3],"x":1}"#).unwrap();
        assert_eq!(content_id(&a).unwrap(), content_id(&b).unwrap());
    }

    #[test]
    fn content_and_artifact_domains_never_collide() {
        let payload = json!({ "a": 1 });
        let bytes = canonicalize(&payload).unwrap();
        assert_ne!(
            digest(DOMAIN_CONTENT, &bytes),
            digest(DOMAIN_ARTIFACT, &bytes)
        );
    }

    #[test]
    fn refuses_numbers_it_cannot_format_exactly() {
        let big: Value = serde_json::from_str("1e21").unwrap();
        assert!(matches!(
            content_id(&big),
            Err(CanonicalError::UnrepresentableNumber(_))
        ));
        let tiny: Value = serde_json::from_str("1e-7").unwrap();
        assert!(matches!(
            content_id(&tiny),
            Err(CanonicalError::UnrepresentableNumber(_))
        ));
        assert!(content_id(&serde_json::from_str::<Value>("0.9").unwrap()).is_ok());
    }

    #[test]
    fn control_characters_use_the_short_escapes() {
        let value = json!({ "k": "a\nb\tc\u{01}d\"e\\f" });
        assert_eq!(
            String::from_utf8(canonicalize(&value).unwrap()).unwrap(),
            r#"{"k":"a\nb\tc\u0001d\"e\\f"}"#
        );
    }
}
