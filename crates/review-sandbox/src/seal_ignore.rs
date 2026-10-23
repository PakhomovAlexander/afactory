//! What sealing leaves out of a candidate (ADR-0198): the files the project's own ignore rules
//! exclude, which a Worker's test run or build generated (`__pycache__/`, `*.pyc`, `/target/`,
//! `node_modules/`) and nobody meant to deliver.
//!
//! The rules are git's ignore rules, read from the sealed tree itself: every `.gitignore`, in
//! directory order, then `.af/seal-ignore`, after a short kernel default list. The last rule that
//! matches a path decides; a `!` rule takes a path back, which is how a Worker stages a file the
//! rules would otherwise leave out; a path below an ignored directory stays ignored, as in git.
//! Only a path the parent Snapshot did not hold can be ignored: what the Snapshot tracked is
//! sealed whatever the rules say.

use std::path::Path;

use review_source_git::{EntryKind, Manifest, fs_path};

/// The rules every seal starts from, before the project's own: the generated files of the
/// commonest toolchains. A project's `!` rule takes any of them back.
pub const DEFAULT_SEAL_IGNORE: &[&str] = &["__pycache__/", "*.pyc", "/target/", "node_modules/"];

/// The project's sealing-only rules, read after every `.gitignore`.
pub const SEAL_IGNORE_FILE: &str = ".af/seal-ignore";

/// The most bytes one ignore file may hold; a larger one is not read.
const MAX_IGNORE_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    /// The directory of the file the rule came from, `""` for the root.
    base: String,
    segments: Vec<String>,
    /// The pattern held a `/` before its end: it matches from `base`, not any basename.
    anchored: bool,
    directory_only: bool,
    negated: bool,
}

/// The ignore rules of one sealed tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoreRules {
    rules: Vec<Rule>,
}

impl IgnoreRules {
    /// The kernel defaults, then every `.gitignore` the manifest holds as a regular file (the
    /// root's first, then deeper ones), then `.af/seal-ignore`, each read below `root` without
    /// following a link.
    pub fn of_tree(root: &Path, manifest: &Manifest) -> std::io::Result<Self> {
        let mut rules = Self::default();
        for line in DEFAULT_SEAL_IGNORE {
            rules.push("", line);
        }
        let mut files: Vec<&str> = manifest
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::File | EntryKind::Executable))
            .map(|entry| entry.path.as_str())
            .filter(|path| *path == ".gitignore" || path.ends_with("/.gitignore"))
            .collect();
        files.sort_by_key(|path| (path.matches('/').count(), *path));
        if manifest
            .get(SEAL_IGNORE_FILE)
            .is_some_and(|entry| matches!(entry.kind, EntryKind::File | EntryKind::Executable))
        {
            files.push(SEAL_IGNORE_FILE);
        }
        for file in files {
            let base = if file == SEAL_IGNORE_FILE {
                ""
            } else {
                file.rsplit_once('/').map_or("", |(directory, _)| directory)
            };
            let path = root.join(fs_path(file));
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() || metadata.len() > MAX_IGNORE_FILE_BYTES {
                continue;
            }
            let text = std::fs::read(&path)?;
            for line in String::from_utf8_lossy(&text).lines() {
                rules.push(base, line);
            }
        }
        Ok(rules)
    }

    /// Rules from `lines`, as one ignore file at the root holds them.
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Self {
        let mut rules = Self::default();
        for line in lines {
            rules.push("", line);
        }
        rules
    }

    fn push(&mut self, base: &str, line: &str) {
        let line = line.trim_end_matches(['\r', ' ']);
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        let (negated, line) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line.strip_prefix('\\').unwrap_or(line)),
        };
        let (directory_only, line) = match line.strip_suffix('/') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let anchored = line.contains('/');
        let line = line.strip_prefix('/').unwrap_or(line);
        if line.is_empty() {
            return;
        }
        self.rules.push(Rule {
            base: base.to_string(),
            segments: line.split('/').map(String::from).collect(),
            anchored,
            directory_only,
            negated,
        });
    }

    /// Whether `path`, a Manifest path of a file, is ignored: one of its directories is, or the
    /// last rule matching it ignores it.
    pub fn ignores(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        for depth in 1..parts.len() {
            if self.decides(&parts[..depth], true) {
                return true;
            }
        }
        self.decides(&parts, false)
    }

    fn decides(&self, parts: &[&str], directory: bool) -> bool {
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.matches(parts, directory))
            .is_some_and(|rule| !rule.negated)
    }
}

impl Rule {
    fn matches(&self, parts: &[&str], directory: bool) -> bool {
        if self.directory_only && !directory {
            return false;
        }
        let relative = if self.base.is_empty() {
            parts
        } else {
            let base: Vec<&str> = self.base.split('/').collect();
            match parts.strip_prefix(base.as_slice()) {
                Some(rest) => rest,
                None => return false,
            }
        };
        if relative.is_empty() {
            return false;
        }
        if !self.anchored {
            return glob(&self.segments[0], relative[relative.len() - 1]);
        }
        let segments: Vec<&str> = self.segments.iter().map(String::as_str).collect();
        segments_match(&segments, relative)
    }
}

/// `pattern` against `parts`, a `**` segment standing for any number of directories.
fn segments_match(pattern: &[&str], parts: &[&str]) -> bool {
    match pattern.split_first() {
        None => parts.is_empty(),
        Some((&"**", rest)) => (0..=parts.len()).any(|skip| segments_match(rest, &parts[skip..])),
        Some((first, rest)) => parts
            .split_first()
            .is_some_and(|(part, others)| glob(first, part) && segments_match(rest, others)),
    }
}

/// One path segment against one glob segment: `*`, `?`, `[...]` (with `!` or `^` negation and
/// ranges) and `\` escapes.
fn glob(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    glob_at(&pattern, &text)
}

fn glob_at(pattern: &[char], text: &[char]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some('*') => (0..=text.len()).any(|skip| glob_at(&pattern[1..], &text[skip..])),
        Some('?') => !text.is_empty() && glob_at(&pattern[1..], &text[1..]),
        Some('[') => {
            let Some(close) = pattern
                .iter()
                .skip(2)
                .position(|c| *c == ']')
                .map(|i| i + 2)
            else {
                return text.first() == Some(&'[') && glob_at(&pattern[1..], &text[1..]);
            };
            let Some(first) = text.first() else {
                return false;
            };
            let mut class = &pattern[1..close];
            let negated = matches!(class.first(), Some('!' | '^'));
            if negated {
                class = &class[1..];
            }
            let mut found = false;
            let mut index = 0;
            while index < class.len() {
                if index + 2 < class.len() && class[index + 1] == '-' {
                    found |= (class[index]..=class[index + 2]).contains(first);
                    index += 3;
                } else {
                    found |= class[index] == *first;
                    index += 1;
                }
            }
            found != negated && glob_at(&pattern[close + 1..], &text[1..])
        }
        Some('\\') if pattern.len() > 1 => {
            text.first() == Some(&pattern[1]) && glob_at(&pattern[2..], &text[1..])
        }
        Some(c) => text.first() == Some(c) && glob_at(&pattern[1..], &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_files_are_ignored_by_default_and_a_negation_stages_one_back() {
        let rules = IgnoreRules::from_lines(DEFAULT_SEAL_IGNORE.iter().copied());
        for path in [
            "pkg/__pycache__/m.cpython-312.pyc",
            "scripts/__pycache__/x.pyc",
            "a.pyc",
            "target/debug/af",
            "web/node_modules/x/index.js",
        ] {
            assert!(rules.ignores(path), "{path}");
        }
        for path in [
            "crates/fixture/src/target/lib.rs",
            "src/main.py",
            "pyc.md",
            "README.md",
        ] {
            assert!(!rules.ignores(path), "{path}");
        }
        let staged = IgnoreRules::from_lines(
            DEFAULT_SEAL_IGNORE
                .iter()
                .copied()
                .chain(["!fixtures/keep.pyc", "!target/"]),
        );
        assert!(!staged.ignores("fixtures/keep.pyc"));
        assert!(staged.ignores("fixtures/other.pyc"));
        assert!(
            !staged.ignores("target/debug/af"),
            "a negated directory is staged back"
        );
        // A file below an ignored directory stays ignored, as in git.
        let parent = IgnoreRules::from_lines(["build/", "!build/keep.txt"]);
        assert!(parent.ignores("build/keep.txt"));
    }

    #[test]
    fn git_ignore_patterns_match_as_git_matches_them() {
        let rules = IgnoreRules::from_lines([
            "# a comment",
            "/root-only.log",
            "docs/**/draft-*.md",
            "*.[oa]",
            "cache?/",
            "\\#literal",
        ]);
        assert!(rules.ignores("root-only.log"));
        assert!(!rules.ignores("sub/root-only.log"));
        assert!(rules.ignores("docs/draft-1.md"));
        assert!(rules.ignores("docs/a/b/draft-2.md"));
        assert!(!rules.ignores("other/docs/draft-1.md"));
        assert!(rules.ignores("x/y.o") && rules.ignores("lib.a") && !rules.ignores("lib.so"));
        assert!(rules.ignores("cache1/file") && !rules.ignores("cache1"));
        assert!(rules.ignores("#literal"));
    }

    #[test]
    fn a_nested_gitignore_applies_below_its_directory_only() {
        let tree = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tree.path().join("sub")).unwrap();
        std::fs::create_dir_all(tree.path().join(".af")).unwrap();
        std::fs::write(tree.path().join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::write(tree.path().join("sub/.gitignore"), "local.txt\n!keep.tmp\n").unwrap();
        std::fs::write(tree.path().join(".af/seal-ignore"), "*.snap\n").unwrap();
        let entry = |path: &str| review_source_git::Entry {
            path: path.into(),
            kind: EntryKind::File,
            content: review_source_git::digest_bytes(b""),
            size: 0,
        };
        let manifest = Manifest::new(vec![
            entry(".af/seal-ignore"),
            entry(".gitignore"),
            entry("sub/.gitignore"),
        ])
        .unwrap();
        let rules = IgnoreRules::of_tree(tree.path(), &manifest).unwrap();
        assert!(rules.ignores("a.tmp") && rules.ignores("sub/b.tmp"));
        assert!(
            !rules.ignores("sub/keep.tmp"),
            "the deeper file's negation wins below it"
        );
        assert!(rules.ignores("sub/local.txt") && !rules.ignores("local.txt"));
        assert!(rules.ignores("x/y.snap"));
        assert!(
            rules.ignores("m/__pycache__/n.pyc"),
            "the defaults come first"
        );
    }
}
