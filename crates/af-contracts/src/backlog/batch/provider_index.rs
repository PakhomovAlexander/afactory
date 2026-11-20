//! Bounded, private GitHub observation supplied by the Project receiver.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::backlog::valid_text;
use crate::json::SAFE_INTEGER_MAX;

pub const MAX_INDEX_BYTES: u64 = 64 * 1024;
pub const MAX_ISSUES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub schema: Schema,
    pub observation: Walk,
    pub repository: String,
    pub repository_id: u64,
    pub destination: String,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schema {
    #[serde(rename = "af.backlog-open-issues/1")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Walk {
    #[serde(rename = "live-page-walk")]
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub id: u64,
    pub number: u64,
    pub updated_at: String,
    pub title: String,
    pub body_excerpt: String,
    pub body_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderMatch {
    pub issue_id: u64,
    pub quote: String,
}

fn numeric_id(id: u64) -> bool {
    (1..=SAFE_INTEGER_MAX as u64).contains(&id)
}

fn repository_name(name: &str) -> bool {
    name.split_once('/').is_some_and(|(owner, repo)| {
        (1..=39).contains(&owner.len())
            && owner.as_bytes()[0].is_ascii_alphanumeric()
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && (1..=100).contains(&repo.len())
            && !matches!(repo, "." | "..")
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    })
}

fn valid_revision(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || !bytes
            .iter()
            .zip(b"dddd-dd-ddTdd:dd:ddZ")
            .all(|(got, expected)| {
                if *expected == b'd' {
                    got.is_ascii_digit()
                } else {
                    got == expected
                }
            })
    {
        return false;
    }
    let number = |start, end| value[start..end].parse::<u32>().unwrap_or(0);
    let year = number(0, 4);
    let days = match number(5, 7) {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    };
    year > 0
        && (1..=days).contains(&number(8, 10))
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60
}

impl Observation {
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = BTreeSet::new();
        if !repository_name(&self.repository)
            || !numeric_id(self.repository_id)
            || self.destination != format!("github:{}", self.repository)
            || self.issues.len() > MAX_ISSUES
            || !self
                .issues
                .windows(2)
                .all(|pair| pair[0].number < pair[1].number)
            || !self.issues.iter().all(|issue| {
                numeric_id(issue.id)
                    && numeric_id(issue.number)
                    && ids.insert(issue.id)
                    && valid_revision(&issue.updated_at)
                    && (1..=512).contains(&issue.title.chars().count())
                    && valid_text(&issue.title, 2048, false)
                    && issue.body_excerpt.chars().count() <= 512
                    && (!issue.body_truncated || issue.body_excerpt.chars().count() == 512)
                    && issue
                        .body_excerpt
                        .chars()
                        .all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
            })
        {
            return Err("Invalid or over-bound GitHub backlog observation".into());
        }
        Ok(())
    }

    pub fn matched_issue(&self, evidence: &ProviderMatch) -> Option<&Issue> {
        let issue = self
            .issues
            .iter()
            .find(|issue| issue.id == evidence.issue_id)?;
        let quote = &evidence.quote;
        (!issue.body_truncated
            && valid_text(quote, 2048, false)
            && quote != issue.title.trim()
            && !quote.contains("<!--")
            && !quote.contains("-->")
            && issue.body_excerpt.lines().any(|line| line.trim() == quote))
        .then_some(issue)
    }

    pub(super) fn permits_public_fields(&self, fields: &crate::backlog::PublicFields) -> bool {
        self.issues.iter().all(|issue| {
            // Missing issue text precludes a complete duplicate check. Do not forward
            // observed descriptions verbatim; the independent verifier checks privacy in context.
            !issue.body_truncated
                && !issue.body_excerpt.lines().map(str::trim).any(|line| {
                    !line.is_empty()
                        && (fields.title.trim() == line
                            || fields.body.lines().any(|public| public.trim() == line)
                            || (line.len() >= 24
                                && (fields.title.contains(line) || fields.body.contains(line))))
                })
        })
    }
}
