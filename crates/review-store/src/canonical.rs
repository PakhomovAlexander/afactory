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
use std::io::{Read, Write};

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
    let _span = review_core::perf_trace::span(review_core::perf_trace::Phase::Canonicalize);
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
    if a.is_ascii() && b.is_ascii() {
        return a.cmp(b);
    }
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_number(n: &serde_json::Number, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
    if let Some(i) = n.as_i64() {
        write!(out, "{i}").expect("writing to a Vec cannot fail");
        return Ok(());
    }
    if let Some(u) = n.as_u64() {
        write!(out, "{u}").expect("writing to a Vec cannot fail");
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
    format!("sha256:{}", review_core::hex::encode(&hasher.finalize()))
}

/// `content_id` — the identity of the payload bytes alone.
pub fn content_id(payload: &Value) -> Result<String, CanonicalError> {
    Ok(digest(DOMAIN_CONTENT, &canonicalize(payload)?))
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
        format!("sha256:{}", review_core::hex::encode(&hasher.finalize())),
        size,
    ))
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

/// Validate both identities carried by a decoded artifact envelope.
pub fn validate_envelope(envelope: &review_core::ArtifactEnvelope) -> Result<(), String> {
    let _span = review_core::perf_trace::span(review_core::perf_trace::Phase::EnvelopeValidate);
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
    fn every_byte_value_retains_exact_integer_encoding() {
        let value = Value::Array((0u8..=u8::MAX).map(|n| Value::Number(n.into())).collect());
        let expected = concat!(
            "[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,",
            "30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,",
            "60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,",
            "90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,",
            "110,111,112,113,114,115,116,117,118,119,120,121,122,123,124,125,126,127,128,129,",
            "130,131,132,133,134,135,136,137,138,139,140,141,142,143,144,145,146,147,148,149,",
            "150,151,152,153,154,155,156,157,158,159,160,161,162,163,164,165,166,167,168,169,",
            "170,171,172,173,174,175,176,177,178,179,180,181,182,183,184,185,186,187,188,189,",
            "190,191,192,193,194,195,196,197,198,199,200,201,202,203,204,205,206,207,208,209,",
            "210,211,212,213,214,215,216,217,218,219,220,221,222,223,224,225,226,227,228,229,",
            "230,231,232,233,234,235,236,237,238,239,240,241,242,243,244,245,246,247,248,249,",
            "250,251,252,253,254,255]"
        );
        assert_eq!(canonicalize(&value).unwrap(), expected.as_bytes());
    }

    #[test]
    fn directly_constructed_integer_boundaries_retain_exact_encoding() {
        // canonicalize accepts these Numbers directly, even outside json::admit's safe range.
        let signed = [
            (i64::MIN, "-9223372036854775808"),
            (i64::MIN + 1, "-9223372036854775807"),
            (-9_007_199_254_740_993, "-9007199254740993"),
            (-9_007_199_254_740_992, "-9007199254740992"),
            (-9_007_199_254_740_991, "-9007199254740991"),
            (-10, "-10"),
            (-1, "-1"),
            (0, "0"),
            (1, "1"),
            (10, "10"),
            (9_007_199_254_740_991, "9007199254740991"),
            (9_007_199_254_740_992, "9007199254740992"),
            (9_007_199_254_740_993, "9007199254740993"),
            (i64::MAX - 1, "9223372036854775806"),
            (i64::MAX, "9223372036854775807"),
        ];
        for (number, expected) in signed {
            let value = Value::Number(number.into());
            assert_eq!(canonicalize(&value).unwrap(), expected.as_bytes());
        }
        let unsigned = [
            (0, "0"),
            (1, "1"),
            (9_007_199_254_740_991, "9007199254740991"),
            (9_007_199_254_740_992, "9007199254740992"),
            (9_007_199_254_740_993, "9007199254740993"),
            (9_223_372_036_854_775_807, "9223372036854775807"),
            (9_223_372_036_854_775_808, "9223372036854775808"),
            (u64::MAX - 1, "18446744073709551614"),
            (u64::MAX, "18446744073709551615"),
        ];
        for (number, expected) in unsigned {
            let value = Value::Number(number.into());
            assert_eq!(canonicalize(&value).unwrap(), expected.as_bytes());
        }
    }

    #[test]
    fn nested_mixed_integers_retain_exact_encoding() {
        let value = json!({
            "z": u64::MAX,
            "a": [
                i64::MIN, 0, 255,
                { "u": 9_223_372_036_854_775_808u64, "s": -9_007_199_254_740_993i64 },
                -1, 1.5, 1.0, -0.0, true, null, "7"
            ]
        });
        let expected = concat!(
            r#"{"a":[-9223372036854775808,0,255,{"s":-9007199254740993,"u":9223372036854775808},"#,
            r#"-1,1.5,1,0,true,null,"7"],"z":18446744073709551615}"#
        );
        assert_eq!(canonicalize(&value).unwrap(), expected.as_bytes());
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
