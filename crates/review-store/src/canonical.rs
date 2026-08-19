//! Canonical JSON (RFC 8785) and the two domain-separated digests.
//!
//! Identity is the whole point: two producers that emit the same content must land on the same
//! `content_id`, and the same content stored twice under different provenance must still yield
//! two distinct `artifact_id`s. Both fall out of hashing *canonical* bytes under distinct domain
//! separators.
//!
//! Scope of the number rules: payloads are admitted through [`review_core::json::admit`] first,
//! so integers are already inside the interoperable safe range and negative zero is already
//! refused. What remains is ECMAScript number formatting, and this implementation deliberately
//! **refuses** the range where that requires exponential notation (|x| >= 1e21, or nonzero
//! |x| < 1e-6) rather than guessing at it. A digest that is subtly wrong for large magnitudes is
//! worse than one that fails loudly, and the contracts represent such values as strings anyway.

use serde_json::Value;
use sha2::{Digest, Sha256};

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
    let mut out = Vec::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
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
            // RFC 8785 orders members by their UTF-16 code units, which is NOT byte order for
            // anything outside the BMP. Sorting by `str` would silently disagree on such keys.
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            out.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                write_value(&fields[key], out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_number(n: &serde_json::Number, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
    if let Some(i) = n.as_i64() {
        out.extend_from_slice(i.to_string().as_bytes());
        return Ok(());
    }
    if let Some(u) = n.as_u64() {
        out.extend_from_slice(u.to_string().as_bytes());
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

fn write_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{08}' => out.extend_from_slice(b"\\b"),
            '\u{0c}' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

fn digest(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// `content_id` — the identity of the payload bytes alone.
pub fn content_id(payload: &Value) -> Result<String, CanonicalError> {
    Ok(digest(DOMAIN_CONTENT, &canonicalize(payload)?))
}

/// `content_id` for an opaque blob (a diff, a log, a test artifact).
pub fn blob_content_id(bytes: &[u8]) -> String {
    digest(DOMAIN_CONTENT, bytes)
}

/// `artifact_id` — the identity of the record: type, content, producer, exact inputs, subject.
///
/// Deliberately excludes `artifact_id` itself, and is computed over the same canonical form the
/// envelope serializes to, so an envelope round-tripped through JSON keeps its identity.
pub fn artifact_id(envelope: &review_core::ArtifactEnvelope) -> Result<String, CanonicalError> {
    let mut value = serde_json::to_value(envelope).expect("envelope serializes");
    if let Some(object) = value.as_object_mut() {
        object.remove("artifact_id");
    }
    Ok(digest(DOMAIN_ARTIFACT, &canonicalize(&value)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
