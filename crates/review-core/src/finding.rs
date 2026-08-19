//! `FindingReport@1` — one immutable claim by one attempt about one snapshot.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Minor,
    Major,
    Blocker,
}

impl Severity {
    /// Rank, ordered so a re-report may only raise it. The legacy harness ranked
    /// blocker/major/other as 3/2/1 and treated anything unknown as the floor; this enum removes
    /// the "unknown ranks as minor" hole that let an out-of-enum severity slip under a gate.
    pub fn rank(self) -> u8 {
        match self {
            Severity::Minor => 1,
            Severity::Major => 2,
            Severity::Blocker => 3,
        }
    }
}

/// Where a claim applies. An empty location list means change-wide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
}

impl Location {
    pub fn file(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line: None,
            end_line: None,
        }
    }

    pub fn at(path: impl Into<String>, line: u32) -> Self {
        Self {
            path: path.into(),
            line: Some(line),
            end_line: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Corroborates,
    Disputes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimTargetKind {
    Finding,
    Report,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationTarget {
    pub kind: ClaimTargetKind,
    pub id: String,
}

/// An explicit relation to a Finding in the attempt's input FindingSet, or to a Report from the
/// same selected attempt. Only explicit relations — or an exact occurrence-key match — may
/// attach a report; titles and fuzzy fingerprints never prove claim identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub kind: RelationKind,
    pub target: RelationTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The payload of a `review.kernel/FindingReport@1` artifact.
///
/// There is deliberately no status, no resolution and no round on a report: those belong to the
/// Finding projection, which is rebuildable. A report only ever states what one reviewer saw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingReport {
    pub title: String,
    pub severity: Severity,
    pub locations: Vec<Location>,
    pub body: String,
    /// The proposed remedy. Required: the legacy schema required it too, and the legacy ledger
    /// then stored it nowhere.
    pub fix: String,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_trace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
}

impl FindingReport {
    /// True when the claim is about the change as a whole rather than any path.
    pub fn is_change_wide(&self) -> bool {
        self.locations.is_empty()
    }
}
