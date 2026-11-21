//! Private claim assessment bound to a frozen Project batch and the assessed source.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::provider_index::{Observation, ProviderMatch};
use super::{BatchReference, is_member_id};
use crate::backlog::{
    Disposition, ItemRevision, MAX_TEXT_BYTES, MAX_TITLE_BYTES, Privacy, PublicFields, valid_text,
};
use crate::is_digest;
use crate::task::document::{DocumentDraft, DocumentSourceV1, RepositoryCitationV1};
use crate::task::usage::DecimalU64;

pub const SOURCE_NAME: &str = "backlog-batch";
pub const SOURCE_URI: &str = "af:backlog:";
pub const MAX_MEMBERS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollectionSchema {
    #[serde(rename = "af/backlog-collected-batch@3")]
    V3,
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
    pub tracked_items: BTreeMap<String, TrackedItem>,
    pub tracked_items_truncated: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub provider_index: Option<ProviderIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIndex {
    pub observation_id: String,
    pub destination_id: String,
    pub observation: Observation,
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
    #[serde(deserialize_with = "Option::deserialize")]
    pub report: Option<ReportContext>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub additional_report: Option<ReportContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportContext {
    pub report_id: String,
    pub report_digest: String,
    pub sequence: DecimalU64,
    pub source_revision: Option<String>,
    pub title: String,
    pub observed: String,
    pub repro: Option<String>,
    pub privacy: Privacy,
    pub evidence: Vec<EvidenceExcerpt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceExcerpt {
    pub content_id: String,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackedItem {
    pub revision_id: String,
    pub revision: ItemRevision,
    pub verification_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Schema {
    #[serde(rename = "af/backlog-assessment@2")]
    V2,
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
pub struct Decision {
    pub members: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub citations: Vec<RepositoryCitationV1>,
    pub disposition: Disposition,
    pub claim: String,
    pub reason: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub tracked_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub proposed: Option<PublicFields>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub provider_match: Option<ProviderMatch>,
}

impl Assessment {
    pub fn check(
        source: &DocumentSourceV1,
        draft: &DocumentDraft,
        revision: Option<&str>,
    ) -> Result<(), String> {
        Self::checked(source, draft, revision).map(|_| ())
    }

    pub fn checked(
        source: &DocumentSourceV1,
        draft: &DocumentDraft,
        revision: Option<&str>,
    ) -> Result<(CollectedBatch, Vec<Decision>), String> {
        let batch: CollectedBatch = serde_json::from_str(&source.text)
            .map_err(|_| "Invalid frozen backlog batch context")?;
        batch.reference.validate()?;
        if let Some(index) = &batch.provider_index {
            if !is_digest(&index.observation_id) || !is_digest(&index.destination_id) {
                return Err("Invalid Project provider observation identity".into());
            }
            index.observation.validate()?;
        }
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
            .map(|row| (row.member_id.as_str(), row))
            .collect();
        if batch.members.len() != remaining.len()
            || remaining.len() > MAX_MEMBERS
            || batch.members.iter().any(|row| {
                row.sequence.get() == 0
                    || row.sequence.get() > batch.reference.high_water.get()
                    || !row.context_is_bound(batch.reference.high_water.get())
            })
        {
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
                let row = remaining
                    .remove(member.as_str())
                    .ok_or("A backlog decision names an unknown or repeated member")?;
                if requires_evidence
                    && (row.report.is_none()
                        || !row.evidence_ids.iter().any(|id| cited.contains(id)))
                {
                    return Err(
                        "A verified or tracked decision requires evidence for each member".into(),
                    );
                }
                eligible.extend(&row.evidence_ids);
            }
            if decision.members.is_empty()
                || decision.members.iter().any(|id| !is_member_id(id))
                || cited.len() != decision.evidence_ids.len()
                || !cited.is_subset(&eligible)
                || decision.citations.iter().any(|c| !citations.contains(c))
                || !valid_text(&decision.claim, MAX_TEXT_BYTES, true)
                || !valid_text(
                    &decision.reason,
                    if batch.provider_index.is_some() {
                        MAX_TEXT_BYTES - 256
                    } else {
                        MAX_TEXT_BYTES
                    },
                    true,
                )
                || !decision.valid_disposition(&batch)
            {
                return Err(
                    "Backlog decision lacks bound evidence, citations or valid fields".into(),
                );
            }
        }
        if !remaining.is_empty() {
            return Err("Backlog assessment did not account for every frozen member".into());
        }
        Ok((batch, assessment.decisions))
    }
}

impl CollectedMember {
    fn context_is_bound(&self, cutoff: u64) -> bool {
        self.report.as_ref().is_none_or(|report| {
            report.report_id == self.report_id
                && report.report_digest == self.report_digest
                && report.sequence == self.sequence
                && report.source_revision == self.source_revision
                && self.evidence_report_id.is_some() == self.additional_report.is_some()
        }) && self.additional_report.as_ref().is_none_or(|report| {
            self.report.is_some()
                && self.evidence_report_id.as_ref() == Some(&report.report_id)
                && report.sequence.get() > self.sequence.get()
                && report.sequence.get() <= cutoff
        })
    }
}

impl Decision {
    fn valid_disposition(&self, batch: &CollectedBatch) -> bool {
        if let Some(evidence) = &self.provider_match {
            return self.disposition == Disposition::AlreadyTracked
                && self.tracked_id.is_none()
                && self.proposed.is_none()
                && !self.evidence_ids.is_empty()
                && !self.citations.is_empty()
                && batch
                    .provider_index
                    .as_ref()
                    .is_some_and(|index| index.observation.matched_issue(evidence).is_some());
        }
        match self.disposition {
            Disposition::Verified => {
                self.tracked_id.is_none()
                    && !self.evidence_ids.is_empty()
                    && !self.citations.is_empty()
                    && self.proposed.as_ref().is_some_and(|public| {
                        valid_text(&public.title, MAX_TITLE_BYTES, false)
                            && valid_text(&public.body, MAX_TEXT_BYTES, true)
                            && batch
                                .provider_index
                                .as_ref()
                                .is_none_or(|index| index.observation.permits_public_fields(public))
                    })
            }
            Disposition::AlreadyTracked => {
                self.proposed.is_none()
                    && self
                        .tracked_id
                        .as_ref()
                        .is_some_and(|id| batch.tracked_items.contains_key(id))
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
            schema: CollectionSchema::V3,
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
                report: Some(ReportContext {
                    report_id: "producer".into(),
                    report_digest: digest("7"),
                    sequence: 1.into(),
                    source_revision: Some(source_revision.clone()),
                    title: "Boundary".into(),
                    observed: "The boundary input fails.".into(),
                    repro: None,
                    privacy: Privacy::Private,
                    evidence: Vec::new(),
                }),
                additional_report: None,
            }],
            tracked_items: BTreeMap::new(),
            tracked_items_truncated: false,
            provider_index: None,
        };
        let citation = RepositoryCitationV1 {
            path: "rules.json".into(),
            line: Some(1),
        };
        let mut assessment = Assessment {
            schema: Schema::V2,
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
                provider_match: None,
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
        let mut context_withheld = batch.clone();
        context_withheld.members[0].report = None;
        assert!(check(&context_withheld, &assessment, &source_revision).is_err());
        let mut wrong_receipt = batch.clone();
        wrong_receipt.members[0].report.as_mut().unwrap().sequence = 2.into();
        assert!(check(&wrong_receipt, &assessment, &source_revision).is_err());
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
        batch.tracked_items.insert(
            digest("a"),
            TrackedItem {
                revision_id: digest("b"),
                revision: ItemRevision {
                    project_id: digest("1"),
                    item_id: digest("a"),
                    report_ids: vec!["earlier".into()],
                    claim: "The boundary input fails.".into(),
                    evidence_ids: vec![digest("6")],
                    source_revision: source_revision.clone(),
                    proposed: PublicFields {
                        title: "Boundary".into(),
                        body: "Use an inclusive boundary.".into(),
                    },
                },
                verification_id: digest("c"),
            },
        );
        assert!(check(&batch, &assessment, &source_revision).is_ok());

        let title = "Boundary comparison rejects an input";
        let quote = "rules.json accepts 4 and incorrectly rejects 3";
        let marker = "<!-- af-backlog-item:project:item -->";
        let observation: Observation = serde_json::from_value(serde_json::json!({
            "schema": "af.backlog-open-issues/1", "observation": "live-page-walk",
            "repository": "fixture/backlog", "repository_id": 23, "destination": "github:fixture/backlog",
            "issues": [{
                "id": 1073, "number": 73, "title": title, "updated_at": "2026-11-19T12:30:00Z",
                "body_excerpt": format!("{quote}\n{title}\n{marker}"), "body_truncated": false,
            }],
        })).unwrap();
        batch.provider_index = Some(ProviderIndex {
            observation_id: digest("8"),
            destination_id: digest("9"),
            observation,
        });
        assert!(check(&batch, &assessment, &source_revision).is_ok()); // local lookup
        assessment.decisions[0].tracked_id = None;
        for (issue_id, excerpt) in [
            (73, quote),
            (1073, title),
            (1073, marker),
            (1073, "missing"),
        ] {
            assessment.decisions[0].provider_match = Some(ProviderMatch {
                issue_id,
                quote: excerpt.into(),
            });
            assert!(check(&batch, &assessment, &source_revision).is_err());
        }
        assessment.decisions[0].provider_match = Some(ProviderMatch {
            issue_id: 1073,
            quote: quote.into(),
        });
        assert!(check(&batch, &assessment, &source_revision).is_ok());
        let issue = &mut batch.provider_index.as_mut().unwrap().observation.issues[0];
        issue.body_excerpt = format!("{quote}\n{}", "x".repeat(511 - quote.len()));
        issue.body_truncated = true;
        assert!(check(&batch, &assessment, &source_revision).is_err());
        assessment.decisions[0].disposition = Disposition::Verified;
        assessment.decisions[0].provider_match = None;
        assessment.decisions[0].proposed = Some(PublicFields {
            title: "Inclusive boundary".into(),
            body: "Restore the boundary behavior.".into(),
        });
        assert!(check(&batch, &assessment, &source_revision).is_err());
        batch.provider_index.as_mut().unwrap().observation.issues[0].body_truncated = false;
        assert!(check(&batch, &assessment, &source_revision).is_ok());
        assessment.decisions[0].proposed.as_mut().unwrap().body = quote.into();
        assert!(check(&batch, &assessment, &source_revision).is_err());
    }
}
