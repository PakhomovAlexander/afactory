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
            JiraSource {
                selector: &selector,
                transport: &transport,
            }
            .read(&control)
        }
    }
    .map_err(|e| e.to_string())?;
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
