//! Read-only Jira Cloud transport. Credentials never enter argv, capture, diagnostics or Workers.
use crate::*;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::Command,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JiraSelector {
    pub site: String,
    pub key: String,
    #[serde(default)]
    pub acceptance_fields: Vec<String>,
}
impl JiraSelector {
    pub fn validate(&self) -> Result<(), SourceError> {
        let site = self.site.strip_suffix(".atlassian.net").unwrap_or("");
        let (project, number) = self.key.rsplit_once('-').unwrap_or(("", ""));
        if site.is_empty()
            || site.len() > 63
            || site.starts_with('-')
            || site.ends_with('-')
            || !site
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || project.is_empty()
            || project.len() > 64
            || !project.as_bytes()[0].is_ascii_uppercase()
            || !project
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || number.is_empty()
            || number.len() > 20
            || number.starts_with('0')
            || !number.bytes().all(|b| b.is_ascii_digit())
            || self.acceptance_fields.len() > 16
            || self.acceptance_fields.windows(2).any(|p| p[0] >= p[1])
            || self.acceptance_fields.iter().any(|f| {
                !f.strip_prefix("customfield_").is_some_and(|n| {
                    !n.is_empty() && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit())
                })
            })
        {
            return Err(SourceError::Invalid(
                "Jira requires an exact Cloud tenant, issue key and sorted unique custom field IDs"
                    .into(),
            ));
        }
        Ok(())
    }
    pub fn url(&self) -> Result<String, SourceError> {
        self.validate()?;
        let mut fields = vec![
            "summary".to_string(),
            "description".into(),
            "updated".into(),
        ];
        fields.extend(self.acceptance_fields.iter().cloned());
        Ok(format!(
            "https://{}/rest/api/3/issue/{}?fields={}",
            self.site,
            self.key,
            fields.join(",")
        ))
    }
}
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}
pub trait JiraTransport {
    fn get_issue(
        &self,
        selector: &JiraSelector,
        control: &SourceControl<'_>,
    ) -> Result<HttpResponse, SourceError>;
}
pub struct JiraSource<'a> {
    pub selector: &'a JiraSelector,
    pub transport: &'a dyn JiraTransport,
}
impl TaskSource for JiraSource<'_> {
    fn read(&self, control: &SourceControl<'_>) -> Result<SourceData, SourceError> {
        control.check()?;
        let url = self.selector.url()?;
        let response = self.transport.get_issue(self.selector, control)?;
        control.check()?;
        match response.status {
            200 => (),
            401 | 403 => return Err(SourceError::Unauthorized),
            404 => return Err(SourceError::NotFound),
            429 => return Err(SourceError::RateLimited),
            _ => return Err(SourceError::Unavailable),
        }
        if response.body.len() > MAX_SOURCE_BYTES {
            return Err(SourceError::TooLarge);
        }
        let object: Value = serde_json::from_slice(&response.body)
            .map_err(|_| SourceError::Invalid("Malformed Jira response".into()))?;
        let string = |v: Option<&Value>| {
            v.and_then(Value::as_str).map(str::to_owned).ok_or_else(|| {
                SourceError::Invalid("Jira response omitted a required issue field".into())
            })
        };
        let fields = object
            .get("fields")
            .and_then(Value::as_object)
            .ok_or_else(|| SourceError::Invalid("Jira response omitted fields".into()))?;
        let id = string(object.get("id"))?;
        let key = string(object.get("key"))?;
        if key != self.selector.key || id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SourceError::Invalid(
                "Jira response does not identify the selected issue".into(),
            ));
        }
        let revision = string(fields.get("updated"))?;
        let summary = string(fields.get("summary"))?;
        let description = field_text(fields.get("description"))?;
        let mut acceptance = BTreeMap::new();
        let mut field_values = BTreeMap::from([
            ("summary".into(), fields["summary"].clone()),
            ("description".into(), fields["description"].clone()),
        ]);
        for name in &self.selector.acceptance_fields {
            acceptance.insert(name.clone(), field_text(fields.get(name))?);
            field_values.insert(name.clone(), fields[name].clone());
        }
        let issue = IssueInput {
            schema: "af.issue-input/1".into(),
            id,
            key,
            revision,
            summary,
            description,
            acceptance,
        };
        issue.validate().map_err(SourceError::Invalid)?;
        Ok(SourceData {
            adapter: TaskSourceAdapterV1::JiraCloud,
            locator: url,
            raw: response.body,
            issue,
            field_values,
        })
    }
}
fn field_text(value: Option<&Value>) -> Result<String, SourceError> {
    match value {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(v @ Value::Object(_)) => crate::adf::plain_text(v),
        _ => Err(SourceError::Invalid(
            "Selected Jira requirement field is missing or unsupported".into(),
        )),
    }
}

/// Intentionally neither Debug nor Serialize. Values are only delivered to the owned transport.
pub struct JiraCredentials {
    email: String,
    token: String,
}
impl JiraCredentials {
    pub fn new(email: String, token: String) -> Result<Self, SourceError> {
        if !email.contains('@')
            || email.contains(':')
            || [&email, &token]
                .iter()
                .any(|v| v.is_empty() || v.len() > 4096 || v.chars().any(|c| c.is_control()))
        {
            return Err(SourceError::Invalid(
                "Invalid local Jira credentials".into(),
            ));
        }
        Ok(Self { email, token })
    }
    fn curl_config(&self) -> Vec<u8> {
        let value = format!("{}:{}", self.email, self.token)
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        format!("user = \"{value}\"\n").into_bytes()
    }
}
pub struct CurlJiraTransport {
    pub program: PathBuf,
    pub credentials: JiraCredentials,
}
impl JiraTransport for CurlJiraTransport {
    fn get_issue(
        &self,
        selector: &JiraSelector,
        control: &SourceControl<'_>,
    ) -> Result<HttpResponse, SourceError> {
        control.check()?;
        let url = selector.url()?;
        if !self.program.is_absolute() {
            return Err(SourceError::Invalid(
                "Jira transport requires an absolute curl executable".into(),
            ));
        }
        let isolated = tempfile::tempdir().map_err(|_| SourceError::Unavailable)?;
        let timeout = control
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(30));
        let mut command = Command::new(&self.program);
        command
            .env_clear()
            .env("HOME", isolated.path())
            .env("LC_ALL", "C")
            .current_dir(isolated.path())
            .args([
                "--disable",
                "--silent",
                "--globoff",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--proxy",
                "",
                "--noproxy",
                "*",
                "--max-redirs",
                "0",
                "--request",
                "GET",
                "--header",
                "Accept: application/json",
                "--connect-timeout",
                "5",
                "--max-time",
            ])
            .arg(format!("{:.3}", timeout.as_secs_f64()))
            .arg("--max-filesize")
            .arg(MAX_SOURCE_BYTES.to_string())
            .args(["--config", "-", "--write-out", "\n%{http_code}", "--url"])
            .arg(url);
        let credentials = self.credentials.curl_config();
        let result = review_process::run_supervised_duplex_cancellable(
            &mut command,
            timeout,
            review_process::ExitPolicy::KillProcessGroup,
            control.cancelled,
            move |stdin: &mut dyn Write| stdin.write_all(&credentials),
            |stdout: &mut dyn Read| {
                let mut bytes = Vec::new();
                stdout
                    .take((MAX_SOURCE_BYTES + 5) as u64)
                    .read_to_end(&mut bytes)
                    .map(|_| bytes)
            },
        );
        let output = result.map_err(|error| match error {
            review_process::SupervisedError::Cancelled => SourceError::Cancelled,
            review_process::SupervisedError::TimedOut { .. } => SourceError::TimedOut,
            _ => SourceError::Unavailable,
        })?;
        control.check()?;
        let bytes = output.output.map_err(|_| SourceError::Unavailable)?;
        if bytes.len() > MAX_SOURCE_BYTES + 4 || output.status.code() == Some(63) {
            return Err(SourceError::TooLarge);
        }
        if !output.status.success() || output.input.is_err() || output.stderr_held {
            return Err(if output.status.code() == Some(28) {
                SourceError::TimedOut
            } else {
                SourceError::Unavailable
            });
        }
        let split = bytes.len().checked_sub(4).ok_or(SourceError::Unavailable)?;
        if bytes[split] != b'\n' {
            return Err(SourceError::Unavailable);
        }
        let status = std::str::from_utf8(&bytes[split + 1..])
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .filter(|n| (100..=599).contains(n))
            .ok_or(SourceError::Unavailable)?;
        Ok(HttpResponse {
            status,
            body: bytes[..split].to_vec(),
        })
    }
}
