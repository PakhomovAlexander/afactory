//! Canonical native Task Review reduction and path-independent Finding/Demand identities.

mod reduction;
pub use reduction::{
    PreparedReviewReduction, prepare_canonical_task_review,
    prepare_canonical_task_review_with_shards,
};

use af_contracts::reviewer_result::{BenchmarkDemand, CHANGE_WIDE_SENTINEL};
use af_contracts::{
    CANONICAL_FINDING_IDENTITY_POLICY, EventType, FindingDispositionPosition, FindingDispositionV1,
    FindingReport, Producer, ReviewerStageOutput, RunEvent,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::cas::Cas;
use crate::ledger::{EVENT_FINDING_REPORTED, EVENT_FINDING_RESOLVED, Ledger, ReportScope, Status};
use crate::store::{NewEvent, StoreError};

/// One selected flat reviewer result plus the exact authority needed by the typed-report bridge.
pub struct CanonicalStage<'a> {
    pub source: &'a str,
    pub demand_requirement: af_contracts::DemandRequirement,
    pub stage: &'a ReviewerStageOutput,
    pub attempt_id: &'a str,
    pub result_artifact_id: &'a str,
    pub input_artifacts: &'a [String],
    pub subject_snapshot_id: &'a str,
    pub subject_id: &'a str,
}

/// The exact immutable inputs emitted by one canonical ledger reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalReduction {
    /// Domain-separated typed Report IDs recorded in `FindingSet@1`.
    pub selected_report_ids: Vec<String>,
    /// Same-result Report index authority for post-reduction Proposal finalization.
    pub report_ids_by_source: std::collections::BTreeMap<String, Vec<String>>,
    /// Domain-separated relation IDs recorded in `FindingSet@1`.
    pub relation_ids: Vec<String>,
    /// Domain-separated typed Demand artifacts selected by this reduction.
    pub selected_demand_artifact_ids: Vec<String>,
    /// CAS IDs of all Report and relation envelopes consumed by the Set reducer.
    pub input_artifact_ids: Vec<String>,
    /// CAS records for selected Demand envelopes.
    pub demand_input_artifact_ids: Vec<String>,
}

#[derive(Clone)]
struct ReportProvenance {
    producer: Producer,
    input_artifacts: Vec<String>,
    subject_snapshot_id: String,
    subject_id: String,
}

struct PreparedStage {
    source: String,
    demand_requirement: af_contracts::DemandRequirement,
    reports: Vec<FindingReport>,
    demands: Vec<BenchmarkDemand>,
    dispositions: Vec<af_contracts::reviewer_result::ReviewerDisposition>,
    provenance: ReportProvenance,
}

/// A new canonical Finding is derived from the first selected Report that establishes it.
pub fn canonical_finding_id(report_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"review.kernel/finding-id/v1\0");
    hasher.update(report_id.as_bytes());
    format!("sha256:{}", af_contracts::hex::encode(&hasher.finalize()))
}

/// A reviewer re-stating the exact same obligation keeps one Demand identity across Rounds.
pub fn canonical_demand_id(source: &str, demand: &BenchmarkDemand) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"review.kernel/demand-id/v1\0");
    for field in [
        source,
        demand.claim.as_str(),
        demand.why.as_str(),
        demand.suggested_method.as_str(),
    ] {
        hasher.update(field.as_bytes());
        hasher.update([0]);
    }
    format!("sha256:{}", af_contracts::hex::encode(&hasher.finalize()))
}

fn canonical_stage_keys(
    reports: &[(&FindingReport, String, String)],
    prior: &Ledger,
    pending_occurrences: &std::collections::BTreeMap<(String, String), String>,
) -> Result<Vec<String>, StoreError> {
    let mut parent: Vec<usize> = (0..reports.len()).collect();
    let find = |parent: &mut Vec<usize>, mut index: usize| {
        while parent[index] != index {
            let grandparent = parent[parent[index]];
            parent[index] = grandparent;
            index = grandparent;
        }
        index
    };
    let union = |parent: &mut Vec<usize>, left: usize, right: usize| {
        let find_root = |mut index: usize| {
            while parent[index] != index {
                index = parent[index];
            }
            index
        };
        let left = find_root(left);
        let right = find_root(right);
        if left != right {
            let (keep, merge) = if left < right {
                (left, right)
            } else {
                (right, left)
            };
            parent[merge] = keep;
        }
    };

    let mut by_report = std::collections::BTreeMap::new();
    for (index, (_, _, artifact_id)) in reports.iter().enumerate() {
        if let Some(previous) = by_report.insert(artifact_id.as_str(), index) {
            union(&mut parent, previous, index);
        }
    }

    let mut occurrence_first = std::collections::BTreeMap::new();
    for (index, (report, _, _)) in reports.iter().enumerate() {
        if let (Some(rule_id), Some(occurrence_key)) = (&report.rule_id, &report.occurrence_key) {
            if let Some(previous) = occurrence_first.insert((rule_id, occurrence_key), index) {
                union(&mut parent, previous, index);
            }
        }
    }

    let mut first_report = std::collections::BTreeMap::new();
    for index in 0..reports.len() {
        let root = find(&mut parent, index);
        first_report.entry(root).or_insert(index);
    }
    let mut candidates: std::collections::BTreeMap<usize, BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for (index, (report, _, _)) in reports.iter().enumerate() {
        let root = find(&mut parent, index);
        if let (Some(rule_id), Some(occurrence_key)) = (&report.rule_id, &report.occurrence_key) {
            let occurrence = (rule_id.clone(), occurrence_key.clone());
            if let Some(finding) = pending_occurrences
                .get(&occurrence)
                .map(String::as_str)
                .or_else(|| prior.finding_for_occurrence(rule_id, occurrence_key))
            {
                candidates
                    .entry(root)
                    .or_default()
                    .insert(finding.to_string());
            }
        }
        // A relation can only corroborate a Finding in the input Finding Set.
        for relation in &report.relations {
            if prior.get(&relation.target.id).is_none() {
                return Err(StoreError::Conflict(format!(
                    "Report {} relates to Finding `{}` outside its input Finding Set",
                    reports[index].2, relation.target.id
                )));
            }
            candidates
                .entry(root)
                .or_default()
                .insert(relation.target.id.clone());
        }
    }

    let mut keys = Vec::with_capacity(reports.len());
    for index in 0..reports.len() {
        let root = find(&mut parent, index);
        let first = &reports[first_report[&root]].2;
        let group = candidates.get(&root);
        if group.is_some_and(|group| group.len() > 1) {
            return Err(StoreError::Conflict(format!(
                "corroboration and occurrence authority disagree for Report {first}"
            )));
        }
        keys.push(
            group
                .and_then(BTreeSet::first)
                .cloned()
                .unwrap_or_else(|| canonical_finding_id(first)),
        );
    }
    Ok(keys)
}

/// Apply a not-yet-persisted event through the authoritative projection. Sequence and identity
/// are irrelevant to Ledger, so placeholders let ingest validate the exact payload and artifact
/// set before the atomic append makes any part of the batch durable.
fn apply_candidate(ledger: &mut Ledger, event: &NewEvent, cas: &Cas) -> Result<(), StoreError> {
    ledger.apply_event(
        &RunEvent {
            event_id: String::new(),
            run_id: String::new(),
            sequence: 0,
            event_type: event.event_type,
            occurred_at: event.occurred_at.clone(),
            node_id: event.node_id.clone(),
            attempt_id: event.attempt_id.clone(),
            causation_id: event.causation_id.clone(),
            correlation_id: event.correlation_id.clone(),
            artifact_refs: event.artifact_refs.clone(),
            payload: event.payload.clone(),
        },
        cas,
    )
}
