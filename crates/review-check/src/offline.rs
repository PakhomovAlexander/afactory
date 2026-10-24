//! Offline checks (ADR-0207): a check declared `offline = true` runs with no network. npm reads
//! only its cache, every proxy variable names a closed local port, and Cargo stays offline. The
//! npm packages it runs are pinned by the SHA-256 of their registry tarballs; before each run
//! the kernel finds each tarball in npm's content-addressed cache (`_cacache`) and verifies it.
//! A cache that does not hold a pin yet is filled once, by `npm exec` with the network still
//! allowed, and verified then; after that the check never needs the network.

use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256, Sha512};

use crate::NpmPackagePin;

/// Where an offline check's proxy variables point: a local port nothing listens on, so a tool
/// that tries the network fails at once instead of waiting on a DNS lookup.
pub const OFFLINE_PROXY: &str = "http://127.0.0.1:9";

/// The most bytes a pinned tarball, or one cache index bucket, may hold.
const MAX_TARBALL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BUCKET_BYTES: u64 = 4 * 1024 * 1024;
/// How long filling the cache once may take, at most.
const FILL_TIMEOUT: Duration = Duration::from_secs(600);

/// The variables an offline check runs with, on top of its own.
pub fn offline_environment() -> Vec<(String, String)> {
    [
        ("npm_config_offline", "true"),
        ("npm_config_audit", "false"),
        ("npm_config_fund", "false"),
        ("npm_config_update_notifier", "false"),
        ("HTTP_PROXY", OFFLINE_PROXY),
        ("HTTPS_PROXY", OFFLINE_PROXY),
        ("ALL_PROXY", OFFLINE_PROXY),
        ("http_proxy", OFFLINE_PROXY),
        ("https_proxy", OFFLINE_PROXY),
        ("all_proxy", OFFLINE_PROXY),
        ("NO_PROXY", ""),
        ("no_proxy", ""),
        ("CARGO_NET_OFFLINE", "true"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect()
}

/// What npm's cache holds for one pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinState {
    /// The tarball is there and its SHA-256 is the pinned one.
    Verified,
    /// The cache does not hold the tarball.
    Absent,
    /// The cache holds other bytes under the package's tarball URL.
    Mismatch { actual: String },
}

/// Find `pin`'s registry tarball in the npm cache `cache` and compare its SHA-256 with the pin.
/// The index entry is found by its key, as npm's cache files it; the content by the entry's
/// SHA-512 integrity, which must match the bytes too.
pub fn verify_pin(cache: &Path, pin: &NpmPackagePin) -> Result<PinState, String> {
    let url = pin
        .tarball_url()
        .ok_or_else(|| format!("npm package `{}` is not an exact name@version", pin.spec))?;
    let key = format!("make-fetch-happen:request-cache:{url}");
    let bucket = hex(&Sha256::digest(key.as_bytes()));
    let index = cache
        .join("_cacache/index-v5")
        .join(&bucket[..2])
        .join(&bucket[2..4])
        .join(&bucket[4..]);
    let Some(lines) = read_bounded(&index, MAX_BUCKET_BYTES)? else {
        return Ok(PinState::Absent);
    };
    // The newest entry for the key wins, as npm reads it; a null integrity is a deletion.
    let integrity = String::from_utf8_lossy(&lines)
        .lines()
        .filter_map(|line| line.split_once('\t').map(|(_, entry)| entry))
        .filter_map(|entry| serde_json::from_str::<serde_json::Value>(entry).ok())
        .rfind(|entry| entry["key"] == key.as_str())
        .and_then(|entry| entry["integrity"].as_str().map(String::from));
    let Some(sha512) = integrity.as_deref().and_then(sha512_of_integrity) else {
        return Ok(PinState::Absent);
    };
    let content = cache
        .join("_cacache/content-v2/sha512")
        .join(&sha512[..2])
        .join(&sha512[2..4])
        .join(&sha512[4..]);
    let Some(bytes) = read_bounded(&content, MAX_TARBALL_BYTES)? else {
        return Ok(PinState::Absent);
    };
    if hex(&Sha512::digest(&bytes)) != sha512 {
        return Ok(PinState::Mismatch {
            actual: hex(&Sha256::digest(&bytes)),
        });
    }
    let actual = hex(&Sha256::digest(&bytes));
    Ok(if actual == pin.sha256 {
        PinState::Verified
    } else {
        PinState::Mismatch { actual }
    })
}

/// Make sure the npm cache `cache` holds every pin of an offline check, verified: fill it once
/// with `npm exec` (the network allowed, install scripts not run) when a pin is absent, under
/// `environment` (the check's own, without the offline variables) and at most `budget`. The
/// error is the refusal the check records instead of running; it names the package and what
/// to do.
pub fn prepare_offline(
    pins: &[NpmPackagePin],
    cache: &Path,
    environment: &[(String, String)],
    workdir: &Path,
    budget: Duration,
) -> Result<bool, String> {
    let mut filled = false;
    for pin in pins {
        let mut state = verify_pin(cache, pin)?;
        if state == PinState::Absent {
            fill(pin, cache, environment, workdir, budget.min(FILL_TIMEOUT))?;
            filled = true;
            state = verify_pin(cache, pin)?;
        }
        match state {
            PinState::Verified => {}
            PinState::Absent => {
                return Err(format!(
                    "offline check: npm did not put `{}` in its cache at {}; run the check \
                     once with the network up so af can fill the cache",
                    pin.spec,
                    cache.display()
                ));
            }
            PinState::Mismatch { actual } => {
                return Err(format!(
                    "offline check: the npm cache holds `{}` with SHA-256 {actual}, but the code \
                     policy pins {}; the registry served other bytes than the pin, so the check \
                     does not run. Check the pin in .af/code-policy.toml",
                    pin.spec, pin.sha256
                ));
            }
        }
    }
    Ok(filled)
}

fn fill(
    pin: &NpmPackagePin,
    cache: &Path,
    environment: &[(String, String)],
    workdir: &Path,
    bound: Duration,
) -> Result<(), String> {
    if bound.is_zero() {
        return Err(format!(
            "offline check: no time was left to fill the npm cache with `{}`",
            pin.spec
        ));
    }
    eprintln!(
        "offline check: filling the npm cache with {} once (the network is needed this time only)",
        pin.spec
    );
    let mut command = std::process::Command::new("npm");
    command
        .args(["exec", "--yes", "--ignore-scripts"])
        .arg(format!("--package={}", pin.spec))
        .args(["--", "true"])
        .current_dir(workdir)
        .env_clear()
        .envs(environment.iter().map(|(key, value)| (key, value)))
        .env("npm_config_cache", cache)
        .env("npm_config_update_notifier", "false")
        .env("npm_config_audit", "false")
        .env("npm_config_fund", "false");
    let captured = review_process::run_supervised_captured_with_policy(
        &mut command,
        None,
        bound,
        review_process::ExitPolicy::KillProcessGroup,
    );
    match captured.status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "offline check: `npm exec --package={}` exited with {status} while filling the npm \
             cache: {}; run the check again with the network up",
            pin.spec,
            tail(&captured.stderr)
        )),
        Err(error) => Err(format!(
            "offline check: filling the npm cache with `{}` did not complete: {error}; run the \
             check again with the network up",
            pin.spec
        )),
    }
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().rev().find(|line| !line.trim().is_empty());
    line.unwrap_or("no output")
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect()
}

/// The whole regular file at `path`, links not followed; `None` when absent.
fn read_bounded(path: &Path, max: u64) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= max => {
            std::fs::read(path)
                .map(Some)
                .map_err(|error| format!("reading the npm cache at {}: {error}", path.display()))
        }
        Ok(_) => Err(format!(
            "the npm cache entry {} is not a regular file of at most {max} bytes",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "inspecting the npm cache at {}: {error}",
            path.display()
        )),
    }
}

/// The hex SHA-512 of an SRI integrity string's first `sha512-` hash.
fn sha512_of_integrity(integrity: &str) -> Option<String> {
    let encoded = integrity
        .split_whitespace()
        .find_map(|hash| hash.strip_prefix("sha512-"))?;
    let bytes = base64_decode(encoded.split('?').next()?)?;
    (bytes.len() == 64).then(|| hex(&bytes))
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let trimmed = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let (mut buffer, mut bits) = (0_u32, 0);
    for byte in trimmed.bytes() {
        buffer = (buffer << 6) | value(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Write `tarball` into the npm cache `cache` as npm files the registry response for `pin`'s
/// URL: the content by its SHA-512, one index entry by the key. What a filled cache holds for a
/// pin, for tests and fixtures.
pub fn cache_tarball(cache: &Path, pin: &NpmPackagePin, tarball: &[u8]) -> Result<(), String> {
    let url = pin
        .tarball_url()
        .ok_or_else(|| format!("npm package `{}` is not an exact name@version", pin.spec))?;
    let key = format!("make-fetch-happen:request-cache:{url}");
    let sha512 = Sha512::digest(tarball);
    let content_hex = hex(&sha512);
    let content = cache
        .join("_cacache/content-v2/sha512")
        .join(&content_hex[..2])
        .join(&content_hex[2..4]);
    let write = |path: &Path, bytes: &[u8]| {
        std::fs::create_dir_all(path.parent().unwrap_or(path))
            .and_then(|()| std::fs::write(path, bytes))
            .map_err(|error| format!("writing {}: {error}", path.display()))
    };
    write(&content.join(&content_hex[4..]), tarball)?;
    let integrity = format!("sha512-{}", base64_encode(&sha512));
    let entry = serde_json::json!({
        "key": key,
        "integrity": integrity,
        "time": 0,
        "size": tarball.len(),
        "metadata": {"url": url}
    })
    .to_string();
    let bucket = hex(&Sha256::digest(key.as_bytes()));
    let index = cache
        .join("_cacache/index-v5")
        .join(&bucket[..2])
        .join(&bucket[2..4])
        .join(&bucket[4..]);
    // npm prefixes each entry with its SHA-1; a reader that verifies it is npm's, not af's.
    write(&index, format!("0\t{entry}\n").as_bytes())
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(value >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(sha256: &str) -> NpmPackagePin {
        NpmPackagePin {
            spec: "markdownlint-cli2@0.22.1".into(),
            sha256: sha256.into(),
        }
    }

    #[test]
    fn a_pin_is_an_exact_package_and_a_tarball_url() {
        assert_eq!(
            pin("").tarball_url().unwrap(),
            "https://registry.npmjs.org/markdownlint-cli2/-/markdownlint-cli2-0.22.1.tgz"
        );
        let scoped = NpmPackagePin {
            spec: "@scope/tool@1.0.0-rc.1".into(),
            sha256: "a".repeat(64),
        };
        scoped.validate().unwrap();
        assert_eq!(
            scoped.tarball_url().unwrap(),
            "https://registry.npmjs.org/@scope/tool/-/tool-1.0.0-rc.1.tgz"
        );
        for spec in [
            "markdownlint-cli2",
            "x@^1.0.0",
            "x@latest",
            "x@1.0",
            "X@1.0.0",
            "@s@1.0.0",
        ] {
            assert!(
                NpmPackagePin {
                    spec: spec.into(),
                    sha256: "a".repeat(64)
                }
                .validate()
                .is_err(),
                "{spec}"
            );
        }
        assert!(pin("ABC").validate().is_err());
    }

    #[test]
    fn a_cached_tarball_is_found_by_its_key_and_verified_by_its_sha256() {
        let cache = tempfile::tempdir().unwrap();
        let tarball = b"tarball bytes".as_slice();
        let sha = hex(&Sha256::digest(tarball));
        assert_eq!(
            verify_pin(cache.path(), &pin(&sha)).unwrap(),
            PinState::Absent
        );
        cache_tarball(cache.path(), &pin(&sha), tarball).unwrap();
        assert_eq!(
            verify_pin(cache.path(), &pin(&sha)).unwrap(),
            PinState::Verified
        );
        assert_eq!(
            verify_pin(cache.path(), &pin(&"0".repeat(64))).unwrap(),
            PinState::Mismatch {
                actual: sha.clone()
            }
        );
        // Bytes changed under the index's integrity are not the pinned tarball either.
        let sha512 = hex(&Sha512::digest(tarball));
        std::fs::write(
            cache
                .path()
                .join("_cacache/content-v2/sha512")
                .join(&sha512[..2])
                .join(&sha512[2..4])
                .join(&sha512[4..]),
            b"other",
        )
        .unwrap();
        assert!(matches!(
            verify_pin(cache.path(), &pin(&sha)).unwrap(),
            PinState::Mismatch { .. }
        ));
    }

    #[test]
    fn base64_round_trips() {
        for bytes in [&b""[..], b"f", b"fo", b"foo", b"foob", &[0xff; 64]] {
            assert_eq!(base64_decode(&base64_encode(bytes)).unwrap(), bytes);
        }
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn the_offline_environment_closes_every_proxy_and_keeps_npm_and_cargo_offline() {
        let environment = offline_environment();
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "ALL_PROXY",
        ] {
            assert!(
                environment.contains(&(key.into(), OFFLINE_PROXY.into())),
                "{key}"
            );
        }
        assert!(environment.contains(&("npm_config_offline".into(), "true".into())));
        assert!(environment.contains(&("CARGO_NET_OFFLINE".into(), "true".into())));
    }
}
