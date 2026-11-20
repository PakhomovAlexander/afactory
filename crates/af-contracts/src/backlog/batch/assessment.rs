//! Private claim assessment bound to a frozen Project batch and the assessed source.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{BatchReference, is_member_id};
use crate::backlog::{Disposition, MAX_TEXT_BYTES, MAX_TITLE_BYTES, PublicFields, valid_text};
use crate::task::document::{DocumentDraft, DocumentSourceV1, RepositoryCitationV1};
use crate::task::usage::DecimalU64;

pub const SOURCE_NAME: &str = "backlog-batch";
pub const SOURCE_URI: &str = "af:backlog:";
pub const MAX_MEMBERS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollectionSchema {
    #[serde(rename = "af/backlog-collected-batch@1")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectedBatch {
    pub schema: CollectionSchema,
    pub reference: BatchReference,
    pub authority_id: String,
    pub scenario_id: String,
    pub source_revision: String,
    pub previous_high_water: DecimalU64,
    pub members: Vec<CollectedMember>,
    pub tracked_items: BTreeMap<String, String>,
    pub tracked_items_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectedMember {
    pub member_id: String,
    pub report_id: String,
    pub item_id: Option<String>,
    pub evidence_report_id: Option<String>,
    pub evidence_ids: Vec<String>,
    pub sequence: DecimalU64,
    pub report_digest: String,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Schema {
    #[serde(rename = "af/backlog-assessment@1")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    schema: Schema,
    batch_id: String,
    source_revision: String,
    decisions: Vec<Decision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    members: Vec<String>,
    evidence_ids: Vec<String>,
    citations: Vec<RepositoryCitationV1>,
    disposition: Disposition,
    claim: String,
    reason: String,
    #[serde(deserialize_with = "Option::deserialize")]
    tracked_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    proposed: Option<PublicFields>,
}

impl Assessment {
    pub fn check(
        source: &DocumentSourceV1,
        draft: &DocumentDraft,
        revision: Option<&str>,
    ) -> Result<(), String> {
        let batch: CollectedBatch = serde_json::from_str(&source.text)
            .map_err(|_| "Invalid frozen backlog batch context")?;
        batch.reference.validate()?;
        let body = draft
            .sections
            .iter()
            .find(|section| section.heading == "Decisions")
            .and_then(|section| section.body.strip_prefix("```json\n"))
            .and_then(|body| body.strip_suffix("\n```"))
            .ok_or("Backlog assessment requires a Decisions JSON block")?;
        let assessment: Self =
            serde_json::from_str(body).map_err(|_| "Invalid backlog assessment document")?;
        if source.uri != format!("{SOURCE_URI}{}", batch.reference.batch_id)
            || assessment.batch_id != batch.reference.batch_id
            || assessment.source_revision != batch.source_revision
            || revision != Some(batch.source_revision.as_str())
            || !draft.named_citations().any(|name| name == SOURCE_NAME)
        {
            return Err("Backlog assessment differs from its captured batch or source".into());
        }
        let citations: BTreeSet<_> = draft.repository_citations().collect();
        let mut remaining: BTreeMap<_, _> = batch
            .members
            .iter()
            .map(|row| (row.member_id.as_str(), &row.evidence_ids))
            .collect();
        if batch.members.len() != remaining.len() || !(1..=MAX_MEMBERS).contains(&remaining.len()) {
            return Err("Invalid frozen backlog membership".into());
        }
        for decision in &assessment.decisions {
            let cited: BTreeSet<_> = decision.evidence_ids.iter().collect();
            let requires_evidence = matches!(
                decision.disposition,
                Disposition::Verified | Disposition::AlreadyTracked
            );
            let mut eligible = BTreeSet::new();
            for member in &decision.members {
                let evidence = remaining
                    .remove(member.as_str())
                    .ok_or("A backlog decision names an unknown or repeated member")?;
                if requires_evidence && !evidence.iter().any(|id| cited.contains(id)) {
                    return Err(
                        "A verified or tracked decision requires evidence for each member".into(),
                    );
                }
                eligible.extend(evidence);
            }
            if decision.members.is_empty()
                || decision.members.iter().any(|id| !is_member_id(id))
                || cited.len() != decision.evidence_ids.len()
                || !cited.is_subset(&eligible)
                || decision.citations.iter().any(|c| !citations.contains(c))
                || !valid_text(&decision.claim, MAX_TEXT_BYTES, true)
                || !valid_text(&decision.reason, MAX_TEXT_BYTES, true)
                || !decision.valid_disposition(&batch.tracked_items)
            {
                return Err(
                    "Backlog decision lacks bound evidence, citations or valid fields".into(),
                );
            }
        }
        if !remaining.is_empty() {
            return Err("Backlog assessment did not account for every frozen member".into());
        }
        Ok(())
    }
}

impl Decision {
    fn valid_disposition(&self, tracked_items: &BTreeMap<String, String>) -> bool {
        match self.disposition {
            Disposition::Verified => {
                self.tracked_id.is_none()
                    && !self.evidence_ids.is_empty()
                    && !self.citations.is_empty()
                    && self.proposed.as_ref().is_some_and(|public| {
                        valid_text(&public.title, MAX_TITLE_BYTES, false)
                            && valid_text(&public.body, MAX_TEXT_BYTES, true)
                    })
            }
            Disposition::AlreadyTracked => {
                self.proposed.is_none()
                    && self
                        .tracked_id
                        .as_ref()
                        .is_some_and(|id| tracked_items.contains_key(id))
                    && !self.evidence_ids.is_empty()
                    && !self.citations.is_empty()
            }
            Disposition::NeedsEvidence | Disposition::Dismissed | Disposition::Held => {
                self.proposed.is_none() && self.tracked_id.is_none()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::BatchReferenceSchema;
    use super::*;
    use crate::task::document::{DocumentSectionV1, DraftCitation};

    #[test]
    fn assessment_refuses_unbound_members_and_unapproved_fields() {
        let digest = |digit: &str| format!("sha256:{}", digit.repeat(64));
        let source_revision = "a".repeat(40);
        let mut batch = CollectedBatch {
            schema: CollectionSchema::V1,
            reference: BatchReference {
                schema: BatchReferenceSchema::V1,
                project_id: digest("1"),
                batch_id: digest("2"),
                policy_id: digest("3"),
                high_water: 1.into(),
            },
            authority_id: digest("4"),
            scenario_id: digest("5"),
            source_revision: source_revision.clone(),
            previous_high_water: 0.into(),
            members: vec![CollectedMember {
                member_id: "producer".into(),
                report_id: "producer".into(),
                item_id: None,
                evidence_report_id: None,
                evidence_ids: vec![digest("6")],
                sequence: 1.into(),
                report_digest: digest("7"),
                source_revision: Some(source_revision.clone()),
            }],
            tracked_items: BTreeMap::new(),
            tracked_items_truncated: false,
        };
        let citation = RepositoryCitationV1 {
            path: "rules.json".into(),
            line: Some(1),
        };
        let mut assessment = Assessment {
            schema: Schema::V1,
            batch_id: batch.reference.batch_id.clone(),
            source_revision: source_revision.clone(),
            decisions: vec![Decision {
                members: vec!["producer".into()],
                evidence_ids: vec![digest("6")],
                citations: vec![citation.clone()],
                disposition: Disposition::Verified,
                claim: "One off-by-one error.".into(),
                reason: "The configured boundary is exclusive.".into(),
                tracked_id: None,
                proposed: Some(PublicFields {
                    title: "Off-by-one error".into(),
                    body: "Use an inclusive boundary.".into(),
                }),
            }],
        };
        let check = |batch: &CollectedBatch, assessment: &Assessment, commit: &str| {
            let source = DocumentSourceV1 {
                title: "Frozen batch".into(),
                uri: format!("{SOURCE_URI}{}", batch.reference.batch_id),
                revision: digest("9"),
                text: serde_json::to_string(batch).unwrap(),
            };
            let draft = DocumentDraft {
                schema: "af.document-draft/3".into(),
                title: "Assessment".into(),
                sections: vec![DocumentSectionV1 {
                    heading: "Decisions".into(),
                    body: format!(
                        "```json\n{}\n```",
                        serde_json::to_string(assessment).unwrap()
                    ),
                }],
                citations: [
                    DraftCitation::from(SOURCE_NAME),
                    DraftCitation::Repository {
                        location: citation.clone(),
                    },
                ]
                .into(),
            };
            Assessment::check(&source, &draft, Some(commit))
        };
        assert!(check(&batch, &assessment, &source_revision).is_ok());
        assert!(check(&batch, &assessment, &"b".repeat(40)).is_err());
        assessment.decisions[0].evidence_ids = vec![digest("8")];
        assert!(check(&batch, &assessment, &source_revision).is_err());
        assessment.decisions[0].evidence_ids = vec![digest("6")];
        assessment.decisions[0].members.push("producer".into());
        assert!(check(&batch, &assessment, &source_revision).is_err());
        assessment.decisions[0].members.pop();
        assessment.decisions[0].disposition = Disposition::Held;
        assert!(check(&batch, &assessment, &source_revision).is_err());
        assessment.decisions[0].proposed = None;
        assert!(check(&batch, &assessment, &source_revision).is_ok());
        assessment.decisions[0].disposition = Disposition::AlreadyTracked;
        assessment.decisions[0].tracked_id = Some(digest("a"));
        assert!(check(&batch, &assessment, &source_revision).is_err());
        batch.tracked_items.insert(digest("a"), digest("b"));
        assert!(check(&batch, &assessment, &source_revision).is_ok());
    }
}
