//! Every compile-time `CARGO_MANIFEST_DIR` in this workspace is a fallback behind the run-time
//! `AF_WORKSPACE_ROOT`. A Task check under `[warm]` (ADR-0123) reuses test binaries compiled in
//! an earlier, already-destroyed gate sandbox, so a fixture path baked in at compile time names
//! a directory that no longer exists; `scripts/verify.sh` exports the root for exactly this.
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_compile_time_manifest_dir_is_a_fallback_behind_the_runtime_workspace_root() {
    let mut files = Vec::new();
    rust_files(&workspace().join("crates"), &mut files);
    let mut offenders = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains("env!(\"CARGO_MANIFEST_DIR\")") {
                continue;
            }
            let window = lines[index.saturating_sub(6)..index + 1].join("\n");
            if !window.contains("AF_WORKSPACE_ROOT") {
                offenders.push(format!("{}:{}", file.display(), index + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "compile-time fixture paths without a run-time AF_WORKSPACE_ROOT fallback:\n{}",
        offenders.join("\n")
    );
}
