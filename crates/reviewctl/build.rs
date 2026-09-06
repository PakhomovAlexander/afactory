//! Build metadata for `af --version --json` and `af self`: the target triple, the git commit, and
//! the release public key. No build date — identical inputs must give identical binaries.

use std::path::Path;
use std::process::Command;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=AF_TARGET={target}");
    let commit = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=AF_GIT_COMMIT={commit}");

    // The minisign public key the release job signs `SHA256SUMS` with (`keys/release.pub`). A
    // build without the file carries no key and verifies checksums only; the release workflow
    // refuses to publish such a build.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let key_file = Path::new(&manifest_dir).join("keys/release.pub");
    let key = std::fs::read_to_string(&key_file)
        .ok()
        .and_then(|text| {
            text.lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
                .map(str::to_string)
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=AF_RELEASE_KEY={key}");
    println!("cargo:rerun-if-changed=keys");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=build.rs");
}
