//! Source replacement ends at normalized data. This crate cannot schedule or dispatch Workers.
pub mod adf;
pub mod jira;
use review_core::task::source::*;
use review_store::Cas;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    Invalid(String),
    Unavailable,
    Unauthorized,
    NotFound,
    RateLimited,
    TooLarge,
    Cancelled,
    TimedOut,
}
impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "Invalid Task source: {reason}"),
            Self::Unavailable => write!(f, "Task source is unavailable"),
            Self::Unauthorized => write!(f, "Task source binding is not authorized"),
            Self::NotFound => write!(f, "Task source was not found"),
            Self::RateLimited => write!(f, "Task source rate limit reached; no automatic retry"),
            Self::TooLarge => write!(f, "Task source exceeds its byte bound"),
            Self::Cancelled => write!(f, "Task source capture cancelled"),
            Self::TimedOut => write!(f, "Task source capture exceeded its deadline"),
        }
    }
}
impl std::error::Error for SourceError {}
pub struct SourceControl<'a> {
    pub deadline: Instant,
    pub cancelled: &'a AtomicBool,
}
impl SourceControl<'_> {
    pub fn check(&self) -> Result<(), SourceError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(SourceError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(SourceError::TimedOut)
        } else {
            Ok(())
        }
    }
}
/// Replacement adapters return captured data, without accepting policy or budget mutations.
pub trait TaskSource {
    fn read(&self, control: &SourceControl<'_>) -> Result<SourceData, SourceError>;
}
pub struct SourceData {
    pub adapter: TaskSourceAdapterV1,
    pub locator: String,
    pub raw: Vec<u8>,
    pub issue: IssueInput,
    pub field_values: BTreeMap<String, Value>,
}
pub use review_core::task::source::IssueInputV1 as IssueInput;
#[derive(Debug, Clone, Copy)]
pub enum LocalFormat {
    Json,
    Toml,
}
pub struct LocalIssueSource<'a> {
    pub bytes: &'a [u8],
    pub locator: &'a str,
    pub format: LocalFormat,
}
impl TaskSource for LocalIssueSource<'_> {
    fn read(&self, control: &SourceControl<'_>) -> Result<SourceData, SourceError> {
        control.check()?;
        if self.bytes.len() > MAX_SOURCE_BYTES {
            return Err(SourceError::TooLarge);
        }
        let issue: IssueInput = match self.format {
            LocalFormat::Json => serde_json::from_slice(self.bytes)
                .map_err(|_| SourceError::Invalid("Malformed issue JSON".into()))?,
            LocalFormat::Toml => toml::from_str(
                std::str::from_utf8(self.bytes)
                    .map_err(|_| SourceError::Invalid("Issue TOML is not UTF-8".into()))?,
            )
            .map_err(|_| SourceError::Invalid("Malformed issue TOML".into()))?,
        };
        issue.validate().map_err(SourceError::Invalid)?;
        control.check()?;
        Ok(SourceData {
            adapter: TaskSourceAdapterV1::LocalIssue,
            locator: self.locator.into(),
            raw: self.bytes.to_vec(),
            field_values: issue
                .fields()
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect(),
            issue,
        })
    }
}
impl SourceData {
    /// Publish exact raw bytes plus each selected field. Callers reference this capture from
    /// the normalized Requirements envelope; Workers receive only their declared payload.
    pub fn capture(&self, cas: &Cas) -> Result<TaskSourceCaptureV1, SourceError> {
        self.issue.validate().map_err(SourceError::Invalid)?;
        if self.raw.len() > MAX_SOURCE_BYTES {
            return Err(SourceError::TooLarge);
        }
        let normalized = self.issue.fields();
        if normalized.keys().ne(self.field_values.keys()) {
            return Err(SourceError::Invalid(
                "Source field capture is incomplete".into(),
            ));
        }
        let mut fields = BTreeMap::new();
        for (name, value) in &self.field_values {
            fields.insert(
                name.clone(),
                TaskSourceFieldV1 {
                    value_id: cas
                        .put_json(value)
                        .map_err(|e| SourceError::Invalid(e.to_string()))?,
                    text_id: cas
                        .put(normalized[name].as_bytes())
                        .map_err(|e| SourceError::Invalid(e.to_string()))?,
                },
            );
        }
        let capture = TaskSourceCaptureV1 {
            schema: "af.task-source-capture/1".into(),
            adapter: self.adapter.clone(),
            locator: self.locator.clone(),
            external_id: self.issue.id.clone(),
            external_key: self.issue.key.clone(),
            source_revision: self.issue.revision.clone(),
            raw_source_id: cas
                .put(&self.raw)
                .map_err(|e| SourceError::Invalid(e.to_string()))?,
            fields,
        };
        capture.validate().map_err(SourceError::Invalid)?;
        Ok(capture)
    }
}
