//! Lowercase hex for digest bytes.
//!
//! Digest identities — Attempt IDs, `sha256:` artifact addresses, receipt prefixes — are
//! persisted and compared across releases, so their spelling is part of the on-disk contract.
//! `sha2` 0.10 spelled them through `LowerHex` on the digest's `GenericArray`; 0.11 returns a
//! `hybrid-array` `Array`, which implements no such thing. [`encode`] reproduces that spelling
//! byte-for-byte so a dependency bump cannot silently rename stored identities.

use std::fmt::Write;

/// Encode `bytes` as lowercase hex, two digits per byte, no separators or prefix.
pub fn encode(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut encoded, byte| {
            // Writing into a String is infallible; the Result exists only to satisfy `Write`.
            let _ = write!(encoded, "{byte:02x}");
            encoded
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_is_two_lowercase_digits_including_the_leading_zero() {
        assert_eq!(encode(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }

    #[test]
    fn an_empty_digest_encodes_to_an_empty_string() {
        assert_eq!(encode(&[]), "");
    }

    #[test]
    fn a_sha256_digest_keeps_the_spelling_sha2_0_10_produced() {
        // The SHA-256 of the empty input, as `format!("{:x}", Sha256::digest(b""))` spelled it.
        let empty_sha256 = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(
            encode(&empty_sha256),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
