//! Rust `Debug` output must never be load-bearing in persisted state, Provider fingerprints,
//! Worker input, or JSON documents. ADR-0002 removed the first such case (`RunReport@1`), and a
//! later audit found the class had returned: `missing_nodes[].reason`, the Provider failure
//! fingerprint, the prior-Finding rows a Worker receives, and `af/review-outcome@1` all spelled
//! serde enums with `{:?}`. A variant rename compiled and silently changed persisted state.
//!
//! This guard keeps the class out. Every `{:?}` / `{:#?}` in non-test library and binary source
//! must appear in [`ALLOWED`], each entry a site whose formatted value is not a serde-derived
//! enum (an `io::Error`, a path, a string quoted for display) or a human-only diagnostic. A new
//! site fails here with instructions; a stale entry fails too, so the list cannot rot.
//!
//! The rule for adding an entry: if the value has a serde form, use it (`serde_json::to_value`
//! or the type's `as_str`) — never add the site here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `(path relative to the workspace root, the trimmed source line)`.
///
/// Categories, so a reviewer can check each entry against its rule:
/// - **quoted text**: a `String`/`&str`/`OsString` printed with `Debug` for the quotes;
/// - **non-serde type**: `ExitStatus`, `Vec<String>`, a `Path`, or an enum with no serde form;
/// - **serde enum, human diagnostic only**: an error or progress message that is never
///   persisted, matched, or delivered to a Worker — the crate is outside this guard's fix set,
///   and its owner should still move it to a serde name;
/// - **KNOWN DEFECT**: persisted through `Debug`; listed so the guard stays green while the
///   owning crate fixes it under its own versioning discipline. Never add to this category.
const ALLOWED: &[(&str, &str)] = &[
    // serde enum, human diagnostic only (review-config is not in this guard's fix set)
    (
        "crates/review-config/src/lib.rs",
        "\"reviewer `{node}` requires {:?} credentials but its runtime adapter is {:?}\",",
    ),
    // quoted text: untrusted argument values
    (
        "crates/review-core/src/exec.rs",
        "\"untrusted value {value:?} would be read as a response file\"",
    ),
    (
        "crates/review-core/src/exec.rs",
        "\"untrusted value {value:?} would be read as an option; a check must pass such values in a value position\"",
    ),
    (
        "crates/review-core/src/exec.rs",
        "write!(f, \"untrusted value {value:?} contains a NUL\")",
    ),
    // non-serde type: a bounded sample of mutated paths in a gate diagnostic
    (
        "crates/review-pipeline/src/kernel/gate.rs",
        "\"gate mutated its read-only sandbox: {} paths, e.g. {:?}\",",
    ),
    // quoted text / non-serde type: OsString attribute, ExitStatus, the sandbox `Isolation` enum
    (
        "crates/review-sandbox/src/cache.rs",
        "\"materialized cache object retained extended attribute {attribute:?}\"",
    ),
    (
        "crates/review-sandbox/src/container.rs",
        "\"runtime exited {:?}: {}\",",
    ),
    (
        "crates/review-sandbox/src/lib.rs",
        "\"pipeline requires {required:?} isolation but the sandbox provides only \\",
    ),
    (
        "crates/review-sandbox/src/lib.rs",
        "{provided:?}; refusing rather than reviewing under a weaker boundary than declared\"",
    ),
    // quoted text: git plumbing output echoed into capture diagnostics
    (
        "crates/review-source-git/src/capture.rs",
        "detail: format!(\"unparseable header {:?}\", header.trim_end()),",
    ),
    (
        "crates/review-source-git/src/capture.rs",
        "detail: format!(\"unparseable size in header {:?}\", header.trim_end()),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"invalid similarity score in {status:?}\"),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"resolved tree id is not a full object id: {tree:?}\"),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"similarity score exceeds 100 in {status:?}\"),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"synthetic manifest has invalid path {:?}\", entry.path),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"synthetic manifest size disagrees at {:?}\", entry.path),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"unexpected raw header {header:?}\"),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"unexpected score on raw status {status:?}\"),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"unknown raw status {status:?}\"),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"unsupported Git object format {object_format:?}\"),",
    ),
    (
        "crates/review-source-git/src/git.rs",
        "detail: format!(\"{operation} returned an invalid object id {value:?}\"),",
    ),
    // serde enum, human diagnostic only (materialize.rs is outside this guard's fix set;
    // `PathEncoding::as_str` exists for it)
    (
        "crates/review-source-git/src/materialize.rs",
        "\"path `{encoded}` is not canonical for {path_encoding:?}\"",
    ),
    // serde enum in Ledger projection notes: derived text, rebuilt from events, never an input
    // to identity (review-store is outside this guard's fix set)
    (
        "crates/review-store/src/ledger.rs",
        "\"resolution challenge {challenge:?}: re-reported as {} by {source} in round {round}\",",
    ),
    (
        "crates/review-store/src/ledger.rs",
        "\"resolution challenge {challenge:?}: re-reported by {source} in round {round}\"",
    ),
    (
        "crates/review-store/src/ledger.rs",
        "note: Some(format!(\"{:?}: {}\", challenge.kind, challenge.reason)),",
    ),
    // non-serde type: a list of malformed locations
    (
        "crates/review-store/src/ledger.rs",
        "{invalid_locations:?}; claim content remains readable with unknown Scope\"",
    ),
    // KNOWN DEFECT: `ResolutionChallengeKind` spelled by `Debug` inside a persisted
    // `Producer::KernelOperation.operation_id` (artifact identity) and `Resolution.outcome`
    // inside the persisted challenge reason. Owned by review-store; a fix changes artifact
    // identity and needs its versioning discipline.
    (
        "crates/review-store/src/legacy.rs",
        "\"automatic-resolution-challenge:{root}:{kind:?}\"",
    ),
    (
        "crates/review-store/src/legacy.rs",
        "\"in-scope Report {} challenged the scoped {:?} Resolution\",",
    ),
    // quoted text: Campaign labels and names in error messages; non-serde types: the graph's
    // `RunVerdict` progress line and the projection's `ScopeAuthorityKind`
    (
        "crates/reviewctl/src/main.rs",
        "\"campaign name {campaign:?} must be a trimmed human label without separators, control characters, traversal forms, or the reserved opaque Campaign ID shape\"",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"campaign name {campaign:?} must be one safe path component\"",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"campaign {campaign:?} has both encoded and legacy state beneath {}; remove the ambiguity before continuing\",",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"campaign {label:?} Round {} epoch {} refers to a different manifest\",",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"campaign {label:?} has both encoded and legacy state beneath {}; remove the ambiguity before continuing\",",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"campaign {label:?} is present in both encoded and legacy state directories beneath {}\",",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"campaign {label:?} opening and manifest disagree on authority Snapshot ID\"",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"campaign {label:?} state directory must be its opaque ID `{id}` or legacy label\"",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"legacy Campaign state {} blocks resolution of {campaign:?}: {error}\",",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"legacy sibling state for campaign {label:?} blocks direct resolution: {reason}\"",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "\"warning: round {} Report Scope is unknown: {:?} authority {} is unavailable: {}\",",
    ),
    (
        "crates/reviewctl/src/main.rs",
        ".map_err(|error| format!(\"reading campaign {label:?} manifest: {error}\"))?,",
    ),
    (
        "crates/reviewctl/src/main.rs",
        ".map_err(|error| format!(\"reading campaign {label:?} manifest: {error}\"))?;",
    ),
    (
        "crates/reviewctl/src/main.rs",
        ".map_err(|error| format!(\"reading campaign {label:?} opening: {error}\"))?;",
    ),
    (
        "crates/reviewctl/src/main.rs",
        ".ok_or_else(|| format!(\"campaign {label:?} has no CampaignOpened event\"))?;",
    ),
    (
        "crates/reviewctl/src/main.rs",
        "run_progress(options, format_args!(\"verdict  {verdict:?}\"));",
    ),
    // serde enum, human display only: the `af review show` history line, whose `Reported`
    // spelling is what operators and the CLI tests read; the projection is rebuilt, never
    // persisted from this text
    ("crates/reviewctl/src/main.rs", "\"  round {}: {:?}{}\","),
    // serde enums, human output only (onboard.rs and task.rs are outside this guard's fix set)
    (
        "crates/reviewctl/src/onboard.rs",
        ".map(|node| format!(\"node {} [{:?}]\", node.id, node.kind).to_lowercase())",
    ),
    // non-serde type: the pipeline's `RunVerdict` in TUI status lines
    (
        "crates/reviewctl/src/tui.rs",
        "Ok(verdict) => app.success(format!(\"Run completed: {verdict:?}\")),",
    ),
    (
        "crates/reviewctl/src/tui.rs",
        "Ok(verdict) => println!(\"\\naf review tui: run completed: {verdict:?}\"),",
    ),
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    let crates = root.join("crates");
    for entry in std::fs::read_dir(&crates).expect("crates directory") {
        let src = entry.expect("crate entry").path().join("src");
        if src.is_dir() {
            collect_rust_files(&src, &mut sources);
        }
    }
    sources.sort();
    sources
}

fn collect_rust_files(directory: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            collect_rust_files(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Code,
    Comment,
    Literal,
}

/// Classify every byte as code, comment, or string/char literal.
fn classify(source: &[u8]) -> Vec<Class> {
    let mut classes = vec![Class::Code; source.len()];
    let mut index = 0;
    while index < source.len() {
        let byte = source[index];
        let rest = &source[index..];
        if rest.starts_with(b"//") {
            let end = rest
                .iter()
                .position(|b| *b == b'\n')
                .map_or(source.len(), |offset| index + offset);
            classes[index..end].fill(Class::Comment);
            index = end;
        } else if rest.starts_with(b"/*") {
            let mut depth = 0_usize;
            let mut cursor = index;
            while cursor < source.len() {
                if source[cursor..].starts_with(b"/*") {
                    depth += 1;
                    cursor += 2;
                } else if source[cursor..].starts_with(b"*/") {
                    depth -= 1;
                    cursor += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    cursor += 1;
                }
            }
            classes[index..cursor.min(source.len())].fill(Class::Comment);
            index = cursor;
        } else if rest.starts_with(b"r\"")
            || rest.starts_with(b"r#")
            || rest.starts_with(b"br\"")
            || rest.starts_with(b"br#")
        {
            let start = index + if byte == b'b' { 2 } else { 1 };
            let hashes = source[start..].iter().take_while(|b| **b == b'#').count();
            let quote = start + hashes;
            if source.get(quote) != Some(&b'"') {
                index += 1;
                continue;
            }
            let mut terminator = vec![b'"'];
            terminator.extend(std::iter::repeat_n(b'#', hashes));
            let mut cursor = quote + 1;
            while cursor < source.len() && !source[cursor..].starts_with(&terminator) {
                cursor += 1;
            }
            let end = (cursor + terminator.len()).min(source.len());
            classes[index..end].fill(Class::Literal);
            index = end;
        } else if byte == b'"' || rest.starts_with(b"b\"") {
            let start = if byte == b'"' { index } else { index + 1 };
            let mut cursor = start + 1;
            while cursor < source.len() && source[cursor] != b'"' {
                if source[cursor] == b'\\' {
                    cursor += 1;
                }
                cursor += 1;
            }
            let end = (cursor + 1).min(source.len());
            classes[index..end].fill(Class::Literal);
            index = end;
        } else if byte == b'\'' {
            // A char literal closes within four bytes (`'x'`, `'\n'`) or is a `'\u{..}'`
            // escape; anything else is a lifetime and stays code.
            let close = source[index + 1..]
                .iter()
                .take(12)
                .position(|b| *b == b'\'')
                .map(|offset| index + 1 + offset);
            match close {
                Some(end)
                    if end - index <= 4
                        || source.get(index + 1) == Some(&b'\\')
                            && source.get(index + 2) == Some(&b'u') =>
                {
                    classes[index..=end].fill(Class::Literal);
                    index = end + 1;
                }
                _ => index += 1,
            }
        } else {
            index += 1;
        }
    }
    classes
}

/// Blank every `#[cfg(test)]` item and every comment, keeping newlines so lines still count.
fn non_test_code(source: &str) -> String {
    let bytes = source.as_bytes();
    let classes = classify(bytes);
    let mut blanked = vec![false; bytes.len()];
    let marker = b"#[cfg(test)]";
    let mut search = 0;
    while let Some(offset) = find(bytes, marker, search) {
        search = offset + marker.len();
        if classes[offset] != Class::Code {
            continue;
        }
        // The item's body: the first code `{` after the attribute, to its matching `}`.
        let open = (offset..bytes.len()).find(|i| classes[*i] == Class::Code && bytes[*i] == b'{');
        let Some(open) = open else {
            blanked[offset..].fill(true);
            break;
        };
        let mut depth = 0_usize;
        let mut close = bytes.len() - 1;
        for i in open..bytes.len() {
            if classes[i] != Class::Code {
                continue;
            }
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        blanked[offset..=close].fill(true);
        search = close + 1;
    }
    bytes
        .iter()
        .enumerate()
        .map(|(i, byte)| {
            if *byte == b'\n' {
                '\n'
            } else if blanked[i] || classes[i] == Class::Comment {
                ' '
            } else {
                *byte as char
            }
        })
        .collect()
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

#[test]
fn debug_formatting_outside_tests_is_allow_listed() {
    let root = workspace_root();
    let mut found = BTreeSet::new();
    for path in rust_sources(&root) {
        let source = std::fs::read_to_string(&path).expect("source is UTF-8");
        let relative = path
            .strip_prefix(&root)
            .expect("under the workspace")
            .to_string_lossy()
            .replace('\\', "/");
        for line in non_test_code(&source).lines() {
            if line.contains(":?}") || line.contains(":#?}") {
                found.insert((relative.clone(), line.trim().to_string()));
            }
        }
    }
    let allowed: BTreeSet<(String, String)> = ALLOWED
        .iter()
        .map(|(path, line)| (path.to_string(), line.to_string()))
        .collect();
    let unexpected: Vec<_> = found.difference(&allowed).collect();
    let stale: Vec<_> = allowed.difference(&found).collect();
    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "\nDebug formatting (`{{:?}}`) outside test code must be allow-listed in \
         crates/review-core/tests/debug_format_guard.rs.\n\
         A serde-derived enum must use its serde name (serde_json::to_value / as_str), never \
         Debug: the spelling would become persisted state or Worker input that a variant rename \
         silently changes.\n\n\
         new sites:\n{}\n\nstale allow-list entries:\n{}\n",
        unexpected
            .iter()
            .map(|(path, line)| format!("  (\"{path}\", \"{}\"),", line.replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join("\n"),
        stale
            .iter()
            .map(|(path, line)| format!("  ({path}, {line})"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_scanner_skips_test_modules_comments_and_string_braces() {
    let source = r##"
fn live() -> String { format!("{:?}", 1) } // {:?} in a comment
/// {value:?} in a doc comment
fn braces() { let _ = "}"; let _ = '{'; let _ = "a{b:?}"; }
#[cfg(test)]
mod tests {
    fn t() { let _ = "{"; assert!(true, "{:?}", 2); }
}
fn after() { let _ = r#"{after:?}"#; }
"##;
    let scanned = non_test_code(source);
    let hits: Vec<&str> = scanned
        .lines()
        .filter(|line| line.contains(":?}"))
        .map(str::trim)
        .collect();
    assert_eq!(
        hits,
        vec![
            r##"fn live() -> String { format!("{:?}", 1) }"##,
            r##"fn braces() { let _ = "}"; let _ = '{'; let _ = "a{b:?}"; }"##,
            r##"fn after() { let _ = r#"{after:?}"#; }"##,
        ]
    );
}
