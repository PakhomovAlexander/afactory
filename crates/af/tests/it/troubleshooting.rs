//! The troubleshooting table of `docs/README.md` quotes messages `af` prints: every row's
//! message, its variable parts written `…`, is found word for word in the source of a crate the
//! binary is built from. A message whose wording changes must change the table with it.

use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// Every Rust source file under `dir`.
fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// The text of every string the binary's crates hold, as the binary prints it: a string
/// continued with `\` at a line's end joined, and `\"` read as `"`. Tests are left out, so a
/// row can only be matched by what a release prints.
fn printed() -> String {
    let mut files = Vec::new();
    for crate_dir in std::fs::read_dir(workspace().join("crates")).unwrap() {
        let src = crate_dir.unwrap().path().join("src");
        if src.is_dir() {
            sources(&src, &mut files);
        }
    }
    let mut text = String::new();
    for file in files {
        if file
            .file_name()
            .is_some_and(|name| name == "tests.rs" || name.to_string_lossy().ends_with("_tests.rs"))
            || file.components().any(|part| part.as_os_str() == "tests")
        {
            continue;
        }
        let source = std::fs::read_to_string(&file).unwrap();
        let mut joined = String::with_capacity(source.len());
        let mut lines = source.lines().peekable();
        while let Some(line) = lines.next() {
            match line.strip_suffix('\\') {
                // A string continued: the next line's leading whitespace is not part of it.
                Some(head) => {
                    joined.push_str(head);
                    if let Some(next) = lines.peek_mut() {
                        *next = next.trim_start();
                    }
                }
                None => {
                    joined.push_str(line);
                    joined.push('\n');
                }
            }
        }
        text.push_str(&joined.replace("\\\"", "\""));
    }
    text
}

/// The rows of the table under `## Troubleshooting`: message, meaning, fix.
fn rows() -> Vec<[String; 3]> {
    let readme = std::fs::read_to_string(workspace().join("docs/README.md")).unwrap();
    let section = readme
        .split("\n## Troubleshooting\n")
        .nth(1)
        .expect("docs/README.md has a Troubleshooting section");
    let section = section.split("\n## ").next().unwrap();
    section
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cells: Vec<String> = line
                .trim_matches('|')
                .split(" | ")
                .map(|cell| cell.trim().to_owned())
                .collect();
            assert_eq!(cells.len(), 3, "{line}");
            [cells[0].clone(), cells[1].clone(), cells[2].clone()]
        })
        .collect()
}

#[test]
fn every_troubleshooting_row_quotes_a_message_af_prints() {
    let printed = printed();
    let rows = rows();
    assert!(rows.len() >= 20, "{} rows", rows.len());
    for [message, meaning, fix] in &rows {
        assert!(!meaning.is_empty() && !fix.is_empty(), "{message}");
        let quoted = message
            .strip_prefix('`')
            .and_then(|message| message.strip_suffix('`'))
            .unwrap_or_else(|| panic!("a message is quoted as code: {message}"));
        for piece in quoted
            .split('…')
            .map(str::trim)
            .filter(|piece| !piece.is_empty())
        {
            assert!(
                piece.len() >= 8 || quoted.split('…').count() > 1,
                "too short to identify a message: {piece:?}"
            );
            assert!(
                printed.contains(piece),
                "`{piece}` of the troubleshooting row `{quoted}` is not a message af prints"
            );
        }
    }
}
