//! Goal proposals are untrusted planning data. Only the coordinator owns progress; this
//! coordinator admits complete loops and settles only native Run/Task evidence.
use super::contract::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_TOKENS: u64 = review_core::json::SAFE_INTEGER_MAX as u64;
const YEAR: u64 = 366 * 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Proposal {
    pub schema: String,
    pub id: String,
    pub objective: String,
    pub horizon: Horizon,
    pub max_tokens: u64,
    pub daily_tokens: u64,
    pub rolling_tokens: u64,
    pub rolling_window_ms: u64,
    pub goals: Vec<Goal>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Horizon {
    pub start_unix_ms: u64,
    pub end_unix_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Goal {
    pub id: String,
    pub objective: String,
    pub partition: Partition,
    /// Product scope and workload bound a complete loop, never a pipeline stage or date.
    pub component: String,
    pub scope: String,
    pub workload: String,
    pub metric: Metric,
    pub acceptance: Vec<String>,
    pub guardrails: Vec<String>,
    pub priority: u16,
    pub dependencies: Vec<String>,
    pub conflicts: Vec<String>,
    pub estimated_tokens: Option<u64>,
    /// Explicit whole-loop allowance, independent of the untrusted estimate.
    pub loop_tokens: u64,
    pub proposed_eligible: bool,
    pub deferral_reason: Option<String>,
    pub remaining_work: Vec<String>,
    /// Untrusted references are leads, never accepted execution evidence.
    pub sources: Vec<Reference>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Partition {
    ProductComponent,
    TargetMetric,
    OptimizationGoal,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Metric {
    pub name: String,
    pub direction: Direction,
    pub unit: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Direction {
    Decrease,
    Increase,
    Maintain,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    pub kind: ReferenceKind,
    pub digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReferenceKind {
    Source,
    Research,
    Candidate,
    Experiment,
    Verification,
    Report,
}
impl Goal {
    fn same_partition(&self, other: &Self) -> bool {
        self.partition == other.partition
            && self.component == other.component
            && self.scope == other.scope
            && self.workload == other.workload
            && match self.partition {
                Partition::ProductComponent => true,
                Partition::TargetMetric => self.metric == other.metric,
                Partition::OptimizationGoal => {
                    self.objective == other.objective && self.metric == other.metric
                }
            }
    }
}
impl Reference {
    fn validate(&self) -> Result<()> {
        require(
            review_core::is_digest(&self.digest),
            "Malformed roadmap reference",
        )
    }
}
fn texts(values: &[String]) -> Result<()> {
    require(
        !values.is_empty() && values.len() <= 128,
        "Missing or excessive goal criteria/work",
    )?;
    for value in values {
        text(value)?;
        require(!value.trim().is_empty(), "Blank goal criteria/work")?;
    }
    Ok(())
}
impl Proposal {
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-roadmap-proposal@2",
            "Unsupported roadmap proposal version",
        )?;
        name(&self.id)?;
        text(&self.objective)?;
        require(!self.objective.trim().is_empty(), "Blank roadmap objective")?;
        require(
            self.horizon.start_unix_ms > 0
                && self.horizon.start_unix_ms < self.horizon.end_unix_ms
                && self.horizon.end_unix_ms <= MAX_TIME
                && self.horizon.end_unix_ms - self.horizon.start_unix_ms <= YEAR,
            "Roadmap horizon must be positive and at most 366 days",
        )?;
        require(
            self.max_tokens > 0 && self.max_tokens <= MAX_TOKENS,
            "Invalid roadmap token bound",
        )?;
        require(
            !self.goals.is_empty() && self.goals.len() <= 256,
            "Roadmap requires 1..256 goal partitions",
        )?;
        let mut ids = BTreeSet::new();
        require(
            self.daily_tokens > 0
                && self.daily_tokens <= MAX_TOKENS
                && self.rolling_tokens > 0
                && self.rolling_tokens <= MAX_TOKENS
                && self.rolling_window_ms > 0
                && self.rolling_window_ms <= YEAR,
            "Invalid roadmap daily/rolling limits",
        )?;
        for goal in &self.goals {
            require(
                goal.loop_tokens > 0 && goal.loop_tokens <= MAX_TOKENS,
                "Invalid complete-loop allowance",
            )?;
            name(&goal.id)?;
            require(ids.insert(goal.id.as_str()), "Duplicate stable goal ID")?;
            for s in [
                &goal.objective,
                &goal.component,
                &goal.scope,
                &goal.workload,
                &goal.metric.name,
                &goal.metric.unit,
            ] {
                text(s)?;
                require(!s.trim().is_empty(), "Blank goal scope or metric")?;
            }
            texts(&goal.acceptance)?;
            texts(&goal.guardrails)?;
            texts(&goal.remaining_work)?;
            require(goal.priority > 0, "Goal priority must be positive")?;
            require(
                goal.estimated_tokens.is_none_or(|n| n <= self.max_tokens),
                "Goal estimate exceeds roadmap bound",
            )?;
            if let Some(reason) = &goal.deferral_reason {
                text(reason)?;
                require(!reason.trim().is_empty(), "Blank deferral reason")?;
            }
            require(
                goal.proposed_eligible || goal.deferral_reason.is_some(),
                "Ineligible goal requires a deferral/blocker reason",
            )?;
            require(goal.sources.len() <= 128, "Too many roadmap references")?;
            for reference in &goal.sources {
                reference.validate()?;
            }
        }
        for goal in &self.goals {
            for links in [&goal.dependencies, &goal.conflicts] {
                let mut seen = BTreeSet::new();
                for id in links {
                    require(
                        ids.contains(id.as_str()) && id != &goal.id,
                        "Unknown or self goal dependency/conflict",
                    )?;
                    require(seen.insert(id), "Duplicate goal dependency/conflict")?;
                }
            }
            require(
                !goal
                    .dependencies
                    .iter()
                    .any(|id| goal.conflicts.contains(id)),
                "Goal both depends on and conflicts with another goal",
            )?;
        }
        // Bounded Kahn traversal avoids recursive parsing of an untrusted graph.
        let mut visited = BTreeSet::new();
        loop {
            let before = visited.len();
            for goal in &self.goals {
                if goal.dependencies.iter().all(|id| visited.contains(id)) {
                    visited.insert(goal.id.clone());
                }
            }
            if visited.len() == self.goals.len() {
                break;
            }
            require(before != visited.len(), "Cyclic roadmap dependencies")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revision {
    pub schema: String,
    pub revision: String,
    pub parent: Option<String>,
    pub proposal: Proposal,
}
impl Revision {
    pub fn new(parent: Option<String>, proposal: Proposal) -> Result<Self> {
        proposal.validate()?;
        if let Some(id) = &parent {
            require(review_core::is_digest(id), "Malformed roadmap parent")?;
        }
        let revision = hash(&("af/scenario-roadmap-revision@2", &parent, &proposal))?;
        Ok(Self {
            schema: "af/scenario-roadmap-revision@2".into(),
            revision,
            parent,
            proposal,
        })
    }
    fn validate(&self) -> Result<()> {
        require(
            *self == Self::new(self.parent.clone(), self.proposal.clone())?,
            "Roadmap revision identity differs",
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GoalState {
    Pending,
    Active,
    Completed,
    Deferred,
    Blocked,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Progress {
    pub state: GoalState,
    pub claim: Option<Claim>,
    pub reason: Option<String>,
    pub captured_goal: Option<String>,
    pub reserved_tokens: u64,
    pub actual_tokens: u64,
    /// Same native reservation/settlement records as Scenario Runs. Keys identify exact
    /// Task Store/revision charges; failed work is charged without manufacturing acceptance.
    pub task_charges: BTreeMap<String, super::state::TaskCharge>,
    /// Native accepted Task receipts; no second acceptance engine. Empty in milestone one.
    pub task_receipts: Vec<TaskReceipt>,
    pub artifacts: Vec<Reference>,
    pub completed_work: Vec<String>,
}
impl Progress {
    fn pending() -> Self {
        Self {
            state: GoalState::Pending,
            claim: None,
            reason: None,
            captured_goal: None,
            reserved_tokens: 0,
            actual_tokens: 0,
            task_charges: BTreeMap::new(),
            task_receipts: Vec::new(),
            artifacts: Vec::new(),
            completed_work: Vec::new(),
        }
    }
    fn occupies_partition(&self) -> bool {
        self.state == GoalState::Completed
            || self.state == GoalState::Active
            || self
                .claim
                .as_ref()
                .is_some_and(|c| c.settled_unix_ms.is_none())
    }
    fn protected(&self) -> bool {
        self.claim.is_some()
            || self.captured_goal.is_some()
            || self.reserved_tokens > 0
            || self.actual_tokens > 0
            || !self.task_charges.is_empty()
            || !self.task_receipts.is_empty()
            || !self.artifacts.is_empty()
            || !self.completed_work.is_empty()
            || self.state == GoalState::Active
            || self.state == GoalState::Completed
    }
    fn validate(&self, goal: &Goal) -> Result<()> {
        require(
            self.reserved_tokens <= MAX_TOKENS && self.actual_tokens <= MAX_TOKENS,
            "Invalid roadmap accounting",
        )?;
        if let Some(reason) = &self.reason {
            text(reason)?;
        }
        if matches!(self.state, GoalState::Blocked | GoalState::Deferred) {
            require(self.reason.is_some(), "Missing progress reason")?;
        }
        if self.protected() {
            require(
                self.captured_goal.as_ref() == Some(&hash(goal)?),
                "Active/completed goal definition changed",
            )?;
        }
        let mut reserved = 0_u64;
        let mut actual = 0_u64;
        for (key, charge) in &self.task_charges {
            require(
                review_core::is_digest(key)
                    && charge.maximum <= MAX_TOKENS
                    && charge.charged.is_none_or(|n| n <= MAX_TOKENS),
                "Malformed roadmap Task charge",
            )?;
            text(&charge.step)?;
            actual = actual
                .checked_add(charge.charged.unwrap_or(0))
                .ok_or("Roadmap accounting overflow")?;
            reserved = reserved
                .checked_add(charge.reserved())
                .ok_or("Roadmap reservation overflow")?;
        }

        if let Some(claim) = &self.claim {
            claim.validate()?;
            require(
                claim.allowance == goal.loop_tokens && claim.goal_id == goal.id,
                "Claim allowance or goal identity differs",
            )?;
            require(
                actual == self.actual_tokens
                    && self.reserved_tokens
                        == if claim.settled_unix_ms.is_some() {
                            reserved
                        } else {
                            goal.loop_tokens.saturating_sub(actual).max(reserved)
                        },
                "Roadmap accounting differs from complete-loop reservation",
            )?;
        } else {
            require(
                actual == self.actual_tokens && reserved == self.reserved_tokens,
                "Roadmap accounting differs from native charges/reservations",
            )?;
        }
        let mut keys = BTreeSet::new();
        let mut charged = 0_u64;
        for receipt in &self.task_receipts {
            Datum::Task(receipt.clone()).validate()?;
            require(
                keys.insert((&receipt.task_id, &receipt.revision_id)),
                "Duplicate roadmap Task receipt",
            )?;
            charged = charged
                .checked_add(receipt.charged_tokens)
                .ok_or("Roadmap accounting overflow")?;
        }
        require(
            charged <= self.actual_tokens,
            "Accepted receipts exceed recorded native charges",
        )?;
        for reference in &self.artifacts {
            reference.validate()?;
        }
        for work in &self.completed_work {
            text(work)?;
        }
        if self.state == GoalState::Pending {
            require(
                *self == Self::pending(),
                "Pending goal carries fabricated progress",
            )?;
        }
        if self.state == GoalState::Completed {
            require(
                !self.task_receipts.is_empty()
                    && !self.completed_work.is_empty()
                    && self.reserved_tokens == 0,
                "Completed goal lacks accepted Task evidence",
            )?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Roadmap {
    pub schema: String,
    pub id: String,
    /// Opaque namespace for stable goal identities across dates, revisions and Stores.
    pub identity: String,
    pub revisions: Vec<Revision>,
    pub progress: BTreeMap<String, Progress>,
}
impl Roadmap {
    pub fn new(revision: Revision) -> Result<Self> {
        revision.validate()?;
        require(
            revision.parent.is_none(),
            "New roadmap cannot supply history",
        )?;
        let value = Self {
            schema: "af/scenario-roadmap@2".into(),
            id: revision.proposal.id.clone(),
            identity: execution_id()?,
            progress: revision
                .proposal
                .goals
                .iter()
                .map(|g| (g.id.clone(), Progress::pending()))
                .collect(),
            revisions: vec![revision],
        };
        value.validate()?;
        Ok(value)
    }
    pub fn head(&self) -> &Revision {
        self.revisions
            .last()
            .expect("validated roadmap has a revision")
    }
    pub fn plan(&self, proposal: Proposal) -> Result<Revision> {
        self.validate()?;
        let revision = Revision::new(Some(self.head().revision.clone()), proposal)?;
        self.check_edit(&revision.proposal)?;
        Ok(revision)
    }
    fn check_edit(&self, proposal: &Proposal) -> Result<()> {
        require(proposal.id == self.id, "Roadmap identity cannot change")?;
        for old in &self.head().proposal.goals {
            let new = proposal
                .goals
                .iter()
                .find(|g| g.id == old.id)
                .ok_or("Stable goal IDs cannot be removed; defer future work instead")?;
            if self.progress[&old.id].protected() {
                require(old == new, "Cannot revise an active or completed goal")?;
            }
        }
        let committed = self
            .progress
            .values()
            .try_fold(0_u64, |n, p| {
                n.checked_add(p.actual_tokens)?
                    .checked_add(p.reserved_tokens)
            })
            .ok_or("Roadmap accounting overflow")?;
        require(
            committed <= proposal.max_tokens,
            "Roadmap budget is below accumulated charges/reservations",
        )
    }
    pub fn revise(&mut self, proposal: Proposal, confirm: &str) -> Result<()> {
        let revision = self.plan(proposal)?;
        require(
            revision.revision == confirm,
            "Roadmap revision conflict; validate and confirm the exact revision again",
        )?;
        for goal in &revision.proposal.goals {
            self.progress
                .entry(goal.id.clone())
                .or_insert_with(Progress::pending);
        }
        self.revisions.push(revision);
        self.validate()
    }
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-roadmap@2" && review_core::is_digest(&self.identity),
            "Unsupported roadmap or invalid identity",
        )?;
        name(&self.id)?;
        require(!self.revisions.is_empty(), "Roadmap history is empty")?;
        let mut parent = None;
        let mut previous: Option<&Proposal> = None;
        for revision in &self.revisions {
            revision.validate()?;
            require(
                revision.parent == parent && revision.proposal.id == self.id,
                "Broken roadmap revision chain",
            )?;
            if let Some(old) = previous {
                for goal in &old.goals {
                    require(
                        revision.proposal.goals.iter().any(|g| g.id == goal.id),
                        "Roadmap history dropped a stable goal",
                    )?;
                }
            }
            parent = Some(revision.revision.clone());
            previous = Some(&revision.proposal);
        }
        let goals = &self.head().proposal.goals;
        require(
            self.progress.len() == goals.len(),
            "Roadmap progress keys differ",
        )?;
        let mut charges = BTreeSet::new();
        for goal in goals {
            for key in self
                .progress
                .get(&goal.id)
                .ok_or("Missing goal progress")?
                .task_charges
                .keys()
            {
                require(
                    charges.insert(key),
                    "Task charged to more than one roadmap goal",
                )?;
            }
            self.progress
                .get(&goal.id)
                .ok_or("Missing goal progress")?
                .validate(goal)?;
            if let Some(claim) = &self.progress[&goal.id].claim {
                let revision = self
                    .revisions
                    .iter()
                    .find(|r| r.revision == claim.revision)
                    .ok_or("Claim revision missing from roadmap history")?;
                require(
                    revision.proposal.goals.iter().any(|g| g == goal),
                    "Claim goal differs from its captured roadmap revision",
                )?;
            }
        }
        // Recorded overruns remain readable evidence. They cannot authorize a future
        // revision below committed accounting, but must never make spending disappear.
        self.progress
            .values()
            .try_fold(0_u64, |n, p| {
                n.checked_add(p.actual_tokens)?
                    .checked_add(p.reserved_tokens)
            })
            .ok_or("Roadmap accounting overflow")?;
        Ok(())
    }
    /// A proposal-only transaction cannot replace history, identity or runtime evidence.
    pub fn validate_revision_of(&self, old: &Self) -> Result<()> {
        require(
            self.identity == old.identity
                && self.id == old.id
                && self.revisions.starts_with(&old.revisions),
            "Roadmap history/identity is immutable",
        )?;
        for (id, progress) in &old.progress {
            require(
                self.revisions == old.revisions || self.progress.get(id) == Some(progress),
                "Revision cannot rewrite runtime accounting or evidence",
            )?;
        }
        for (id, progress) in &self.progress {
            if !old.progress.contains_key(id) {
                require(
                    *progress == Progress::pending(),
                    "New goal must start pending",
                )?;
            }
        }
        if self.revisions == old.revisions {
            return Ok(());
        }
        for revision in &self.revisions[old.revisions.len()..] {
            old.check_edit(&revision.proposal)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        store::Store,
        tests::{schema, valid},
    };
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Barrier};

    fn proposal() -> Proposal {
        serde_json::from_str(include_str!(
            "../../../../fixtures/scenarios/performance-roadmap.json"
        ))
        .unwrap()
    }
    fn roadmap() -> Roadmap {
        Roadmap::new(Revision::new(None, proposal()).unwrap()).unwrap()
    }
    fn receipt() -> TaskReceipt {
        let id = digest(b"test Task result");
        serde_json::from_value(json!({
            "task_id":"task", "plan_id":id, "revision_id":id,"result_id":id,"charged_tokens":17,
            "result":{"task_revision_id":id,"execution":"completed","acceptance":"satisfied",
            "domain_conclusion":"verified", "outputs":{"report":{"artifact_ids":[id],"artifact_type":"af/Report@1","cardinality":"one"}},"evidence":[id],"missing_obligations":[]}
        })).unwrap()
    }
    #[test]
    fn roadmap_schemas_roundtrip_and_refuse_old_shapes_and_forged_input() {
        let roadmap = roadmap();
        for (file, value) in [
            ("scenario-roadmap-proposal-v2.json", json!(proposal())),
            ("scenario-roadmap-revision-v2.json", json!(roadmap.head())),
            ("scenario-roadmap-v2.json", json!(roadmap)),
            (
                "scenario-roadmap-list-v2.json",
                json!({"schema":"af/scenario-roadmap-list@2","roadmaps":{"performance":roadmap}}),
            ),
        ] {
            valid(file, &value);
            let mut old = value;
            old["schema"] = json!("af/unknown@0");
            assert!(!schema(file).is_valid(&old));
        }
        let replay: Roadmap = serde_json::from_value(json!(roadmap)).unwrap();
        assert_eq!(replay, roadmap);
        replay.validate().unwrap();
        for (key, value) in [
            ("actual_tokens", json!(17)),
            ("state", json!("completed")),
            ("task_receipts", json!([receipt()])),
            (
                "task_charges",
                json!({digest(b"forged"): {"maximum":100,"charged":17,"step":"experiment"}}),
            ),
            (
                "artifacts",
                json!([{"kind":"experiment","digest":digest(b"claim")}]),
            ),
        ] {
            let mut forged = json!(proposal());
            forged["goals"][0][key] = value;
            assert!(
                serde_json::from_value::<Proposal>(forged.clone()).is_err(),
                "{key}"
            );
            assert!(
                !schema("scenario-roadmap-proposal-v2.json").is_valid(&forged),
                "{key}"
            );
        }
        let mut old = proposal();
        old.schema = "af/scenario-roadmap-proposal@0".into();
        assert!(old.validate().is_err());
    }
    #[test]
    fn roadmap_rejects_unknown_duplicate_cyclic_dependencies_and_invalid_bounds_or_criteria() {
        let p = proposal();
        let mut cases = Vec::new();
        let mut c = p.clone();
        c.goals.push(c.goals[0].clone());
        cases.push(c);
        let mut c = p.clone();
        c.goals[0].dependencies.push("unknown".into());
        cases.push(c);
        let mut c = p.clone();
        c.goals[0].conflicts.push("unknown".into());
        cases.push(c);
        let mut c = p.clone();
        let mut second = c.goals[0].clone();
        second.id = "metric".into();
        second.dependencies.push(c.goals[0].id.clone());
        c.goals[0].dependencies.push(second.id.clone());
        c.goals.push(second);
        cases.push(c);
        let mut c = p.clone();
        c.horizon.end_unix_ms = c.horizon.start_unix_ms;
        cases.push(c);
        let mut c = p.clone();
        c.horizon.end_unix_ms = c.horizon.start_unix_ms + YEAR + 1;
        cases.push(c);
        let mut c = p.clone();
        c.max_tokens = u64::MAX;
        cases.push(c);
        let mut c = p.clone();
        c.goals[0].estimated_tokens = Some(c.max_tokens + 1);
        cases.push(c);
        let mut c = p.clone();
        c.goals[0].guardrails.clear();
        cases.push(c);
        let mut c = p.clone();
        c.goals[0].acceptance = vec!["  ".into()];
        cases.push(c);
        let mut c = p.clone();
        c.goals[0].sources.push(Reference {
            kind: ReferenceKind::Verification,
            digest: "completed".into(),
        });
        cases.push(c);
        let mut c = p.clone();
        c.goals[0].proposed_eligible = false;
        cases.push(c);
        for c in cases {
            assert!(c.validate().is_err(), "{c:?}");
        }
        let mut valid = p.clone();
        let mut second = p.goals[0].clone();
        second.id = "metric".into();
        second.partition = Partition::TargetMetric;
        second.dependencies.push(p.goals[0].id.clone());
        valid.goals.push(second);
        valid.validate().unwrap();
        let mut stage = json!(p);
        stage["goals"][0]["partition"] = json!("implementation");
        assert!(serde_json::from_value::<Proposal>(stage).is_err());
    }
    #[test]
    fn roadmap_revisions_cannot_reset_active_completed_definitions_receipts_or_charges() {
        let mut active = roadmap();
        let id = active.head().proposal.goals[0].id.clone();
        let p = active.progress.get_mut(&id).unwrap();
        p.state = GoalState::Active;
        p.captured_goal = Some(hash(&proposal().goals[0]).unwrap());
        p.reserved_tokens = 20;
        p.task_charges.insert(
            digest(b"reserved task"),
            super::super::state::TaskCharge {
                maximum: 20,
                charged: None,
                unknown_usage: false,
                step: "research".into(),
            },
        );
        active.validate().unwrap();
        let mut changed = proposal();
        changed.goals[0].workload = "another workload".into();
        assert!(active.plan(changed.clone()).is_err());
        let mut completed = active.clone();
        let p = completed.progress.get_mut(&id).unwrap();
        p.state = GoalState::Completed;
        p.reserved_tokens = 0;
        p.actual_tokens = 25;
        p.task_charges.clear();
        p.task_charges.insert(
            digest(b"accepted task"),
            super::super::state::TaskCharge {
                maximum: 20,
                charged: Some(17),
                unknown_usage: false,
                step: "verification".into(),
            },
        );
        p.task_charges.insert(
            digest(b"failed task"),
            super::super::state::TaskCharge {
                maximum: 10,
                charged: Some(8),
                unknown_usage: false,
                step: "experiment".into(),
            },
        );
        p.task_receipts = vec![receipt()];
        p.completed_work = vec!["Accepted native verification Task".into()];
        p.artifacts = vec![Reference {
            kind: ReferenceKind::Report,
            digest: digest(b"report"),
        }];
        completed.validate().unwrap();
        valid("scenario-roadmap-v2.json", &completed);
        assert!(completed.plan(changed).is_err());
        // A later horizon is future planning; the same goal keeps its evidence and spend.
        let mut future = proposal();
        future.horizon.start_unix_ms += 86_400_000;
        future.horizon.end_unix_ms += 86_400_000;
        let revision = completed.plan(future.clone()).unwrap();
        let before = completed.clone();
        completed.revise(future, &revision.revision).unwrap();
        completed.validate_revision_of(&before).unwrap();
        assert_eq!(completed.progress, before.progress);
        for state in [GoalState::Pending, GoalState::Active] {
            let mut forged = completed.clone();
            forged.progress.get_mut(&id).unwrap().state = state;
            assert!(forged.validate_revision_of(&before).is_err());
        }
        let mut forged = completed.clone();
        forged.progress.get_mut(&id).unwrap().actual_tokens = 0;
        assert!(forged.validate().is_err());
        assert!(forged.validate_revision_of(&before).is_err());
        let mut low = completed.head().proposal.clone();
        low.max_tokens = 16;
        assert!(completed.plan(low).is_err());
        let mut no_evidence = before.clone();
        no_evidence
            .progress
            .get_mut(&id)
            .unwrap()
            .task_receipts
            .clear();
        assert!(no_evidence.validate().is_err());
        // A failed experiment's overrun is still recorded spending, not acceptance.
        let mut overrun = before.clone();
        let progress = overrun.progress.get_mut(&id).unwrap();
        progress
            .task_charges
            .get_mut(&digest(b"failed task"))
            .unwrap()
            .charged = Some(100_000);
        progress.actual_tokens = 100_017;
        overrun.validate().unwrap();
        assert!(overrun.plan(proposal()).is_err());
        let mut cleared = before.clone();
        cleared.revisions.clear();
        assert!(cleared.validate().is_err());
        let mut future = proposal();
        let mut other = future.goals[0].clone();
        other.id = "other-component".into();
        future.goals.push(other);
        let revision = completed.plan(future.clone()).unwrap();
        let previous = completed.clone();
        completed.revise(future, &revision.revision).unwrap();
        completed.validate_revision_of(&previous).unwrap();
        assert_eq!(completed.progress["other-component"], Progress::pending());
        let mut dropped = completed.head().proposal.clone();
        dropped.goals.remove(0);
        assert!(completed.plan(dropped).is_err());
        // The same exact Task charge cannot be counted under two goal partitions.
        let mut duplicate = completed.clone();
        let mut progress = duplicate.progress[&id].clone();
        progress.captured_goal = Some(hash(&duplicate.head().proposal.goals[1]).unwrap());
        duplicate
            .progress
            .insert("other-component".into(), progress);
        assert!(duplicate.validate().is_err());
        let mut renamed = before.clone();
        renamed.identity = execution_id().unwrap();
        assert!(renamed.validate_revision_of(&before).is_err());
    }
    #[test]
    fn roadmap_restart_immutable_history_and_concurrent_revision_conflict() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let store = Store::isolated(&path).unwrap();
        let initial = roadmap();
        store
            .update(|w| {
                w.roadmaps.insert(initial.id.clone(), initial.clone());
                Ok(())
            })
            .unwrap();
        let mut future = proposal();
        future.goals[0].estimated_tokens = Some(200);
        let revision = initial.plan(future.clone()).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let barrier = barrier.clone();
                    let store = store.clone();
                    let future = future.clone();
                    let revision = revision.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        store.update(|w| {
                            w.roadmaps
                                .get_mut("performance")
                                .unwrap()
                                .revise(future, &revision.revision)
                        })
                    })
                })
                .collect();
            barrier.wait();
            assert_eq!(
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .filter(Result::is_ok)
                    .count(),
                1
            );
        });
        drop(store);
        let store = Store::isolated(&path).unwrap();
        let replay = store.read().unwrap();
        assert_eq!(replay.roadmaps["performance"].identity, initial.identity);
        assert_eq!(replay.roadmaps["performance"].revisions.len(), 2);
        assert_eq!(
            replay.roadmaps["performance"].revisions[0],
            initial.revisions[0]
        );
        assert_eq!(replay.roadmaps["performance"].progress, initial.progress);
        valid("scenario-store-v6.json", &replay);
        assert!(
            store
                .update(|w| {
                    w.roadmaps.clear();
                    Ok(())
                })
                .is_err()
        );
        assert!(
            store
                .update(|w| {
                    w.roadmaps
                        .get_mut("performance")
                        .unwrap()
                        .revisions
                        .remove(0);
                    Ok(())
                })
                .is_err()
        );
        assert!(
            store
                .update(|w| {
                    w.roadmaps
                        .get_mut("performance")
                        .unwrap()
                        .progress
                        .values_mut()
                        .next()
                        .unwrap()
                        .actual_tokens = 1;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(store.read().unwrap(), replay);
        // All prior-release Store documents fail closed, rather than defaulting missing history.
        let mut old = json!(replay);
        old["schema"] = json!("af/scenario-store@1");
        assert!(!schema("scenario-store-v6.json").is_valid(&old));
        let parsed: super::super::store::World = serde_json::from_value(old.clone()).unwrap();
        assert!(parsed.validate().is_err());
        old.as_object_mut().unwrap().remove("roadmaps");
        assert!(serde_json::from_value::<super::super::store::World>(old).is_err());
        std::fs::write(path.join("runs/scenario-owner"), b"af/scenario-store@1\n").unwrap();
        assert!(Store::isolated(&path).is_err());
    }
}

/// The pin names one exact roadmap revision. Editing future planning requires a new pin, but
/// never a new accounting namespace. Manual Runs and Ant occurrences call the same admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Admission {
    pub roadmap: String,
    pub identity: String,
    pub revision: String,
}
impl Admission {
    pub fn validate(&self) -> Result<()> {
        name(&self.roadmap)?;
        require(
            review_core::is_digest(&self.identity) && review_core::is_digest(&self.revision),
            "Invalid roadmap admission pin",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Claim {
    pub run_id: String,
    pub execution_id: String,
    pub revision: String,
    pub goal_id: String,
    pub allowance: u64,
    pub started_unix_ms: u64,
    pub settled_unix_ms: Option<u64>,
}
impl Claim {
    fn validate(&self) -> Result<()> {
        name(&self.run_id)?;
        name(&self.goal_id)?;
        require(
            review_core::is_digest(&self.execution_id)
                && review_core::is_digest(&self.revision)
                && self.allowance > 0
                && self.allowance <= MAX_TOKENS
                && self.started_unix_ms > 0
                && self.started_unix_ms <= MAX_TIME
                && self
                    .settled_unix_ms
                    .is_none_or(|t| t >= self.started_unix_ms && t <= MAX_TIME),
            "Invalid goal claim",
        )
    }
}
/// The validated bundle is acyclic and at most 16 levels deep. Memoization also bounds
/// admission work when several steps invoke the same child definition.
fn declared_loop(bundle: &Bundle, id: &str, memo: &mut BTreeMap<String, u64>) -> Result<u64> {
    if let Some(value) = memo.get(id) {
        return Ok(*value);
    }
    let mut total = 0u64;
    for step in &bundle.definitions[id].steps {
        let amount = match &step.operation {
            Kind::FreshTask { template, .. } => template
                .max_tokens
                .checked_mul(u64::from(step.max_attempts))
                .ok_or("Loop reservation overflow")?,
            Kind::Scenario { scenario } => declared_loop(bundle, scenario, memo)?,
            _ => 0,
        };
        total = total
            .checked_add(amount)
            .ok_or("Loop reservation overflow")?;
    }
    memo.insert(id.into(), total);
    Ok(total)
}
impl Roadmap {
    pub fn admit(&mut self, run: &super::state::Run) -> Result<Option<Claim>> {
        let Some(admission) = &run.pin.bundle.definitions[&run.pin.bundle.root].admission else {
            return Ok(None);
        };
        require(
            admission.roadmap == self.id
                && admission.identity == self.identity
                && admission.revision == self.head().revision,
            "Stale roadmap admission identity/revision",
        )?;
        let proposal = self.head().proposal.clone();
        let now = run.created_unix_ms;
        require(
            now >= proposal.horizon.start_unix_ms && now < proposal.horizon.end_unix_ms,
            "Roadmap horizon does not admit this Run",
        )?;
        require(
            self.progress
                .values()
                .filter_map(|p| p.claim.as_ref())
                .all(|c| now >= c.started_unix_ms && c.settled_unix_ms.is_none_or(|t| now >= t)),
            "Roadmap admission clock moved backwards",
        )?;
        if let Some(c) = self
            .progress
            .values()
            .filter_map(|p| p.claim.as_ref())
            .find(|c| c.execution_id == run.execution_id)
        {
            require(c.run_id == run.id, "Claim Run identity differs")?;
            return Ok(Some(c.clone()));
        }
        let mut goals = proposal.goals.clone();
        goals.sort_by_key(|g| (g.priority, g.id.clone()));
        let total = self.committed()?;
        let daily = self.window_usage(now, Some(now / 86_400_000))?;
        let rolling = self.window_usage(now, None)?;
        let declared = declared_loop(&run.pin.bundle, &run.pin.bundle.root, &mut BTreeMap::new())?;
        for goal in goals {
            let progress = &self.progress[&goal.id];
            if progress.protected() {
                continue;
            }
            let reason = if !goal.proposed_eligible {
                goal.deferral_reason.clone()
            } else if goal
                .dependencies
                .iter()
                .any(|d| self.progress[d].state != GoalState::Completed)
            {
                Some("Dependency lacks accepted complete-loop evidence".into())
            } else if proposal.goals.iter().any(|other| {
                other.id != goal.id
                    && self.progress[&other.id].occupies_partition()
                    && (goal.conflicts.contains(&other.id)
                        || other.conflicts.contains(&goal.id)
                        || goal.same_partition(other))
            }) {
                Some(
                    "Conflicting or duplicate partition goal is active, uncertain or complete"
                        .into(),
                )
            } else if goal.loop_tokens < declared {
                Some("Complete-loop allowance does not cover all declared Task attempts".into())
            } else if goal.loop_tokens > run.limits.max_tokens {
                Some("Whole optimization loop exceeds per-run allowance; explicitly narrow scope or defer".into())
            } else if total
                .checked_add(goal.loop_tokens)
                .is_none_or(|v| v > proposal.max_tokens)
            {
                Some("Whole optimization loop exceeds roadmap-total allowance".into())
            } else if daily
                .checked_add(goal.loop_tokens)
                .is_none_or(|v| v > proposal.daily_tokens)
            {
                Some("Whole optimization loop exceeds UTC daily allowance".into())
            } else if rolling
                .checked_add(goal.loop_tokens)
                .is_none_or(|v| v > proposal.rolling_tokens)
            {
                Some("Whole optimization loop exceeds rolling allowance".into())
            } else {
                None
            };
            let progress = self.progress.get_mut(&goal.id).unwrap();
            if let Some(reason) = reason {
                progress.state = GoalState::Deferred;
                progress.reason = Some(reason);
                continue;
            }
            let claim = Claim {
                run_id: run.id.clone(),
                execution_id: run.execution_id.clone(),
                revision: admission.revision.clone(),
                goal_id: goal.id.clone(),
                allowance: goal.loop_tokens,
                started_unix_ms: now,
                settled_unix_ms: None,
            };
            progress.state = GoalState::Active;
            progress.reason = Some("Whole optimization loop reserved before dispatch".into());
            progress.captured_goal = Some(hash(&goal)?);
            progress.reserved_tokens = goal.loop_tokens;
            progress.claim = Some(claim.clone());
            return Ok(Some(claim));
        }
        Ok(None)
    }
    fn committed(&self) -> Result<u64> {
        self.progress
            .values()
            .try_fold(0u64, |n, p| {
                n.checked_add(p.actual_tokens)?
                    .checked_add(p.reserved_tokens)
            })
            .ok_or("Roadmap accounting overflow".into())
    }
    fn window_usage(&self, now: u64, day: Option<u64>) -> Result<u64> {
        let window = self.head().proposal.rolling_window_ms;
        self.progress.values().try_fold(0u64, |n, p| {
            let charge = match &p.claim {
                Some(c)
                    if c.settled_unix_ms.is_none()
                        || p.reserved_tokens > 0
                        || match day {
                            Some(day) => {
                                c.started_unix_ms / 86_400_000 == day
                                    || c.settled_unix_ms.is_some_and(|t| t / 86_400_000 == day)
                            }
                            None => c
                                .settled_unix_ms
                                .is_some_and(|t| t > now.saturating_sub(window)),
                        } =>
                {
                    p.actual_tokens
                        .checked_add(p.reserved_tokens)
                        .ok_or("Roadmap charge overflow")?
                }
                _ => 0,
            };
            n.checked_add(charge)
                .ok_or("Roadmap window accounting overflow".into())
        })
    }
    pub fn reconcile(&mut self, runs: &BTreeMap<String, super::state::Run>) -> Result<()> {
        let goals = self.head().proposal.goals.clone();
        for goal in goals {
            let p = self
                .progress
                .get_mut(&goal.id)
                .ok_or("Missing goal progress")?;
            let Some(claim) = &mut p.claim else { continue };
            let run = runs.get(&claim.run_id).ok_or("Goal claim lost its Run")?;
            require(
                run.execution_id == claim.execution_id
                    && run
                        .goal_claim
                        .as_ref()
                        .is_some_and(|c| c.goal_id == goal.id && c.revision == claim.revision),
                "Goal claim differs from native Run",
            )?;
            p.task_charges = run.task_charges.clone();
            p.actual_tokens = p
                .task_charges
                .values()
                .try_fold(0u64, |n, c| n.checked_add(c.charged.unwrap_or(0)))
                .ok_or("Roadmap charge overflow")?;
            let unknown = p
                .task_charges
                .values()
                .try_fold(0u64, |n, c| n.checked_add(c.reserved()))
                .ok_or("Roadmap reservation overflow")?;
            // A never-dispatched cancelled Run can release its allowance. Any ambiguous native
            // charge retains the whole unused loop across midnight and revisions, indefinitely.
            let settled = !run.status.active()
                && !p
                    .task_charges
                    .values()
                    .any(super::state::TaskCharge::uncertain);
            claim.settled_unix_ms = settled.then_some(run.updated_unix_ms);
            p.reserved_tokens = if settled {
                0
            } else {
                goal.loop_tokens
                    .saturating_sub(p.actual_tokens)
                    .max(unknown)
            };
            p.task_receipts.clear();
            let mut seen = BTreeSet::new();
            for frame in run.frames.values() {
                for step in &frame.steps {
                    for value in step.outputs.values() {
                        if let Datum::Task(receipt) = value {
                            if seen.insert((&receipt.task_id, &receipt.revision_id)) {
                                p.task_receipts.push(receipt.clone());
                            }
                        }
                    }
                }
            }
            if run.status == super::state::Status::Succeeded
                && settled
                && !p.task_receipts.is_empty()
            {
                p.state = GoalState::Completed;
                p.reason =
                    Some("Pinned Scenario completion and accepted native Task receipts".into());
                p.completed_work = goal.remaining_work.clone();
            } else if !run.status.active() {
                p.state = GoalState::Deferred;
                p.reason=Some(if settled {"Run terminated without accepted complete-loop evidence"} else {"Run terminated with unknown Task charge; complete-loop reservation retained"}.into());
            }
        }
        self.validate()
    }
}
