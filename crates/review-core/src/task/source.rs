//! A captured issue supplies business data, never execution authority.
use super::{present_option, require};
use crate::is_digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const TASK_SOURCE_CAPTURE_V1: &str = "af/TaskSourceCapture@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedRequirementsV1 {
    pub text: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub specification: Option<serde_json::Map<String, serde_json::Value>>,
}
impl NormalizedRequirementsV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            bounded_text(&self.text, 262144),
            "Requirements need bounded nonempty text",
        )?;
        if let Some(spec) = &self.specification {
            require(
                !spec.is_empty()
                    && serde_json::to_vec(spec).map_err(|e| e.to_string())?.len() <= 65536,
                "Structured requirements must be nonempty and at most 64 KiB",
            )?;
        }
        Ok(())
    }
}

pub fn bounded_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= limit
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSourceAdapterV1 {
    LocalIssue,
    JiraCloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSourceFieldV1 {
    /// Canonical JSON value captured from this response, before normalization.
    pub value_id: String,
    /// Exact normalized UTF-8 bytes supplied to requirement construction.
    pub text_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSourceCaptureV1 {
    pub schema: String,
    pub adapter: TaskSourceAdapterV1,
    pub locator: String,
    pub external_id: String,
    pub external_key: String,
    /// Source-provided revision label. Jira's updated timestamp is not immutable versioning.
    pub source_revision: String,
    pub raw_source_id: String,
    pub fields: BTreeMap<String, TaskSourceFieldV1>,
}
impl TaskSourceCaptureV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.task-source-capture/1"
                && bounded_text(&self.locator, 2048)
                && !self.locator.contains(['\n', '\t'])
                && bounded_text(&self.external_id, 128)
                && bounded_text(&self.external_key, 128)
                && bounded_text(&self.source_revision, 256)
                && ![&self.external_id, &self.external_key, &self.source_revision]
                    .iter()
                    .any(|s| s.contains(['\n', '\t']))
                && is_digest(&self.raw_source_id)
                && (2..=18).contains(&self.fields.len())
                && self.fields.contains_key("summary")
                && self.fields.contains_key("description"),
            "Invalid captured Task source identity or field set",
        )?;
        for (name, field) in &self.fields {
            require(
                super::is_name(name) && is_digest(&field.value_id) && is_digest(&field.text_id),
                "Captured source fields require exact value and normalized text identities",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueInputV1 {
    pub schema: String,
    pub id: String,
    pub key: String,
    pub revision: String,
    pub summary: String,
    pub description: String,
    #[serde(default)]
    pub acceptance: BTreeMap<String, String>,
}
impl IssueInputV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.issue-input/1"
                && bounded_text(&self.id, 128)
                && bounded_text(&self.key, 128)
                && bounded_text(&self.revision, 256)
                && ![&self.id, &self.key, &self.revision]
                    .iter()
                    .any(|s| s.contains(['\n', '\t']))
                && bounded_text(&self.summary, 4096)
                && bounded_text(&self.description, 131072)
                && self.acceptance.len() <= 16
                && self.acceptance.iter().all(|(k, v)| {
                    super::is_name(k)
                        && !matches!(k.as_str(), "summary" | "description")
                        && bounded_text(v, 65536)
                }),
            "Issue requires bounded identity, summary, description and acceptance fields",
        )?;
        self.requirements(None).validate()
    }
    pub fn fields(&self) -> BTreeMap<String, String> {
        let mut fields = self.acceptance.clone();
        fields.insert("summary".into(), self.summary.clone());
        fields.insert("description".into(), self.description.clone());
        fields
    }
    pub fn requirements(
        &self,
        specification: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> NormalizedRequirementsV1 {
        let mut text = format!("{}\n\n{}", self.summary, self.description);
        for (name, value) in &self.acceptance {
            text.push_str(&format!("\n\n{name}:\n{value}"));
        }
        NormalizedRequirementsV1 {
            text,
            specification,
        }
    }
}
