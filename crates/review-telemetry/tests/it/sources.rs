//! ADR-0153 clauses 1 and 2 and ADR-0026 in the source: no crate but `af` links the reader
//! module (`segment::reader`, `index`, `status`, `view`, `ledger`, `accounting`, `turns`,
//! `content_view`, `budget`, `blob::reader`, `blob::collect`), no code that composes a prompt,
//! a Warm Set, Worker Notes or an optimization source reads Telemetry in any form,
//! `review-process` links nothing of Telemetry, and Telemetry links nothing in the workspace.

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

const READER: [&str; 11] = [
    "segment::reader",
    "index",
    "status",
    "view",
    "ledger",
    "accounting",
    "turns",
    "content_view",
    "budget",
    "blob::reader",
    "blob::collect",
];

/// `text` without its in-file test modules: from a `#[cfg(test)]` marker that a `mod … {` line
/// follows, to that module's closing brace at column 0 (rustfmt's layout; a column-0 `}` inside
/// one of its string literals only ends the cut early, which judges more, never less). A marker
/// on anything else, and an out-of-line `mod tests;`, cuts nothing. The rule is about the code
/// a Worker's input passes through: a test module of one of these files may write Telemetry to
/// exercise the spans of its own file.
fn without_test_modules(text: &str) -> String {
    const MARKER: &str = "#[cfg(test)]";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARKER) {
        out.push_str(&rest[..at]);
        let after = &rest[at + MARKER.len()..];
        let opens_module = after
            .trim_start()
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("mod ") && line.trim_end().ends_with('{'));
        rest = match (opens_module, after.find("\n}\n")) {
            (true, Some(end)) => &after[end + "\n}\n".len()..],
            _ => {
                out.push_str(MARKER);
                after
            }
        };
    }
    out.push_str(rest);
    out
}

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

/// The code that composes what a Worker receives, in every crate, `af` included: the Worker
/// request and the model prompt (`review-runner`), every Task host's context, Warm Set and
/// Worker Notes (`review-pipeline`'s `task/` and its warm paths, `review-core`'s Warm Set), and
/// the self-optimizer's sources and History Capture (`af`, `review-store`, `review-core`,
/// `review-config`).
const WORKER_INPUT: [&str; 12] = [
    "crates/review-runner/src/task.rs",
    "crates/review-runner/src/task",
    "crates/review-runner/src/model.rs",
    "crates/review-pipeline/src/task",
    "crates/review-pipeline/src/warm.rs",
    "crates/review-pipeline/src/review_domain.rs",
    "crates/review-pipeline/src/build_cache.rs",
    "crates/review-core/src/warm.rs",
    "crates/review-core/src/task/optimization.rs",
    "crates/review-store/src/optimization.rs",
    "crates/review-config/src/layout.rs",
    "crates/af/src/self_optimizer.rs",
];

/// Design §16 item 2 and ADR-0153 clause 2 in the source: no prompt-composing, Warm Set, Worker
/// Notes or optimization-source code imports Telemetry's reader module — `content_view`, the
/// index, the blob reader or any other — or `af telemetry`'s readers, or names the telemetry
/// directory, its blobs or its registry; the self-optimizer declares no Telemetry source.
#[test]
fn telemetry_content_reaches_no_worker_input() {
    let root = workspace_root();
    let mut files = Vec::new();
    for path in WORKER_INPUT {
        let path = root.join(path);
        if path.is_dir() {
            rust_files(&path, &mut files);
        } else {
            assert!(
                path.is_file(),
                "{} is gone: update the list",
                path.display()
            );
            files.push(path);
        }
    }
    assert!(files.len() > 20, "{files:?}");
    for file in &files {
        let text = without_test_modules(&std::fs::read_to_string(file).unwrap());
        assert_eq!(
            names_reader(&text),
            None,
            "{} reads Telemetry's reader module",
            file.display()
        );
        for needle in [
            "content_view",
            "blob::reader",
            "telemetry::views",
            "telemetry::content",
            "telemetry::harness",
            "telemetry-dirs",
            "v1/blobs",
            "index.sqlite",
            "\"telemetry\")",
        ] {
            assert!(
                !text.contains(needle),
                "{} names `{needle}`: a Worker's input never reads Telemetry",
                file.display()
            );
        }
    }
    // The self-optimizer's declared native sources hold no Telemetry source.
    for sources in [
        root.join(".af/optimization-sources.toml"),
        root.join("crates/review-config/src/layout.rs"),
    ] {
        if let Ok(text) = std::fs::read_to_string(&sources) {
            assert!(
                !text.to_lowercase().contains("telemetry"),
                "{} declares a Telemetry source",
                sources.display()
            );
        }
    }
    assert_eq!(
        without_test_modules("a\n#[cfg(test)]\nmod tests {\n    let x = \"telemetry\";\n}\nb\n"),
        "a\nb\n"
    );
    for kept in [
        "a\n#[cfg(test)]\nmod tests;\nb\n",
        "a\n#[cfg(test)]\n#[path = \"x/tests.rs\"]\nmod tests;\nb\n",
        "a\n#[cfg(test)]\nfn helper() {\n}\nb\n",
    ] {
        assert_eq!(without_test_modules(kept), kept);
    }
    assert!(names_reader("use review_telemetry::content_view::context;").is_some());
    assert!(names_reader("use review_telemetry::{content, content_view::Search};").is_some());
    assert_eq!(
        names_reader("use review_telemetry::content::on_span;"),
        None
    );
}

/// Every span name the catalogue in `docs/architecture.md` lists.
fn catalogue() -> std::collections::BTreeSet<String> {
    let text = std::fs::read_to_string(workspace_root().join("docs/architecture.md")).unwrap();
    let section = text
        .split("### Telemetry span catalogue")
        .nth(1)
        .expect("docs/architecture.md has the telemetry span catalogue");
    section
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .filter_map(|line| line.split('|').nth(1))
        .flat_map(|cell| {
            cell.split(',')
                .filter_map(|name| {
                    name.trim()
                        .strip_prefix('`')
                        .and_then(|name| name.strip_suffix('`'))
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The `af.*` span names a source emits: the name literal of every `span!(` macro (`info_span!`,
/// `process_span!`), the explicit parent skipped; unit tests included.
fn emitted(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (at, _) in text.match_indices("span!(") {
        let rest = &text[at + "span!(".len()..];
        let Some(open) = rest.find('"') else {
            continue;
        };
        if rest[..open].contains(')') {
            continue;
        }
        let rest = &rest[open + 1..];
        if let Some(close) = rest.find('"') {
            let name = &rest[..close];
            if name.starts_with("af.") {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Design §7.1 (P3): every span name any crate's source emits is a row of the catalogue table
/// in `docs/architecture.md`, so a new span site without its row fails here; and every record
/// kind the crate writes is in the record schema and the store's record list, at its version.
#[test]
fn telemetry_span_catalogue() {
    let catalogue = catalogue();
    let crates = workspace_root().join("crates");
    let mut found = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(&crates).unwrap().filter_map(Result::ok) {
        let mut files = Vec::new();
        rust_files(&entry.path().join("src"), &mut files);
        for file in files {
            for name in emitted(&std::fs::read_to_string(&file).unwrap()) {
                found.entry(name).or_insert_with(|| file.clone());
            }
        }
    }
    for phase in [
        "af.run",
        "af.check",
        "af.harness",
        "af.git",
        "af.store.append",
        "af.snapshot.capture",
        "af.measurement.repetition",
    ] {
        assert!(found.contains_key(phase), "{phase} is emitted: {found:?}");
    }
    for (name, file) in &found {
        assert!(
            catalogue.contains(name),
            "{name}, emitted in {}, is not a row of the catalogue in docs/architecture.md",
            file.display()
        );
    }
    // Every record kind the crate writes, at the version it writes, is in the record schema
    // and in the store's record list in `docs/telemetry.md`, and no other version of it is.
    let record = std::fs::read_to_string(crates.join("review-telemetry/src/record.rs")).unwrap();
    let module = record
        .split("pub mod schema {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .unwrap();
    let kinds: Vec<&str> = module
        .lines()
        .filter_map(|line| line.split('"').nth(1))
        .filter(|kind| kind.starts_with("af/Telemetry"))
        .collect();
    assert_eq!(kinds.len(), 11, "{kinds:?}");
    let schema =
        std::fs::read_to_string(workspace_root().join("schemas/telemetry-record-v1.json")).unwrap();
    let docs = std::fs::read_to_string(workspace_root().join("docs/telemetry.md")).unwrap();
    let store = docs
        .split("- **Segments.**")
        .nth(1)
        .and_then(|rest| rest.split("- **Blobs.**").next())
        .unwrap();
    for kind in &kinds {
        assert!(
            schema.contains(&format!("\"{kind}\"")),
            "{kind} not in the record schema"
        );
        assert!(
            store.contains(&format!("`{kind}`")),
            "{kind} not in docs/telemetry.md"
        );
        let (name, version) = kind.split_once('@').unwrap();
        for other in 1..version.parse::<u32>().unwrap() {
            let stale = format!("{name}@{other}");
            assert!(
                !schema.contains(&format!("\"{stale}\"")),
                "{stale} in the schema"
            );
            assert!(
                !store.contains(&format!("`{stale}`")),
                "{stale} in the docs"
            );
        }
    }
    // The scanner reads the forms the sources use.
    assert_eq!(
        emitted("tracing::info_span!(\n    parent: &self.run,\n    \"af.plan\",\n)"),
        ["af.plan"]
    );
    assert_eq!(
        emitted("review_telemetry::process_span!(\"af.git\", process.command = \"git\")"),
        ["af.git"]
    );
    assert!(emitted("tracing::info_span!(\"other\")").is_empty());
}
