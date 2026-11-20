//! Local, restartable batch admission from the authority's committed intake.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use af_contracts::EventType;
use af_contracts::backlog::batch::{
    EvidenceAccess, HoldPolicy, Journal, Policy, Reconsideration, Settlement, ZoneSnapshot,
};
use af_contracts::task::usage::DecimalU64;
use af_scenario::contract::{Limits, MAX_TIME};
use af_scenario::schedule::{
    AntMode, Execution, OccurrenceOutcome, Schedule, Skip, Zone, occurrence_id, pin_revision,
};
use af_store::{NewEvent, local_fs};
use serde::Serialize;

use super::intake::{AcceptedReport, value_id};
use crate::ProjectStore;

const STREAM: &str = "backlog-daily-batch";
pub const POLICY_FILE: &str = "backlog-batch-policy-v1.json";

#[derive(Serialize)]
pub struct IntakeEntry {
    pub report_id: String,
    pub sequence: DecimalU64,
    pub report_digest: String,
}

#[derive(Clone, Serialize)]
struct Work {
    member_id: String,
    report_id: String,
    item_id: Option<String>,
    evidence_report_id: Option<String>,
    evidence_ids: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    Accounted,
    Held,
    NeedsEvidence,
    PublicationReserved,
    DeferredPublicationCap,
    DeferredItemClaim,
}

impl Disposition {
    fn deferred(self) -> bool {
        matches!(self, Self::DeferredPublicationCap | Self::DeferredItemClaim)
    }
}

#[derive(Serialize)]
struct Member {
    #[serde(flatten)]
    work: Work,
    settlement: Option<Settlement>,
    disposition: Option<Disposition>,
}

#[derive(Serialize)]
struct Batch {
    id: String,
    previous_high_water: DecimalU64,
    high_water: DecimalU64,
    members: BTreeMap<String, Member>,
    closed_unix_ms: Option<u64>,
    publication_reservations: BTreeSet<String>,
}

#[derive(Serialize)]
struct Occurrence {
    id: String,
    local_day: i64,
    admitted_unix_ms: u64,
    due_unix_ms: u64,
    outcome: Outcome,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Outcome {
    Admitted(LocalAdmission),
    Skipped(OccurrenceOutcome),
}

#[derive(Serialize)]
#[serde(tag = "kind", rename = "admitted")]
struct LocalAdmission {
    batch_id: String,
    reserved_tokens: u64,
}

#[derive(Serialize)]
struct PendingReconsideration {
    request: Reconsideration,
    admitted_batch_id: Option<String>,
}

#[derive(Serialize)]
pub struct Audit {
    schema: &'static str,
    project_id: String,
    policy: Option<Policy>,
    pin_id: Option<String>,
    intake: Vec<IntakeEntry>,
    high_water: DecimalU64,
    closed_high_water: DecimalU64,
    occurrences: Vec<Occurrence>,
    batches: BTreeMap<String, Batch>,
    active_batch_id: Option<String>,
    reconsiderations: BTreeMap<String, PendingReconsideration>,
    pending_member_ids: Vec<String>,
}

struct Pin {
    policy: Policy,
    id: String,
    zone: Zone,
    schedule: Schedule,
    execution: Execution,
}

impl Pin {
    fn new(policy: Policy, zone: ZoneSnapshot) -> Result<Self, String> {
        let zone = Zone {
            name: zone.name,
            initial_offset: zone.initial_offset,
            transitions: zone.transitions,
            valid_until_unix_seconds: zone.valid_until_unix_seconds,
        };
        let schedule = Schedule::Daily {
            timezone: policy.timezone.clone(),
            hour: policy.hour,
            minute: policy.minute,
            start_unix_ms: policy.start_unix_ms,
        };
        let execution = Execution {
            overlap: Skip::Skip,
            missed: Skip::Skip,
            lateness_ms: policy.lateness_ms,
            per_run: Limits {
                max_tokens: policy.run_tokens,
                max_wall_ms: policy.run_wall_ms,
                max_dispatches: policy.run_dispatches,
            },
            rolling_tokens: policy.rolling_tokens,
            rolling_window_ms: policy.rolling_window_ms,
        };
        let id = pin_revision(
            &("af/backlog-batch-pin@1", &policy),
            &policy.scenario_id,
            &zone,
        )?;
        zone.validate()?;
        execution.validate_schedule(&schedule)?;
        Ok(Self {
            policy,
            id,
            zone,
            schedule,
            execution,
        })
    }
}

#[derive(Default)]
struct State {
    pin: Option<Pin>,
    next_day: i64,
    clock: u64,
    last_tick: Option<u64>,
    high_water: u64,
    closed_high_water: u64,
    occurrences: Vec<Occurrence>,
    batches: BTreeMap<String, Batch>,
    active: Option<String>,
    reconsiderations: BTreeMap<String, PendingReconsideration>,
    accounted_reports: BTreeSet<String>,
    deferred: BTreeMap<String, Work>,
    closed_times: VecDeque<u64>,
}

struct Intake {
    reports: BTreeMap<String, AcceptedReport>,
    ordered: Vec<String>,
}

impl Intake {
    fn new(reports: BTreeMap<String, AcceptedReport>) -> Self {
        let mut ordered = vec![String::new(); reports.len()];
        for (id, accepted) in &reports {
            // Intake replay has already checked the dense authority sequence.
            ordered[(accepted.receipt.body.sequence.get() - 1) as usize] = id.clone();
        }
        Self { reports, ordered }
    }

    fn len(&self) -> u64 {
        self.ordered.len() as u64
    }

    fn sequence(&self, id: &str) -> Result<u64, String> {
        self.reports
            .get(id)
            .map(|r| r.receipt.body.sequence.get())
            .ok_or_else(|| format!("no accepted backlog report {id}"))
    }

    fn work(
        &self,
        pin: &Pin,
        member_id: &str,
        report_id: &str,
        request: Option<&Reconsideration>,
    ) -> Work {
        let evidence_report_id = request.and_then(|r| r.evidence_report_id.clone());
        let evidence_ids = match pin.policy.evidence_access {
            EvidenceAccess::MetadataOnly => Vec::new(),
            EvidenceAccess::EvidenceIds => std::iter::once(report_id)
                .chain(evidence_report_id.as_deref())
                .flat_map(|id| self.reports[id].report.evidence.iter())
                .map(|evidence| evidence.content_id.clone())
                .collect(),
        };
        Work {
            member_id: member_id.into(),
            report_id: report_id.into(),
            item_id: request.map(|r| r.item_id.clone()),
            evidence_report_id,
            evidence_ids,
        }
    }
}

impl State {
    fn freeze(&mut self, intake: &Intake, at: u64, high_water: u64) -> Result<Vec<String>, String> {
        let pin = self.pin.as_ref().ok_or("no pinned backlog batch policy")?;
        if at == 0 || at < self.clock || high_water < self.high_water || high_water > intake.len() {
            return Err("invalid backlog batch clock or authority cutoff".into());
        }
        let end = pin
            .schedule
            .due_index(&pin.zone, at, self.next_day)?
            .ok_or("backlog batch requires a daily schedule")?;
        while self
            .closed_times
            .front()
            .is_some_and(|time| !pin.execution.in_rolling_window(*time, at))
        {
            self.closed_times.pop_front();
        }
        let mut frozen = Vec::new();
        while self.next_day <= end {
            let day = self.next_day;
            let (due, gap) = pin.schedule.due(&pin.zone, day)?;
            if due > at {
                break;
            }
            self.next_day += 1;
            if due < pin.schedule.start() {
                continue;
            }
            let id = occurrence_id("project-backlog", &pin.id, day)?;
            let used =
                (self.closed_times.len() as u64).saturating_mul(pin.execution.per_run.max_tokens);
            let outcome = if let Some(block) = pin.execution.admission_block(
                gap,
                at - due,
                AntMode::Enabled,
                None,
                self.active.is_some(),
                used,
            ) {
                Outcome::Skipped(block)
            } else {
                let mut members = std::mem::take(&mut self.deferred);
                for report_id in &intake.ordered[self.high_water as usize..high_water as usize] {
                    members.insert(
                        report_id.clone(),
                        intake.work(pin, report_id, report_id, None),
                    );
                }
                for (key, pending) in &mut self.reconsiderations {
                    let request = &pending.request;
                    if pending.admitted_batch_id.is_none()
                        && request.eligible_unix_ms <= due
                        && match &request.evidence_report_id {
                            Some(evidence) => intake.sequence(evidence)? <= high_water,
                            None => true,
                        }
                    {
                        members.insert(
                            key.clone(),
                            intake.work(pin, key, &request.report_id, Some(request)),
                        );
                        pending.admitted_batch_id = Some(id.clone());
                    }
                }
                let previous_high_water = std::mem::replace(&mut self.high_water, high_water);
                frozen.extend(members.keys().cloned());
                let closed_unix_ms = members.is_empty().then_some(at);
                if closed_unix_ms.is_some() {
                    self.closed_high_water = high_water;
                } else {
                    self.active = Some(id.clone());
                }
                self.batches.insert(
                    id.clone(),
                    Batch {
                        id: id.clone(),
                        previous_high_water: previous_high_water.into(),
                        high_water: high_water.into(),
                        members: members
                            .into_iter()
                            .map(|(key, work)| {
                                (
                                    key,
                                    Member {
                                        work,
                                        settlement: None,
                                        disposition: None,
                                    },
                                )
                            })
                            .collect(),
                        closed_unix_ms,
                        publication_reservations: BTreeSet::new(),
                    },
                );
                Outcome::Admitted(LocalAdmission {
                    batch_id: id.clone(),
                    reserved_tokens: if closed_unix_ms.is_some() {
                        0
                    } else {
                        pin.execution.per_run.max_tokens
                    },
                })
            };
            self.occurrences.push(Occurrence {
                id,
                local_day: day,
                admitted_unix_ms: at,
                due_unix_ms: due,
                outcome,
            });
        }
        self.clock = at;
        self.last_tick = Some(at);
        Ok(frozen)
    }

    fn reconsider(&mut self, intake: &Intake, request: Reconsideration) -> Result<bool, String> {
        let id = value_id(&("af/backlog-reconsideration@1", &request))?;
        if self.reconsiderations.contains_key(&id) {
            return Ok(false);
        }
        if !self.accounted_reports.contains(&request.report_id) {
            return Err("reconsideration requires an already accounted Report".into());
        }
        if let Some(evidence) = &request.evidence_report_id
            && intake.sequence(evidence)? <= intake.sequence(&request.report_id)?
        {
            return Err("reconsideration evidence must name a later accepted Report".into());
        }
        self.reconsiderations.insert(
            id,
            PendingReconsideration {
                request,
                admitted_batch_id: None,
            },
        );
        Ok(true)
    }

    fn settle(
        &mut self,
        batch_id: &str,
        member_id: &str,
        settlement: Settlement,
    ) -> Result<bool, String> {
        let pin = self.pin.as_ref().ok_or("no pinned backlog batch policy")?;
        let batch = self
            .batches
            .get_mut(batch_id)
            .ok_or("unknown backlog batch")?;
        let member = batch
            .members
            .get_mut(member_id)
            .ok_or("member is outside the frozen backlog batch")?;
        if let Some(previous) = &member.settlement {
            return if *previous == settlement {
                Ok(false)
            } else {
                Err("backlog member already settled differently".into())
            };
        }
        if self.active.as_deref() != Some(batch_id) {
            return Err("backlog batch is no longer active".into());
        }
        let disposition = match &settlement {
            Settlement::Accounted => Disposition::Accounted,
            Settlement::Held => Disposition::Held,
            Settlement::NeedsEvidence => Disposition::NeedsEvidence,
            Settlement::ReservePublication { item_id } => {
                if member
                    .work
                    .item_id
                    .as_ref()
                    .is_some_and(|pinned| pinned != item_id)
                {
                    return Err(
                        "publication reservation names a different reconsideration item".into(),
                    );
                }
                if pin.policy.hold_policy == HoldPolicy::HoldAll {
                    Disposition::Held
                } else if batch.publication_reservations.contains(item_id) {
                    Disposition::DeferredItemClaim
                } else if batch.publication_reservations.len()
                    >= pin.policy.publication_limit as usize
                {
                    Disposition::DeferredPublicationCap
                } else {
                    batch.publication_reservations.insert(item_id.clone());
                    Disposition::PublicationReserved
                }
            }
        };
        member.disposition = Some(disposition);
        member.settlement = Some(settlement);
        Ok(true)
    }

    fn close(&mut self, batch_id: &str, at: u64) -> Result<bool, String> {
        let batch = self
            .batches
            .get_mut(batch_id)
            .ok_or("unknown backlog batch")?;
        if batch.closed_unix_ms.is_some() {
            return Ok(false);
        }
        if !(self.clock..=MAX_TIME).contains(&at)
            || batch
                .members
                .values()
                .any(|member| member.disposition.is_none())
        {
            return Err("cannot close backlog batch before all members are accounted for or with an invalid clock".into());
        }
        for member in batch.members.values() {
            if member.disposition.is_some_and(Disposition::deferred) {
                let mut work = member.work.clone();
                if let Some(Settlement::ReservePublication { item_id }) = &member.settlement {
                    work.item_id = Some(item_id.clone());
                }
                self.deferred.insert(work.member_id.clone(), work);
            } else {
                self.accounted_reports.insert(member.work.report_id.clone());
            }
        }
        batch.closed_unix_ms = Some(at);
        self.closed_high_water = batch.high_water.get();
        self.active = None;
        self.clock = at;
        self.closed_times.push_back(at);
        Ok(true)
    }

    fn apply(&mut self, intake: &Intake, record: Journal) -> Result<bool, String> {
        record.validate()?;
        match record {
            Journal::Pinned { policy, zone } => {
                if self.pin.is_some() {
                    return Err("backlog batch policy is already pinned".into());
                }
                let pin = Pin::new(*policy, zone)?;
                self.next_day = pin.schedule.initial_index(&pin.zone)?;
                self.pin = Some(pin);
                Ok(true)
            }
            Journal::Ticked {
                at_unix_ms,
                high_water,
                members,
            } => {
                if self.last_tick == Some(at_unix_ms) {
                    return Err("duplicate backlog batch tick in journal".into());
                }
                if self.freeze(intake, at_unix_ms, high_water.get())? != members {
                    return Err("frozen backlog membership differs from authority replay".into());
                }
                Ok(true)
            }
            Journal::Reconsidered { request } => self.reconsider(intake, request),
            Journal::Settled {
                batch_id,
                member_id,
                settlement,
            } => self.settle(&batch_id, &member_id, settlement),
            Journal::Closed {
                batch_id,
                at_unix_ms,
            } => self.close(&batch_id, at_unix_ms),
        }
    }

    fn audit(self, project_id: String, intake: Intake) -> Audit {
        let pending_member_ids = self
            .deferred
            .into_keys()
            .chain(intake.ordered[self.high_water as usize..].iter().cloned())
            .chain(
                self.reconsiderations
                    .iter()
                    .filter(|(_, row)| row.admitted_batch_id.is_none())
                    .map(|(id, _)| id.clone()),
            )
            .collect();
        let intake = intake
            .ordered
            .into_iter()
            .map(|report_id| {
                let receipt = &intake.reports[&report_id].receipt.body;
                IntakeEntry {
                    report_id,
                    sequence: receipt.sequence,
                    report_digest: receipt.report_digest.clone(),
                }
            })
            .collect();
        let (pin_id, policy) = self
            .pin
            .map_or((None, None), |pin| (Some(pin.id), Some(pin.policy)));
        Audit {
            schema: "af/backlog-batch-audit@1",
            project_id,
            policy,
            pin_id,
            intake,
            high_water: self.high_water.into(),
            closed_high_water: self.closed_high_water.into(),
            occurrences: self.occurrences,
            batches: self.batches,
            active_batch_id: self.active,
            reconsiderations: self.reconsiderations,
            pending_member_ids,
        }
    }
}

impl ProjectStore {
    fn batch_state(&self) -> Result<(State, Intake), String> {
        let intake = Intake::new(self.backlog_intake()?);
        let mut state = State::default();
        for event in self
            .events
            .replay(STREAM)
            .map_err(|_| "could not replay backlog batches")?
        {
            if event.event_type != EventType::BacklogBatchStoredV1
                || !event.artifact_refs.is_empty()
            {
                return Err("invalid event in backlog batch journal".into());
            }
            let record = serde_json::from_value(event.payload)
                .map_err(|_| "invalid backlog batch journal")?;
            if !state.apply(&intake, record)? {
                return Err("redundant operation in backlog batch journal".into());
            }
        }
        if let Some(pin) = &state.pin {
            self.check_batch_authority(&pin.policy, &intake)?;
        }
        Ok((state, intake))
    }

    fn check_batch_authority(&self, policy: &Policy, intake: &Intake) -> Result<(), String> {
        if policy.project_id != self.project_id
            || self.backlog_authority_id()?.as_deref() != Some(&policy.authority_id)
            || intake
                .reports
                .values()
                .any(|row| row.receipt.body.authority_id != policy.authority_id)
        {
            return Err("backlog batch policy does not match the Project authority intake".into());
        }
        Ok(())
    }

    fn append_batch_journal(&mut self, records: Vec<Journal>) -> Result<(), String> {
        let events = records
            .into_iter()
            .map(|record| {
                record.validate()?;
                let at = match &record {
                    Journal::Ticked { at_unix_ms, .. } | Journal::Closed { at_unix_ms, .. } => {
                        *at_unix_ms
                    }
                    _ => crate::now_unix_ms(),
                };
                let payload = serde_json::to_value(record)
                    .map_err(|_| "could not encode backlog batch journal")?;
                Ok(NewEvent::new(EventType::BacklogBatchStoredV1, payload)
                    .at(af_contracts::task::collection::collected_time(at)))
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.events
            .append_batch(STREAM, &self.cas, &events)
            .map_err(|error| format!("could not commit backlog batch journal: {error}"))?;
        Ok(())
    }

    pub fn tick_backlog_batch(&mut self, now: u64) -> Result<Audit, String> {
        let (mut state, intake) = self.batch_state()?;
        let bytes = local_fs::read_bounded(&self.directory.join(POLICY_FILE), 16_384)
            .map_err(|error| format!("reading backlog batch policy: {error}"))?;
        let policy: Policy =
            serde_json::from_slice(&bytes).map_err(|_| "invalid backlog batch policy document")?;
        self.check_batch_authority(&policy, &intake)?;
        let mut records = Vec::new();
        if let Some(pin) = &state.pin {
            if pin.policy != policy {
                return Err("backlog batch policy differs from its pinned revision".into());
            }
        } else {
            policy.validate()?;
            let zone = Zone::load(&policy.timezone)?;
            let record = Journal::Pinned {
                policy: Box::new(policy),
                zone: ZoneSnapshot {
                    name: zone.name,
                    initial_offset: zone.initial_offset,
                    transitions: zone.transitions,
                    valid_until_unix_seconds: zone.valid_until_unix_seconds,
                },
            };
            state.apply(&intake, record.clone())?;
            records.push(record);
        }
        if state.last_tick != Some(now) {
            let members = state.freeze(&intake, now, intake.len())?;
            records.push(Journal::Ticked {
                at_unix_ms: now,
                high_water: intake.len().into(),
                members,
            });
            self.append_batch_journal(records)?;
        }
        Ok(state.audit(self.project_id.clone(), intake))
    }

    pub fn backlog_batch_audit(&self) -> Result<Audit, String> {
        let (state, intake) = self.batch_state()?;
        Ok(state.audit(self.project_id.clone(), intake))
    }

    pub fn reconsider_backlog_batch(&mut self, request: Reconsideration) -> Result<Audit, String> {
        self.update_backlog_batch(Journal::Reconsidered { request })
    }

    pub fn settle_backlog_batch(
        &mut self,
        batch_id: String,
        member_id: String,
        settlement: Settlement,
    ) -> Result<Audit, String> {
        self.update_backlog_batch(Journal::Settled {
            batch_id,
            member_id,
            settlement,
        })
    }

    pub fn close_backlog_batch(
        &mut self,
        batch_id: String,
        at_unix_ms: u64,
    ) -> Result<Audit, String> {
        self.update_backlog_batch(Journal::Closed {
            batch_id,
            at_unix_ms,
        })
    }

    fn update_backlog_batch(&mut self, record: Journal) -> Result<Audit, String> {
        let (mut state, intake) = self.batch_state()?;
        if state.apply(&intake, record.clone())? {
            self.append_batch_journal(vec![record])?;
        }
        Ok(state.audit(self.project_id.clone(), intake))
    }
}
