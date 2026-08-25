//! Lossless repository-relative path rendering shared by source artifacts and Report readers.

/// Render raw repository-relative path bytes losslessly for JSON.
///
/// Valid UTF-8 without a percent sign stays human-readable. Every other path uses an
/// unambiguous percent encoding, including a literal `%`.
pub fn encode_path(bytes: &[u8]) -> String {
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

/// Recover the raw path bytes named by [`encode_path`].
pub fn decode_path(encoded: &str) -> Vec<u8> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 3 <= bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    decoded
}

/// Whether a JSON path is a canonical repository-relative path.
///
/// This is the semantic rule shared by Change Sets and Report locations. A malformed Report
/// path must be refused at admission; treating it as merely absent from a Change Set would turn
/// a spelling error into an `out` Scope and could make a real blocker stop blocking.
pub fn is_valid_repo_path(path: &str) -> bool {
    !path.is_empty()
        && path == path.trim()
        && !path.starts_with('/')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|component| !matches!(component, "" | "." | ".."))
}

/// Match a Report spelling against a sorted set of losslessly encoded repository paths.
pub fn contains_report_path(changed_paths: &[String], report_path: &str) -> bool {
    is_valid_repo_path(report_path)
        && (changed_paths
            .binary_search_by(|path| path.as_str().cmp(report_path))
            .is_ok()
            || {
                let encoded = encode_path(report_path.as_bytes());
                encoded != report_path && changed_paths.binary_search(&encoded).is_ok()
            })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_is_lossless_and_literal_percents_are_unambiguous() {
        for raw in [
            &b"src/ok.rs"[..],
            b"docs/50%-off.md",
            &[b'a', 0xff, b'b'],
            b"caf\xc3\xa9.rs",
            b"weird#name",
            b"100%",
        ] {
            assert_eq!(decode_path(&encode_path(raw)), raw);
        }
        assert_eq!(encode_path(b"docs/50%-off.md"), "docs/50%25-off.md");
        assert_eq!(encode_path(&[b'a', 0xff, b'b']), "a%FFb");
    }

    #[test]
    fn repository_paths_are_relative_and_canonical() {
        for valid in ["src/a.rs", "docs/50%-off.md", "a\\b", ".../x"] {
            assert!(is_valid_repo_path(valid), "{valid:?}");
        }
        for invalid in [
            "",
            "/src/a.rs",
            "./src/a.rs",
            "src//a.rs",
            "src/../a.rs",
            " src/a.rs",
            "src/a.rs ",
        ] {
            assert!(!is_valid_repo_path(invalid), "{invalid:?}");
        }
    }
}
