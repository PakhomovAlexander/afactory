//! Explicit read-only issue capture. Resuming a Task reads these artifacts, never a live issue.
use super::*;
use review_core::task::source::*;
use review_source_task::{LocalFormat, LocalIssueSource, SourceControl, TaskSource, jira::*};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum IssueSource {
    Local {
        path: String,
    },
    Jira {
        binding: String,
        key: String,
        #[serde(default)]
        acceptance_fields: Vec<String>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceBindings {
    schema: String,
    jira: BTreeMap<String, JiraBinding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JiraBinding {
    site: String,
    email: String,
    token_file: PathBuf,
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "Cannot open local source binding input")?
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read local source binding input")?;
    if bytes.len() as u64 > max {
        return Err("Local source binding input exceeds its byte bound".into());
    }
    Ok(bytes)
}

pub(super) struct CapturedIssue {
    pub capture_id: String,
    pub requirements: NormalizedRequirementsV1,
}
pub(super) fn capture(
    cas: &Cas,
    manifest: &Manifest,
    source: &IssueSource,
    bindings: Option<&Path>,
    specification: Option<serde_json::Map<String, serde_json::Value>>,
    deadline_unix_ms: u64,
) -> Result<CapturedIssue, String> {
    capture_selected(
        cas,
        manifest,
        source,
        bindings,
        specification,
        deadline_unix_ms,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn capture_selected(
    cas: &Cas,
    manifest: &Manifest,
    source: &IssueSource,
    bindings: Option<&Path>,
    specification: Option<serde_json::Map<String, serde_json::Value>>,
    deadline_unix_ms: u64,
    expected_locator: Option<&str>,
) -> Result<CapturedIssue, String> {
    let remaining = deadline_unix_ms
        .checked_sub(clock()?)
        .filter(|n| *n > 0)
        .ok_or("Task deadline expired before source capture")?;
    let cancelled = AtomicBool::new(false);
    let control = SourceControl {
        deadline: Instant::now() + Duration::from_millis(remaining.min(30000)),
        cancelled: &cancelled,
    };
    let data = match source {
        IssueSource::Local { path } => {
            if !review_config::task::shared::safe_relative_path(path) {
                return Err("Issue input must name a captured project-relative file".into());
            }
            let bytes = captured_file(cas, manifest, path)?;
            let format = match Path::new(path).extension().and_then(|s| s.to_str()) {
                Some("json") => LocalFormat::Json,
                Some("toml") => LocalFormat::Toml,
                _ => return Err("Local issue input must be JSON or TOML".into()),
            };
            LocalIssueSource {
                bytes: &bytes,
                locator: path,
                format,
            }
            .read(&control)
        }
        IssueSource::Jira {
            binding,
            key,
            acceptance_fields,
        } => {
            if !is_name(binding) {
                return Err("Invalid Jira source binding name".into());
            }
            let path = bindings.ok_or(
                "Jira capture requires explicit --source-bindings; no ambient account is used",
            )?;
            let settings: SourceBindings = parse(path, &read_bounded(path, 65536)?)?;
            if settings.schema != "af.task-source-bindings/1"
                || settings.jira.is_empty()
                || settings.jira.len() > 32
                || settings.jira.keys().any(|n| !is_name(n))
            {
                return Err("Invalid local Task source binding configuration".into());
            }
            let binding = settings
                .jira
                .get(binding)
                .ok_or("Jira source binding is not configured")?;
            if !binding.token_file.is_absolute() {
                return Err("Jira token file must be an absolute machine-local path".into());
            }
            let token = String::from_utf8(read_bounded(&binding.token_file, 4098)?)
                .map_err(|_| "Invalid local Jira token encoding")?;
            let transport = CurlJiraTransport {
                program: PathBuf::from("/usr/bin/curl"),
                credentials: JiraCredentials::new(
                    binding.email.clone(),
                    token.trim_end_matches(['\r', '\n']).into(),
                )
                .map_err(|e| e.to_string())?,
            };
            let selector = JiraSelector {
                site: binding.site.clone(),
                key: key.clone(),
                acceptance_fields: acceptance_fields.clone(),
            };
            if expected_locator
                .is_some_and(|expected| selector.url().ok().as_deref() != Some(expected))
            {
                return Err(
                    "Jira refresh cannot change the captured tenant, ticket or selected fields"
                        .into(),
                );
            }
            JiraSource {
                selector: &selector,
                transport: &transport,
            }
            .read(&control)
        }
    }
    .map_err(|e| e.to_string())?;
    publish(cas, data, specification)
}

fn publish(
    cas: &Cas,
    data: review_source_task::SourceData,
    specification: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<CapturedIssue, String> {
    let captured = data.capture(cas).map_err(|e| e.to_string())?;
    let requirements = data.issue.requirements(specification);
    requirements.validate()?;
    let mut refs = BTreeSet::from([captured.raw_source_id.clone()]);
    for field in captured.fields.values() {
        refs.insert(field.value_id.clone());
        refs.insert(field.text_id.clone());
    }
    let capture_id = cas
        .put_artifact(
            TASK_SOURCE_CAPTURE_V1,
            producer(),
            refs.into_iter().collect(),
            None,
            serde_json::to_value(captured).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    Ok(CapturedIssue {
        capture_id,
        requirements,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn capture_fresh(
    cas: &Cas,
    repo: &Path,
    source: &IssueSource,
    bindings: Option<&Path>,
    local_file: Option<&Path>,
    specification: Option<serde_json::Map<String, serde_json::Value>>,
    original: &TaskSourceCaptureV1,
) -> Result<CapturedIssue, String> {
    match source {
        IssueSource::Jira { .. } => {
            if local_file.is_some() {
                return Err("--source-file applies only to a local issue source".into());
            }
            capture_selected(
                cas,
                &Manifest::default(),
                source,
                bindings,
                specification,
                clock()?
                    .checked_add(30000)
                    .ok_or("Source deadline overflow")?,
                Some(&original.locator),
            )
        }
        IssueSource::Local { path } => {
            if !review_config::task::shared::safe_relative_path(path) {
                return Err("Invalid captured local issue path".into());
            }
            let path = local_file
                .map(Path::to_path_buf)
                .unwrap_or_else(|| repo.join(path));
            let path = if path.is_absolute() {
                path
            } else {
                repo.join(path)
            };
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|_| "Cannot inspect local issue source")?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err("Local issue source must be a regular file".into());
            }
            let canonical =
                std::fs::canonicalize(&path).map_err(|_| "Cannot resolve local issue source")?;
            if local_file.is_none() && !canonical.starts_with(repo) {
                return Err("Local issue source escaped the project".into());
            }
            let bytes = read_issue_file(repo, &path, local_file.is_none())?;
            let format = match path.extension().and_then(|s| s.to_str()) {
                Some("json") => LocalFormat::Json,
                Some("toml") => LocalFormat::Toml,
                _ => return Err("Local issue input must be JSON or TOML".into()),
            };
            let cancelled = AtomicBool::new(false);
            let control = SourceControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled: &cancelled,
            };
            let locator = canonical
                .to_str()
                .ok_or("Local issue source path must be UTF-8")?;
            let data = LocalIssueSource {
                bytes: &bytes,
                locator,
                format,
            }
            .read(&control)
            .map_err(|e| e.to_string())?;
            publish(cas, data, specification)
        }
    }
}

// Open project-relative components beneath one held directory. A source path cannot turn
// into a symlink to another file between inspection and capture, or block on a FIFO.
fn read_issue_file(repo: &Path, path: &Path, project_relative: bool) -> Result<Vec<u8>, String> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let mut file: std::fs::File = if project_relative {
        let relative = path
            .strip_prefix(repo)
            .map_err(|_| "Issue source escaped the project")?;
        let mut components = relative.components().peekable();
        let mut directory = open(
            repo,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "Cannot open issue source directory")?;
        loop {
            let Some(std::path::Component::Normal(name)) = components.next() else {
                return Err("Invalid issue source path".into());
            };
            let last = components.peek().is_none();
            let flags = OFlags::RDONLY
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK
                | if last {
                    OFlags::empty()
                } else {
                    OFlags::DIRECTORY
                };
            let fd = openat(&directory, name, flags, Mode::empty())
                .map_err(|_| "Issue source path is unavailable or follows a symlink")?;
            if last {
                break fd.into();
            }
            directory = fd;
        }
    } else {
        open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| "Cannot open explicit issue source")?
        .into()
    };
    if !file
        .metadata()
        .map_err(|_| "Cannot inspect opened issue source")?
        .is_file()
    {
        return Err("Issue source must be a regular file".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(review_source_task::MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read local issue source")?;
    if bytes.len() > review_source_task::MAX_SOURCE_BYTES {
        return Err("Local issue source exceeds its byte bound".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn project_issue_reader_is_bounded_regular_and_does_not_follow_symlink_components() {
        let root = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(root.path()).unwrap();
        std::fs::create_dir(repo.join("nested")).unwrap();
        let source = repo.join("nested/issue.json");
        std::fs::write(&source, b"captured").unwrap();
        assert_eq!(read_issue_file(&repo, &source, true).unwrap(), b"captured");
        std::os::unix::fs::symlink(&source, repo.join("issue-link.json")).unwrap();
        std::os::unix::fs::symlink(repo.join("nested"), repo.join("directory-link")).unwrap();
        for path in ["issue-link.json", "directory-link/issue.json", "nested"] {
            assert!(
                read_issue_file(&repo, &repo.join(path), true).is_err(),
                "{path}"
            );
        }
        std::fs::write(
            &source,
            vec![b'x'; review_source_task::MAX_SOURCE_BYTES + 1],
        )
        .unwrap();
        assert!(read_issue_file(&repo, &source, true).is_err());
    }
}
