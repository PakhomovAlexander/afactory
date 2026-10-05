//! The operator's machine-local Remote Check mapping (ADR-0140 §3.2): per repository, where
//! this machine may push the gate branches. It names push targets only: a Task pipeline's check
//! node chooses which checks run remotely. It is the operator's authorization, never committed
//! policy, so nothing here is recorded: the push URL and the mapping's path stay out of every
//! artifact, event and message.

use std::collections::BTreeSet;
use std::path::Path;

use review_core::task::remote_check::is_github_name;
use serde::Deserialize;

const MAX_MAPPING_BYTES: u64 = 64 * 1024;
const MAX_PUSH_URL_BYTES: usize = 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingFile {
    version: u32,
    #[serde(default)]
    github_pr: Vec<MappingEntry>,
}

/// One `[[github_pr]]` entry as written. `checks` is read only to refuse it by name: RC1's
/// mapping selected checks, and a file that still does must not be mistaken for a target.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingEntry {
    repository_id: String,
    github: String,
    push_url: String,
    #[serde(default)]
    checks: Option<toml::Value>,
}

/// One `[[github_pr]]` target: the repository it maps, the GitHub `owner/name` that `gh`
/// addresses, and the Git URL the gate branches are pushed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubPrTarget {
    pub repository_id: String,
    pub github: String,
    pub push_url: String,
}

/// A validated mapping file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCheckMapping {
    entries: Vec<GithubPrTarget>,
}

/// Where the operator keeps the mapping, named the same way in every message so that no
/// message ever needs the path itself.
pub const MAPPING_KNOB: &str = "the remote check mapping (AF_TASK_REMOTE_CHECK_POLICY_FILE or $XDG_CONFIG_HOME/af/remote-checks.toml)";

impl RemoteCheckMapping {
    /// Read the mapping at `path` with the no-follow, bounded reader the Rust toolchain mapping
    /// uses. `None` when the file does not exist: this machine then has no push target. Anything
    /// else that is not a valid mapping is an error.
    pub fn read(path: &Path) -> Result<Option<Self>, String> {
        if !path.is_absolute() {
            return Err(format!("{MAPPING_KNOB} must be an absolute path"));
        }
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "{MAPPING_KNOB} cannot be inspected: {}",
                    error.kind()
                ));
            }
            Ok(_) => {}
        }
        let bytes = review_sandbox::toolchain::read_toolchain_declaration(path, MAX_MAPPING_BYTES)
            .map_err(|error| {
                format!(
                    "{MAPPING_KNOB} cannot be read: {}",
                    error
                        .replace("toolchain declaration", "mapping")
                        .replace("Rust toolchain source", "mapping path")
                )
            })?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| format!("{MAPPING_KNOB} is not UTF-8"))?;
        Self::parse(text).map(Some)
    }

    /// Parse and validate mapping text. A refused entry is named by its repository, never by its
    /// push URL, and a TOML error never quotes the document.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: MappingFile = toml::from_str(text).map_err(|error| {
            format!(
                "{MAPPING_KNOB} is not a valid mapping: {}",
                error.message().lines().next().unwrap_or("malformed TOML")
            )
        })?;
        if file.version != 1 {
            return Err(format!(
                "{MAPPING_KNOB} declares version {}, not 1",
                file.version
            ));
        }
        let mut repositories = BTreeSet::new();
        let mut entries = Vec::with_capacity(file.github_pr.len());
        for entry in file.github_pr {
            let name = &entry.repository_id;
            if !is_repository_id(name) {
                return Err(format!(
                    "{MAPPING_KNOB} has an entry whose repository_id is not a sorted, \
                     comma-separated list of root commits; write `git rev-list --max-parents=0 \
                     HEAD` sorted and joined by commas"
                ));
            }
            if entry.checks.is_some() {
                return Err(format!(
                    "{MAPPING_KNOB} entry for repository {name} carries `checks`, which selects \
                     nothing any more: a Task pipeline's check node chooses where a check runs, \
                     by listing it in `remote_checks`; remove `checks` from the mapping and plan \
                     a pipeline whose check node lists the remote checks"
                ));
            }
            if !repositories.insert(name.clone()) {
                return Err(format!(
                    "{MAPPING_KNOB} maps repository {name} twice; keep one [[github_pr]] entry"
                ));
            }
            if !is_github_name(&entry.github) {
                return Err(format!(
                    "{MAPPING_KNOB} entry for repository {name}: `github` must be `owner/name`"
                ));
            }
            push_url_admissible(&entry.push_url).map_err(|why| {
                format!("{MAPPING_KNOB} entry for repository {name}: `push_url` {why}")
            })?;
            entries.push(GithubPrTarget {
                repository_id: entry.repository_id,
                github: entry.github,
                push_url: entry.push_url,
            });
        }
        Ok(Self { entries })
    }

    /// The push target for `repository_id`, `None` when the mapping does not name it.
    pub fn target(&self, repository_id: &str) -> Option<&GithubPrTarget> {
        self.entries
            .iter()
            .find(|entry| entry.repository_id == repository_id)
    }
}

fn is_repository_id(value: &str) -> bool {
    let roots: Vec<&str> = value.split(',').collect();
    value.len() <= 4096
        && roots.iter().all(|root| {
            (root.len() == 40 || root.len() == 64)
                && root
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        && roots.windows(2).all(|pair| pair[0] < pair[1])
}

/// A push URL may not carry user information: no password anywhere, and no user name except
/// the SSH login of an `ssh://` or scp-like `login@host:path` URL, which names an account and
/// not a credential. Authentication is the operator's ambient Git configuration.
pub(crate) fn push_url_admissible(url: &str) -> Result<(), &'static str> {
    if url.is_empty() || url.len() > MAX_PUSH_URL_BYTES {
        return Err("must be 1 to 1024 bytes");
    }
    if url
        .bytes()
        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err("may not contain whitespace or control characters");
    }
    if url.starts_with('-') {
        return Err("may not begin with `-`");
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if let Some((user, _)) = authority.rsplit_once('@') {
            let ssh = scheme.eq_ignore_ascii_case("ssh") || scheme.eq_ignore_ascii_case("git+ssh");
            if !ssh || user.is_empty() || user.contains(':') || user.contains('%') {
                return Err(
                    "carries user information; remove it and authenticate through Git's \
                     credential helper or SSH",
                );
            }
        }
        return Ok(());
    }
    // scp-like `[login@]host:path`; anything else is a local path.
    let head = url.split('/').next().unwrap_or_default();
    if head.contains(':')
        && let Some((user, _)) = head.rsplit_once('@')
        && (user.is_empty() || user.contains(':') || user.contains('%'))
    {
        return Err(
            "carries user information; remove it and authenticate through Git's credential \
             helper or SSH",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "5f1c000000000000000000000000000000000000";

    fn mapping(push_url: &str) -> String {
        format!(
            "version = 1\n[[github_pr]]\nrepository_id = \"{ROOT}\"\ngithub = \"o/r\"\n\
             push_url = \"{push_url}\"\n"
        )
    }

    #[test]
    fn a_valid_mapping_names_a_push_target_per_repository() {
        let parsed = RemoteCheckMapping::parse(&mapping("git@github.com:o/r.git")).unwrap();
        assert_eq!(
            parsed.target(ROOT),
            Some(&GithubPrTarget {
                repository_id: ROOT.into(),
                github: "o/r".into(),
                push_url: "git@github.com:o/r.git".into(),
            })
        );
        assert_eq!(parsed.target(&"6".repeat(40)), None);
        let empty = RemoteCheckMapping::parse("version = 1\n").unwrap();
        assert!(empty.entries.is_empty());
    }

    #[test]
    fn a_mapping_that_still_selects_checks_is_refused_and_names_the_check_node() {
        for checks in ["[\"kernel\"]", "[]", "\"kernel\""] {
            let text = format!("{}checks = {checks}\n", mapping("git@github.com:o/r.git"));
            let error = RemoteCheckMapping::parse(&text).unwrap_err();
            assert!(error.contains("carries `checks`"), "{error}");
            assert!(
                error.contains("pipeline's check node") && error.contains("`remote_checks`"),
                "{error}"
            );
            assert!(
                error.contains(ROOT) && !error.contains("git@github.com"),
                "{error}"
            );
        }
    }

    #[test]
    fn user_information_in_the_push_url_is_refused_without_quoting_it() {
        for url in [
            "https://user:s3cret-token@github.com/o/r.git",
            "https://s3cret-token@github.com/o/r.git",
            "http://x-access-token:s3cret-token@github.com/o/r.git",
            "ssh://git:s3cret-token@github.com/o/r.git",
            "git:s3cret-token@github.com:o/r.git",
            "%73ecret@github.com:o/r.git",
        ] {
            let error = RemoteCheckMapping::parse(&mapping(url)).unwrap_err();
            assert!(error.contains("user information"), "{url}: {error}");
            assert!(
                !error.contains("s3cret") && !error.contains("ecret@"),
                "{error}"
            );
            assert!(!error.contains(url), "{error}");
        }
        for url in [
            "git@github.com:o/r.git",
            "ssh://git@github.com/o/r.git",
            "https://github.com/o/r.git",
            "/srv/git/gate.git",
            "file:///srv/git/gate.git",
        ] {
            RemoteCheckMapping::parse(&mapping(url)).unwrap();
        }
    }

    #[test]
    fn malformed_mappings_are_errors() {
        for text in [
            "version = 2\n".to_string(),
            mapping("--upload-pack=x"),
            mapping("git@github.com:o/r.git").replace("o/r\"", "o\""),
            mapping("git@github.com:o/r.git").replace(ROOT, "HEAD"),
            format!(
                "{}{}",
                mapping("a:b"),
                mapping("a:b").replace("version = 1\n", "")
            ),
            mapping("git@github.com:o/r.git") + "token = \"x\"\n",
        ] {
            assert!(RemoteCheckMapping::parse(&text).is_err(), "{text}");
        }
    }

    #[test]
    fn an_absent_file_is_no_mapping_and_a_relative_path_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        assert_eq!(
            RemoteCheckMapping::read(&root.join("remote-checks.toml")).unwrap(),
            None
        );
        assert!(RemoteCheckMapping::read(Path::new("remote-checks.toml")).is_err());
        let file = root.join("real.toml");
        std::fs::write(&file, mapping("git@github.com:o/r.git")).unwrap();
        assert!(RemoteCheckMapping::read(&file).unwrap().is_some());
        let link = root.join("link.toml");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let error = RemoteCheckMapping::read(&link).unwrap_err();
        assert!(!error.contains(&root.display().to_string()), "{error}");
    }
}
