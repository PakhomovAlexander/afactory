//! The `no-perf` label of a gate pull request (ADR-0196): whether a candidate's whole change
//! from the Task's source is, by the operator's own list of prose-only paths, without
//! performance impact.
//!
//! Nothing here infers impact from a file's extension or a diff's size: Markdown can be embedded
//! in a binary, and `.af/` prompts are runtime inputs. A candidate qualifies only when every
//! path that differs from the source, in presence, mode or bytes, is a regular file on each existing
//! side that one of the mapping entry's `no_perf_paths` names. Everything else — an unlisted
//! path, an executable or a link, a path the gate workflow or Cargo reads before any command,
//! an empty change or more changes than the bound — does not qualify. The decision is a pure,
//! bounded function of the two manifests and the operator's list, made again for every remote
//! phase, so a repair round or a resume never inherits an earlier phase's exemption.

use std::collections::BTreeMap;

use review_source_git::{EntryKind, Manifest};

/// The one label the executor sets or removes, and only on its own gate pull request.
pub const NO_PERF_LABEL: &str = "no-perf";
/// At most this many `no_perf_paths` in one mapping entry.
pub const MAX_NO_PERF_PATHS: usize = 64;
/// At most this many bytes in one `no_perf_paths` pattern.
pub const MAX_NO_PERF_PATH_BYTES: usize = 256;
/// A candidate with more changed paths than this never qualifies; the walk stops there.
pub const MAX_CLASSIFIED_CHANGES: usize = 1000;

/// The operator's validated prose-only paths. Each pattern is a `/`-separated repository path
/// whose first component is literal; a later component may hold `*`, which matches any run of
/// characters within that one component. Empty means no candidate qualifies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoPerfPaths {
    patterns: Vec<String>,
}

/// Whether a candidate qualifies for the label, and why not when it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoPerf {
    /// Every one of the `changed` paths is a listed prose-only file.
    Exempt {
        changed: usize,
    },
    NotExempt(String),
}

impl NoPerf {
    pub fn exempt(&self) -> bool {
        matches!(self, Self::Exempt { .. })
    }
}

/// Whether any component of `path` is one the gate workflow, Git or Cargo reads as
/// configuration or as an af runtime input, or a Cargo build-closure file; such a path never
/// qualifies, whatever the list says.
fn never_prose(path: &str) -> bool {
    let components: Vec<&str> = path.split('/').collect();
    let last = components.last().copied().unwrap_or_default();
    components.iter().any(|component| {
        let folded = component.to_ascii_lowercase();
        folded == ".af" || folded == ".cargo" || folded.starts_with(".git")
    }) || [
        "cargo.toml",
        "cargo.lock",
        "build.rs",
        "rust-toolchain",
        "rust-toolchain.toml",
    ]
    .contains(&last.to_ascii_lowercase().as_str())
        || (components.len() == 1
            && ["action.yml", "action.yaml"].contains(&last.to_ascii_lowercase().as_str()))
}

impl NoPerfPaths {
    /// Validate the mapping's list. A refusal names the pattern's position, never its text.
    pub fn parse(patterns: &[String]) -> Result<Self, String> {
        if patterns.len() > MAX_NO_PERF_PATHS {
            return Err(format!(
                "`no_perf_paths` lists more than {MAX_NO_PERF_PATHS} paths"
            ));
        }
        for (index, pattern) in patterns.iter().enumerate() {
            let at = format!("`no_perf_paths` entry {}", index + 1);
            if pattern.is_empty() || pattern.len() > MAX_NO_PERF_PATH_BYTES {
                return Err(format!("{at} must be 1 to {MAX_NO_PERF_PATH_BYTES} bytes"));
            }
            if !pattern
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/' | b'*'))
            {
                return Err(format!(
                    "{at} may hold only ASCII letters, digits, `.`, `_`, `-`, `/` and `*`"
                ));
            }
            let components: Vec<&str> = pattern.split('/').collect();
            if components
                .iter()
                .any(|c| c.is_empty() || *c == "." || *c == ".." || c.contains("**"))
            {
                return Err(format!(
                    "{at} must be a relative path without empty, `.`, `..` or `**` components"
                ));
            }
            if components[0].contains('*') {
                return Err(format!(
                    "{at} must begin with a literal directory or file name, not a wildcard"
                ));
            }
            if never_prose(pattern) {
                return Err(format!(
                    "{at} names `.af`, `.cargo`, a `.git*` path, a root action or a Cargo \
                     build-closure file, which is never prose-only"
                ));
            }
        }
        let mut patterns = patterns.to_vec();
        patterns.sort();
        patterns.dedup();
        Ok(Self { patterns })
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    fn matches(&self, path: &str) -> bool {
        !never_prose(path)
            && self.patterns.iter().any(|pattern| {
                let (wanted, given): (Vec<&str>, Vec<&str>) =
                    (pattern.split('/').collect(), path.split('/').collect());
                wanted.len() == given.len()
                    && wanted
                        .iter()
                        .zip(&given)
                        .all(|(w, g)| component_matches(w.as_bytes(), g.as_bytes()))
            })
    }

    /// Classify the whole change from `source` to `candidate`.
    pub fn classify(&self, source: &Manifest, candidate: &Manifest) -> NoPerf {
        if self.patterns.is_empty() {
            return NoPerf::NotExempt(
                "the mapping lists no `no_perf_paths` for this repository".into(),
            );
        }
        fn index(manifest: &Manifest) -> BTreeMap<&str, (EntryKind, &str)> {
            manifest
                .entries
                .iter()
                .map(|entry| (entry.path.as_str(), (entry.kind, entry.content.as_str())))
                .collect()
        }
        let (before, after) = (index(source), index(candidate));
        let mut changed = 0_usize;
        let mut paths = before
            .keys()
            .chain(after.keys())
            .copied()
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths.dedup();
        for path in paths {
            let (was, now) = (before.get(path), after.get(path));
            if was == now {
                continue;
            }
            changed += 1;
            if changed > MAX_CLASSIFIED_CHANGES {
                return NoPerf::NotExempt(format!(
                    "the candidate changes more than {MAX_CLASSIFIED_CHANGES} paths"
                ));
            }
            let shown = shown(path);
            if [was, now]
                .into_iter()
                .flatten()
                .any(|(kind, _)| *kind != EntryKind::File)
            {
                return NoPerf::NotExempt(format!(
                    "`{shown}` is an executable or a symbolic link on one side"
                ));
            }
            let decoded = review_core::decode_path(path);
            match std::str::from_utf8(&decoded) {
                Ok(text) if self.matches(text) => {}
                _ => {
                    return NoPerf::NotExempt(format!(
                        "`{shown}` is not one of the mapping's `no_perf_paths`"
                    ));
                }
            }
        }
        if changed == 0 {
            return NoPerf::NotExempt("the candidate does not differ from the source".into());
        }
        NoPerf::Exempt { changed }
    }
}

/// `pattern` against one path component: `*` matches any run of bytes, everything else itself.
fn component_matches(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((at, from)) = star {
            p = at + 1;
            t = from + 1;
            star = Some((at, from + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|b| *b == b'*')
}

/// A path as a message shows it: control characters replaced, at most 200 characters.
fn shown(path: &str) -> String {
    String::from_utf8_lossy(&review_core::decode_path(path))
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_source_git::Entry;

    fn digest(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn manifest(entries: &[(&str, EntryKind, char)]) -> Manifest {
        Manifest::new(
            entries
                .iter()
                .map(|(path, kind, content)| Entry {
                    path: review_core::encode_path(path.as_bytes()),
                    kind: *kind,
                    content: digest(*content),
                    size: 1,
                })
                .collect(),
        )
        .unwrap()
    }

    fn paths(list: &[&str]) -> NoPerfPaths {
        NoPerfPaths::parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    fn source() -> Manifest {
        manifest(&[
            ("docs/adr/0001-a.md", EntryKind::File, '1'),
            ("docs/guide.md", EntryKind::File, '2'),
            ("crates/af/src/README.md", EntryKind::File, '3'),
            ("src/lib.rs", EntryKind::File, '4'),
            (".af/workers/prompt.md", EntryKind::File, '5'),
        ])
    }

    fn with(changes: &[(&str, EntryKind, char)], removed: &[&str]) -> Manifest {
        let mut entries: Vec<(String, EntryKind, char)> = source()
            .entries
            .iter()
            .map(|e| {
                let path = String::from_utf8(review_core::decode_path(&e.path)).unwrap();
                let content = e.content.chars().last().unwrap();
                (path, e.kind, content)
            })
            .filter(|(path, _, _)| {
                !removed.contains(&path.as_str()) && !changes.iter().any(|(p, _, _)| p == path)
            })
            .collect();
        entries.extend(changes.iter().map(|(p, k, c)| (p.to_string(), *k, *c)));
        let borrowed: Vec<(&str, EntryKind, char)> = entries
            .iter()
            .map(|(p, k, c)| (p.as_str(), *k, *c))
            .collect();
        manifest(&borrowed)
    }

    #[test]
    fn only_listed_prose_files_qualify_and_every_other_change_does_not() {
        let list = paths(&["docs/adr/*.md", "docs/*.md", "changelog.d/*.md"]);
        let source = source();
        // Edits, additions and deletions of listed regular files qualify together.
        let prose = with(
            &[
                ("docs/adr/0001-a.md", EntryKind::File, 'a'),
                ("docs/new.md", EntryKind::File, 'b'),
                ("changelog.d/note.md", EntryKind::File, 'c'),
            ],
            &["docs/guide.md"],
        );
        assert_eq!(
            list.classify(&source, &prose),
            NoPerf::Exempt { changed: 4 }
        );
        for (candidate, needle) in [
            // Markdown is not prose by its extension: an embedded README is runtime.
            (
                with(&[("crates/af/src/README.md", EntryKind::File, 'x')], &[]),
                "crates/af/src/README.md",
            ),
            // An af prompt is a runtime input.
            (
                with(&[(".af/workers/prompt.md", EntryKind::File, 'x')], &[]),
                ".af/workers/prompt.md",
            ),
            // A listed path that changed mode, or became a link, is not prose.
            (
                with(&[("docs/guide.md", EntryKind::Executable, '2')], &[]),
                "docs/guide.md",
            ),
            (
                with(&[("docs/guide.md", EntryKind::Symlink, 'x')], &[]),
                "docs/guide.md",
            ),
            // A mixed change does not qualify for its prose part.
            (
                with(
                    &[
                        ("docs/guide.md", EntryKind::File, 'x'),
                        ("src/lib.rs", EntryKind::File, 'x'),
                    ],
                    &[],
                ),
                "src/lib.rs",
            ),
            // A wildcard spans one component only.
            (
                with(&[("docs/adr/deep/x.md", EntryKind::File, 'x')], &[]),
                "docs/adr/deep/x.md",
            ),
            (
                with(&[("docs/guide.txt", EntryKind::File, 'x')], &[]),
                "docs/guide.txt",
            ),
        ] {
            match list.classify(&source, &candidate) {
                NoPerf::NotExempt(why) => assert!(why.contains(needle), "{why}"),
                other => panic!("{needle}: {other:?}"),
            }
        }
        assert_eq!(
            list.classify(&source, &source),
            NoPerf::NotExempt("the candidate does not differ from the source".into())
        );
        assert!(!NoPerfPaths::default().classify(&source, &prose).exempt());
    }

    #[test]
    fn a_matching_name_the_build_or_the_workflow_reads_never_qualifies() {
        let list = paths(&["docs/*"]);
        let source = source();
        for path in [
            "docs/Cargo.toml",
            "docs/build.rs",
            "docs/rust-toolchain.toml",
            "docs/.gitattributes",
            "docs/.cargo",
        ] {
            let candidate = with(&[(path, EntryKind::File, 'x')], &[]);
            assert!(!list.classify(&source, &candidate).exempt(), "{path}");
        }
        let candidate = with(&[("docs/notes", EntryKind::File, 'x')], &[]);
        assert!(list.classify(&source, &candidate).exempt());
    }

    #[test]
    fn a_change_set_past_the_bound_never_qualifies() {
        let list = paths(&["docs/*.md"]);
        let names: Vec<String> = (0..=MAX_CLASSIFIED_CHANGES)
            .map(|i| format!("docs/{i:05}.md"))
            .collect();
        let many: Vec<(&str, EntryKind, char)> = names
            .iter()
            .map(|n| (n.as_str(), EntryKind::File, 'x'))
            .collect();
        let candidate = with(&many, &[]);
        match list.classify(&source(), &candidate) {
            NoPerf::NotExempt(why) => assert!(why.contains("more than 1000"), "{why}"),
            other => panic!("{other:?}"),
        }
        let fewer = with(&many[..MAX_CLASSIFIED_CHANGES], &[]);
        assert!(list.classify(&source(), &fewer).exempt());
    }

    #[test]
    fn broad_or_unsafe_patterns_are_refused_without_quoting_them() {
        for pattern in [
            "",
            "*.md",
            "*/README.md",
            "docs/**/*.md",
            "docs//x.md",
            "./docs/x.md",
            "docs/../src/lib.rs",
            "/docs/x.md",
            "docs/x?.md",
            "docs/x y.md",
            ".af/prompt.md",
            ".github/workflows/x.yml",
            "docs/.cargo/config.toml",
            "docs/Cargo.toml",
            "action.yml",
            ".gitmodules",
        ] {
            let error = NoPerfPaths::parse(&[pattern.to_string()]).unwrap_err();
            assert!(error.contains("entry 1"), "{pattern}: {error}");
            if pattern.len() > 3 {
                assert!(!error.contains(pattern), "{error}");
            }
        }
        let long = format!("docs/{}", "a".repeat(MAX_NO_PERF_PATH_BYTES));
        assert!(NoPerfPaths::parse(&[long]).is_err());
        let many: Vec<String> = (0..=MAX_NO_PERF_PATHS)
            .map(|i| format!("d/{i}.md"))
            .collect();
        assert!(NoPerfPaths::parse(&many).is_err());
        assert!(NoPerfPaths::parse(&many[..MAX_NO_PERF_PATHS]).is_ok());
        assert!(NoPerfPaths::parse(&[]).unwrap().is_empty());
    }

    #[test]
    fn a_star_matches_within_one_component() {
        for (pattern, text, expected) in [
            ("*.md", "a.md", true),
            ("*.md", ".md", true),
            ("*.md", "a.mdx", false),
            ("0*-*.md", "0196-x-y.md", true),
            ("0*-*.md", "0196.md", false),
            ("*", "anything", true),
            ("a*b*c", "abc", true),
            ("a*b*c", "acb", false),
            ("README.md", "README.md", true),
            ("README.md", "readme.md", false),
        ] {
            assert_eq!(
                component_matches(pattern.as_bytes(), text.as_bytes()),
                expected,
                "{pattern} {text}"
            );
        }
    }
}
