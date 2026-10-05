//! The operator's machine-local Remote Check mapping (ADR-0139 §3.2): per repository, which
//! declared checks this machine hands to a remote executor, and where it may push the gate
//! branches. It is the operator's authorization, never committed policy, so nothing here is
//! recorded: the push URL and the mapping's path stay out of every artifact, event and message.

use std::collections::BTreeSet;
use std::path::Path;

use review_core::task::remote_check::is_github_name;
use serde::Deserialize;

use crate::task::code::CodeTaskPolicy;

const MAX_MAPPING_BYTES: u64 = 64 * 1024;
const MAX_PUSH_URL_BYTES: usize = 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingFile {
    version: u32,
    #[serde(default)]
    github_pr: Vec<GithubPrTarget>,
}

/// One `[[github_pr]]` entry: the repository it maps, the GitHub `owner/name` that `gh`
/// addresses, the Git URL the gate branches are pushed to, and the checks it selects.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubPrTarget {
    pub repository_id: String,
    pub github: String,
    pub push_url: String,
    pub checks: BTreeSet<String>,
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
    /// uses. `None` when the file does not exist: every check then runs locally. Anything else
    /// that is not a valid mapping is an error before any check starts.
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
        for entry in &file.github_pr {
            let name = &entry.repository_id;
            if !is_repository_id(name) {
                return Err(format!(
                    "{MAPPING_KNOB} has an entry whose repository_id is not a sorted, \
                     comma-separated list of root commits; write `git rev-list --max-parents=0 \
                     HEAD` sorted and joined by commas"
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
            if entry.checks.is_empty()
                || entry.checks.len() > 32
                || entry
                    .checks
                    .iter()
                    .any(|check| !review_core::task::is_name(check))
            {
                return Err(format!(
                    "{MAPPING_KNOB} entry for repository {name}: `checks` names 1 to 32 \
                     declared checks"
                ));
            }
        }
        Ok(Self {
            entries: file.github_pr,
        })
    }

    /// The entry for `repository_id`, validated against the captured policy: every check it
    /// names must be declared with a `remote` table. `None` when the mapping does not name the
    /// repository.
    pub fn select(
        &self,
        repository_id: &str,
        policy: &CodeTaskPolicy,
    ) -> Result<Option<&GithubPrTarget>, String> {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.repository_id == repository_id)
        else {
            return Ok(None);
        };
        for name in &entry.checks {
            match policy.checks.get(name) {
                None => {
                    return Err(format!(
                        "{MAPPING_KNOB} selects check `{name}` for repository {repository_id}, \
                         but the captured code policy declares no such check; remove it from \
                         `checks`"
                    ));
                }
                Some(check) if check.remote.is_none() => {
                    return Err(format!(
                        "{MAPPING_KNOB} selects check `{name}` for repository {repository_id}, \
                         but the captured code policy declares it without a `remote` table; \
                         declare [checks.{name}.remote] or remove it from `checks`"
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(Some(entry))
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

    fn mapping(push_url: &str, checks: &str) -> String {
        format!(
            "version = 1\n[[github_pr]]\nrepository_id = \"{ROOT}\"\ngithub = \"o/r\"\n\
             push_url = \"{push_url}\"\nchecks = [{checks}]\n"
        )
    }

    #[test]
    fn a_valid_mapping_selects_its_repository() {
        let parsed =
            RemoteCheckMapping::parse(&mapping("git@github.com:o/r.git", "\"kernel\"")).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].checks, BTreeSet::from(["kernel".into()]));
        let empty = RemoteCheckMapping::parse("version = 1\n").unwrap();
        assert!(empty.entries.is_empty());
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
            let error = RemoteCheckMapping::parse(&mapping(url, "\"kernel\"")).unwrap_err();
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
            RemoteCheckMapping::parse(&mapping(url, "\"kernel\"")).unwrap();
        }
    }

    #[test]
    fn malformed_mappings_are_errors() {
        for text in [
            "version = 2\n".to_string(),
            mapping("git@github.com:o/r.git", ""),
            mapping("git@github.com:o/r.git", "\"bad name\""),
            mapping("--upload-pack=x", "\"kernel\""),
            mapping("git@github.com:o/r.git", "\"kernel\"").replace("o/r\"", "o\""),
            mapping("git@github.com:o/r.git", "\"kernel\"").replace(ROOT, "HEAD"),
            format!(
                "{}{}",
                mapping("a:b", "\"kernel\""),
                mapping("a:b", "\"kernel\"").replace("version = 1\n", "")
            ),
            mapping("git@github.com:o/r.git", "\"kernel\"") + "token = \"x\"\n",
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
        std::fs::write(&file, mapping("git@github.com:o/r.git", "\"kernel\"")).unwrap();
        assert!(RemoteCheckMapping::read(&file).unwrap().is_some());
        let link = root.join("link.toml");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let error = RemoteCheckMapping::read(&link).unwrap_err();
        assert!(!error.contains(&root.display().to_string()), "{error}");
    }
}
