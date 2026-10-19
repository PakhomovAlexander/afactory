//! The `[survey]` table of `.af/af.toml` (ADR-0152): the bounds and globs a Survey measures by.
//!
//! The table lives in the project layer and deep-merges like every other table of the
//! configuration ladder, so the directory layer above every checkout can set it once, and the
//! generic environment form (`AF_SURVEY__MAX_COMMITS=1`) overrides one key. Every key is typed
//! and an unknown key is refused, as in the rest of `af.toml`. `[survey.languages]` extends the
//! built-in extension map; it never replaces it.

use std::collections::BTreeMap;
use std::path::Path;

use review_core::project::SurveyIdentityPolicyV1;
use serde::Deserialize;
use serde_json::json;

use super::glob;

/// The table as one layer writes it: every key optional, unknown keys refused.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SurveyTable {
    max_wall_ms: Option<u64>,
    max_commits: Option<u64>,
    max_file_bytes: Option<u64>,
    exclude: Option<Vec<String>>,
    tests: Option<Vec<String>>,
    decisions: Option<String>,
    vocabulary: Option<String>,
    release_tags: Option<String>,
    identity: Option<SurveyIdentityPolicyV1>,
    agent_authors: Option<Vec<String>>,
    agent_trailers: Option<Vec<String>>,
    hot_paths: Option<u64>,
    languages: Option<BTreeMap<String, String>>,
}

/// Upper bound on the entries of one glob list or of `[survey.languages]`.
const MAX_ENTRIES: usize = 256;
/// Upper bound on one glob, path or language key.
const MAX_TEXT: usize = 256;

/// The effective policy: the defaults, with every layer's `[survey]` merged over them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Policy {
    pub(crate) max_wall_ms: u64,
    pub(crate) max_commits: u64,
    pub(crate) max_file_bytes: u64,
    pub(crate) exclude: Vec<String>,
    pub(crate) tests: Vec<String>,
    pub(crate) decisions: String,
    pub(crate) vocabulary: String,
    pub(crate) release_tags: String,
    pub(crate) identity: SurveyIdentityPolicyV1,
    pub(crate) agent_authors: Vec<String>,
    pub(crate) agent_trailers: Vec<String>,
    pub(crate) hot_paths: u64,
    /// `[survey.languages]` only; [`BUILT_IN_LANGUAGES`] is the Surveyor's own.
    pub(crate) languages: BTreeMap<String, String>,
}

impl Default for Policy {
    fn default() -> Self {
        let strings = |values: &[&str]| values.iter().map(|value| value.to_string()).collect();
        Self {
            max_wall_ms: 60_000,
            max_commits: 100_000,
            max_file_bytes: 1_048_576,
            exclude: Vec::new(),
            tests: strings(&["**/tests/**", "**/*_test.*", "**/test_*", "**/fixtures/**"]),
            decisions: "docs/adr/[0-9][0-9][0-9][0-9]-*.md".into(),
            vocabulary: "CONTEXT.md".into(),
            release_tags: "v*".into(),
            identity: SurveyIdentityPolicyV1::Name,
            agent_authors: strings(&["*[bot]"]),
            agent_trailers: strings(&["*Claude*", "*Codex*", "*GPT*"]),
            hot_paths: 20,
            languages: BTreeMap::new(),
        }
    }
}

/// The built-in extension map, by file name (`Makefile`) or `*.` and an extension. A file is
/// the language of its exact name, else of its longest extension, else `other`.
pub(crate) const BUILT_IN_LANGUAGES: &[(&str, &str)] = &[
    ("*.bash", "shell"),
    ("*.c", "c"),
    ("*.cc", "cpp"),
    ("*.cjs", "javascript"),
    ("*.cpp", "cpp"),
    ("*.cs", "csharp"),
    ("*.css", "css"),
    ("*.cxx", "cpp"),
    ("*.go", "go"),
    ("*.h", "c"),
    ("*.hpp", "cpp"),
    ("*.htm", "html"),
    ("*.html", "html"),
    ("*.java", "java"),
    ("*.js", "javascript"),
    ("*.json", "json"),
    ("*.jsx", "javascript"),
    ("*.kt", "kotlin"),
    ("*.lua", "lua"),
    ("*.md", "markdown"),
    ("*.mjs", "javascript"),
    ("*.mk", "make"),
    ("*.nix", "nix"),
    ("*.php", "php"),
    ("*.proto", "protobuf"),
    ("*.py", "python"),
    ("*.rb", "ruby"),
    ("*.rs", "rust"),
    ("*.scss", "css"),
    ("*.sh", "shell"),
    ("*.sql", "sql"),
    ("*.svg", "svg"),
    ("*.swift", "swift"),
    ("*.toml", "toml"),
    ("*.ts", "typescript"),
    ("*.tsx", "typescript"),
    ("*.txt", "text"),
    ("*.yaml", "yaml"),
    ("*.yml", "yaml"),
    ("*.zsh", "shell"),
    ("Dockerfile", "docker"),
    ("Makefile", "make"),
];

/// Whether `key` is a `[survey.languages]` key: an exact file name, or `*.` and an extension.
fn is_language_key(key: &str) -> bool {
    let name = key.strip_prefix("*.").unwrap_or(key);
    !name.is_empty()
        && key.len() <= MAX_TEXT
        && !name.contains(['/', '*', '?', '['])
        && !name.chars().any(char::is_control)
}

impl Policy {
    /// The effective policy of the checkout at `toplevel`, from the configuration ladder. The
    /// error is af's own sentence and names no path, so a `SurveyFailed@1` may carry it.
    pub(crate) fn load(toplevel: &Path) -> Result<Policy, String> {
        let config = crate::config::load(Some(toplevel)).map_err(|_| {
            "the configuration does not load; `af config show --origin` names the file".to_string()
        })?;
        Self::from_effective(config.effective.get("survey"))
    }

    /// The policy `[survey]` of the merged configuration describes; absent is every default.
    pub(crate) fn from_effective(table: Option<&toml::Value>) -> Result<Policy, String> {
        let table: SurveyTable = match table {
            None => SurveyTable::default(),
            Some(value) => value.clone().try_into().map_err(|error: toml::de::Error| {
                format!(
                    "the [survey] policy does not load: {}",
                    error.message().trim()
                )
            })?,
        };
        let defaults = Policy::default();
        let mut languages = defaults.languages;
        languages.extend(table.languages.unwrap_or_default());
        let policy = Policy {
            max_wall_ms: table.max_wall_ms.unwrap_or(defaults.max_wall_ms),
            max_commits: table.max_commits.unwrap_or(defaults.max_commits),
            max_file_bytes: table.max_file_bytes.unwrap_or(defaults.max_file_bytes),
            exclude: table.exclude.unwrap_or(defaults.exclude),
            tests: table.tests.unwrap_or(defaults.tests),
            decisions: table.decisions.unwrap_or(defaults.decisions),
            vocabulary: table.vocabulary.unwrap_or(defaults.vocabulary),
            release_tags: table.release_tags.unwrap_or(defaults.release_tags),
            identity: table.identity.unwrap_or(defaults.identity),
            agent_authors: table.agent_authors.unwrap_or(defaults.agent_authors),
            agent_trailers: table.agent_trailers.unwrap_or(defaults.agent_trailers),
            hot_paths: table.hot_paths.unwrap_or(defaults.hot_paths),
            languages,
        };
        policy
            .validate()
            .map_err(|error| format!("the [survey] policy does not load: {error}"))?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_wall_ms == 0 || self.max_commits == 0 {
            return Err("max_wall_ms and max_commits are at least 1".into());
        }
        if self.max_wall_ms > 86_400_000 {
            return Err("max_wall_ms is at most 86400000 (one day)".into());
        }
        if self.hot_paths > review_core::project::MAX_SURVEY_HOT_PATHS as u64 {
            return Err(format!(
                "hot_paths is at most {}",
                review_core::project::MAX_SURVEY_HOT_PATHS
            ));
        }
        for (key, globs) in [
            ("exclude", &self.exclude),
            ("tests", &self.tests),
            ("agent_authors", &self.agent_authors),
            ("agent_trailers", &self.agent_trailers),
        ] {
            if globs.len() > MAX_ENTRIES {
                return Err(format!("{key} holds at most {MAX_ENTRIES} globs"));
            }
            for pattern in globs {
                if pattern.is_empty() || pattern.len() > MAX_TEXT {
                    return Err(format!("{key} holds globs of 1 to {MAX_TEXT} bytes"));
                }
            }
        }
        for (key, pattern) in self
            .exclude
            .iter()
            .chain(&self.tests)
            .map(|pattern| ("exclude or tests", pattern))
            .chain([("decisions", &self.decisions)])
        {
            glob::validate_path_glob(pattern).map_err(|error| format!("{key}: {error}"))?;
        }
        if self.release_tags.is_empty() || self.release_tags.len() > MAX_TEXT {
            return Err(format!("release_tags is a glob of 1 to {MAX_TEXT} bytes"));
        }
        if !review_core::is_valid_repo_path(&self.vocabulary) || self.vocabulary.len() > MAX_TEXT {
            return Err("vocabulary is a repository path such as \"CONTEXT.md\"".into());
        }
        if self.languages.len() > MAX_ENTRIES {
            return Err(format!(
                "[survey.languages] holds at most {MAX_ENTRIES} entries"
            ));
        }
        for (key, language) in &self.languages {
            if !is_language_key(key) {
                return Err(format!(
                    "[survey.languages] key `{key}` is a file name or `*.` and an extension"
                ));
            }
            if !review_core::project::is_language(language) {
                return Err(format!(
                    "[survey.languages] `{key}` names a language of lowercase letters, digits \
                     and `+#._-`"
                ));
            }
        }
        Ok(())
    }

    /// The policy as canonical JSON: what `policy_digest` hashes. Numbers are decimal text,
    /// like every number of a Survey.
    pub(crate) fn document(&self) -> serde_json::Value {
        json!({
            "max_wall_ms": self.max_wall_ms.to_string(),
            "max_commits": self.max_commits.to_string(),
            "max_file_bytes": self.max_file_bytes.to_string(),
            "exclude": self.exclude,
            "tests": self.tests,
            "decisions": self.decisions,
            "vocabulary": self.vocabulary,
            "release_tags": self.release_tags,
            "identity": self.identity.as_str(),
            "agent_authors": self.agent_authors,
            "agent_trailers": self.agent_trailers,
            "hot_paths": self.hot_paths.to_string(),
            "languages": self.languages,
        })
    }

    /// `sha256:` + hex(SHA-256 of the canonical JSON of the effective policy).
    pub(crate) fn digest(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        let bytes = super::canonical_bytes(&self.document())?;
        Ok(format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(&bytes))
        ))
    }

    /// The language of a file by its name: `[survey.languages]` first, then the built-in map;
    /// the exact name, then the longest extension; `other` when neither names it.
    pub(crate) fn language(&self, name: &str) -> String {
        let lookup = |key: &str| {
            self.languages.get(key).cloned().or_else(|| {
                BUILT_IN_LANGUAGES
                    .iter()
                    .find(|(built_in, _)| *built_in == key)
                    .map(|(_, language)| language.to_string())
            })
        };
        if let Some(language) = lookup(name) {
            return language;
        }
        for (index, _) in name.match_indices('.') {
            if let Some(language) = lookup(&format!("*{}", &name[index..])) {
                return language;
            }
        }
        "other".into()
    }
}
