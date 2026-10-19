//! ADR-0153 clause 1 and ADR-0026 in the source: no crate but `af` links the reader module
//! (`segment::reader`, `index`, `status`, `blob::reader`, `blob::collect`), `review-process`
//! links nothing of Telemetry, and Telemetry links nothing in the workspace.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

const READER: [&str; 5] = [
    "segment::reader",
    "index",
    "status",
    "blob::reader",
    "blob::collect",
];

/// Whether `text` names the reader module through the `review_telemetry` path.
fn names_reader(text: &str) -> Option<String> {
    for (at, _) in text.match_indices("review_telemetry::") {
        let rest = &text[at + "review_telemetry::".len()..];
        if let Some(group) = rest.strip_prefix('{') {
            let group = &group[..group.find('}').unwrap_or(group.len())];
            for item in group.split(',') {
                let item = item.trim();
                if READER.iter().any(|reader| item.starts_with(reader))
                    || item.starts_with("segment::{")
                    || item.starts_with("blob::{")
                {
                    return Some(item.to_string());
                }
            }
        } else if let Some(reader) = READER.iter().find(|reader| rest.starts_with(*reader)) {
            return Some((*reader).to_string());
        }
    }
    None
}

#[test]
fn only_af_links_the_reader_module_and_review_process_links_nothing() {
    let crates = workspace_root().join("crates");
    for entry in std::fs::read_dir(&crates).unwrap().filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), "af" | "review-telemetry") {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&entry.path(), &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            assert_eq!(
                names_reader(&text),
                None,
                "{} links Telemetry's reader module",
                file.display()
            );
        }
    }
    let process = std::fs::read_to_string(crates.join("review-process/Cargo.toml")).unwrap();
    assert!(
        !process.contains("review-telemetry"),
        "ADR-0026: review-process is a leaf"
    );
    let mut process_sources = Vec::new();
    rust_files(&crates.join("review-process"), &mut process_sources);
    for file in process_sources {
        assert!(
            !std::fs::read_to_string(&file)
                .unwrap()
                .contains("review_telemetry")
        );
    }
    let telemetry = std::fs::read_to_string(crates.join("review-telemetry/Cargo.toml")).unwrap();
    assert!(
        !telemetry.contains("path ="),
        "the telemetry crate is a leaf"
    );
    assert_eq!(
        names_reader("use review_telemetry::redact::WITHHELD;"),
        None
    );
    assert!(names_reader("use review_telemetry::{redact, index::Index};").is_some());
    assert!(names_reader("review_telemetry::segment::reader::list").is_some());
}
