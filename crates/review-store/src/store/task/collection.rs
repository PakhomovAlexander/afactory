//! Store hygiene (ADR-0135): per-Task CAS sizes, and collecting finished Tasks.
//!
//! Collection is one versioned transition. Under the Store's exclusive writer lock, and only
//! when no Task holds a live writer lease, `apply_task_collection` appends one
//! `task_collected` tombstone per collected Task and commits; then, under the lock again, it
//! sweeps every CAS object that no uncollected record reaches. A tombstone references no
//! artifact, so a collected Task's objects become unreachable unless another record reaches
//! them. The sweep is a pure function of the log, so a process that stopped between the
//! tombstones and the end of the sweep leaves a consistent Store the next sweep finishes.
//!
//! Reachability is conservative by construction: a record's roots are its events' artifact
//! references, every digest its event payloads and Attempt wall rows spell, and the walk goes
//! through every digest each reached object spells. A spelling that is not a reference only
//! keeps an object longer; a reference is never missed however a record nests it.

use std::collections::HashMap;

use review_core::task::collection::{TASK_COLLECTED_V1, TaskCollectedV1};
use review_core::task::input_bindings::{TaskInputBindingV1, TaskInputBindingsV1};
use rusqlite::OptionalExtension;
use serde::Serialize;

use super::*;
use crate::cas::FiledObject;

/// The writer name a tombstone records.
pub const COLLECTOR: &str = "af-task-gc";

/// A collected Task as its log still shows it: the tombstone, and the result its `finished`
/// transition named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedTask {
    pub collected: TaskCollectedV1,
    pub result_id: String,
}

/// The bytes of CAS objects one Task reaches: those no other record reaches, and those it
/// shares with another Task or Campaign record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TaskFootprint {
    pub exclusive_objects: u64,
    pub exclusive_bytes: u64,
    pub shared_objects: u64,
    pub shared_bytes: u64,
}

/// Every object in the CAS, and the objects no uncollected record reaches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StoreTotals {
    pub objects: u64,
    pub bytes: u64,
    pub unreachable_objects: u64,
    pub unreachable_bytes: u64,
}

/// `af task list --sizes`: the Store's totals and each uncollected Task's footprint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreInventory {
    pub totals: StoreTotals,
    pub tasks: BTreeMap<String, TaskFootprint>,
}

/// Why `af task gc` does or does not collect one Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CollectionDisposition {
    /// Finished, beyond the newest `keep`, older than the age bound, and named by no binding.
    Collect,
    /// Among the newest `keep` finished Tasks.
    KeptNewest,
    /// Its last event is not older than the age bound.
    KeptRecent,
    /// Never collected: the Task is running.
    Running,
    /// Never collected: the Task has not finished.
    Unfinished,
    /// Never collected: a writer holds the Task's lease.
    WriterLease { until_unix_ms: u64 },
    /// Never collected: another Task's `af/TaskInputBindings@1` names it.
    BoundBy { tasks: Vec<String> },
}

impl CollectionDisposition {
    pub fn collects(&self) -> bool {
        matches!(self, Self::Collect)
    }
}

/// One uncollected Task in a collection plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionCandidate {
    pub task_id: String,
    pub kind: String,
    pub revision_id: String,
    pub result_id: Option<String>,
    pub outcome: Option<String>,
    pub chargeable_tokens: String,
    /// Settled Attempts whose usage is unknown (ADR-0143); the tombstone keeps the count.
    pub unknown_usage_attempts: u64,
    pub last_event_unix_ms: u64,
    pub disposition: CollectionDisposition,
    pub footprint: TaskFootprint,
    /// For a Task this plan collects: the bytes of objects it reaches that no retained record
    /// reaches. Zero otherwise.
    pub collected_bytes: u64,
}

/// What `af task gc` previews and `--apply` acts on. Computed from the log and the CAS only;
/// computing it writes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionPlan {
    pub now_unix_ms: u64,
    pub older_than_ms: u64,
    pub keep: usize,
    pub tasks: Vec<CollectionCandidate>,
    pub collected: Vec<CollectedTask>,
    /// Tasks whose writer lease is live: while any exists, no Store lease can be taken.
    pub live_writers: Vec<(String, u64)>,
    pub totals: StoreTotals,
    /// Objects no record the plan retains reaches: what a sweep after this plan removes.
    pub reclaimable_objects: u64,
    pub reclaimable_bytes: u64,
}

/// How far `apply_task_collection` got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionOutcome {
    pub plan: CollectionPlan,
    /// Tasks this run tombstoned.
    pub tombstoned: Vec<String>,
    /// Whether the sweep ran to its end.
    pub swept: bool,
    pub removed_objects: u64,
    pub removed_bytes: u64,
}

/// Reachability over one reading of the log and the CAS.
struct Reach {
    objects: Vec<FiledObject>,
    /// Each uncollected record's closure, by run ID.
    runs: BTreeMap<String, Vec<usize>>,
    /// How many uncollected records reach each object.
    count: Vec<u32>,
}

impl Reach {
    fn totals(&self) -> StoreTotals {
        let mut totals = StoreTotals::default();
        for (index, object) in self.objects.iter().enumerate() {
            totals.objects += 1;
            totals.bytes += object.len;
            if self.count[index] == 0 {
                totals.unreachable_objects += 1;
                totals.unreachable_bytes += object.len;
            }
        }
        totals
    }

    fn footprint(&self, run_id: &str) -> TaskFootprint {
        let mut footprint = TaskFootprint::default();
        for &index in self.runs.get(run_id).into_iter().flatten() {
            let len = self.objects[index].len;
            if self.count[index] == 1 {
                footprint.exclusive_objects += 1;
                footprint.exclusive_bytes += len;
            } else {
                footprint.shared_objects += 1;
                footprint.shared_bytes += len;
            }
        }
        footprint
    }

    /// With `collected` runs gone: how many records still reach each object.
    fn retained(&self, collected: &BTreeSet<String>) -> Vec<u32> {
        let mut count = self.count.clone();
        for run in collected {
            for &index in self.runs.get(run).into_iter().flatten() {
                count[index] -= 1;
            }
        }
        count
    }
}

/// The last Task transition of a run, parsed without reading any artifact.
fn tombstone_of(payload: &str) -> Result<Option<TaskCollectedV1>, StoreError> {
    let transition: TaskTransitionV1 = serde_json::from_str(payload)?;
    transition.validate().map_err(conflict)?;
    Ok(match transition.change {
        TaskChangeV1::TaskCollected { collected } => Some(collected),
        _ => None,
    })
}

fn spelled(text: &str, found: &mut BTreeSet<String>) {
    crate::cas::scan_digests(text.as_bytes(), found);
}

/// Every Task a bindings record names.
/// The tombstone a Task's log ends in, read on `connection` — the Store's, or a writer's open
/// transaction, so an opening Task can refuse a collected binding under the same lock that
/// collection writes with (ADR-0135).
pub(super) fn tombstone_in(
    connection: &rusqlite::Connection,
    run_id: &str,
) -> Result<Option<TaskCollectedV1>, StoreError> {
    if !run_id.starts_with("task:") {
        return Ok(None);
    }
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM events WHERE run_id=?1 AND type='TaskTransition@5'
             ORDER BY sequence DESC LIMIT 1",
            [run_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(collected) = payload.as_deref().map(tombstone_of).transpose()?.flatten() else {
        return Ok(None);
    };
    if task_run_id(&collected.task_id)? != run_id {
        return Err(conflict("Task tombstone names another Task"));
    }
    Ok(Some(collected))
}

fn bound_tasks(binding: &TaskInputBindingV1, out: &mut BTreeSet<String>) {
    out.extend(binding.task.iter().map(|task| task.task_id.clone()));
    for further in &binding.also {
        bound_tasks(further, out);
    }
}

impl TaskProjection {
    /// Policy time of the Task's last event.
    pub fn last_event_unix_ms(&self) -> u64 {
        self.last_time
    }
}

impl EventStore {
    /// The tombstone a collected Task's log ends in, read without any artifact.
    pub fn task_tombstone(&self, task_id: &str) -> Result<Option<TaskCollectedV1>, StoreError> {
        self.run_tombstone(&task_run_id(task_id)?)
    }

    /// A collected Task's tombstone and the result its log named, from the log alone: the
    /// projection of a collected Task stops at the tombstone and validates no artifact.
    pub fn collected_task(&self, task_id: &str) -> Result<Option<CollectedTask>, StoreError> {
        let Some(collected) = self.task_tombstone(task_id)? else {
            return Ok(None);
        };
        let mut result_id = None;
        for event in self.replay(&task_run_id(task_id)?)? {
            if let TaskChangeV1::Finished { result_id: id } = read_task_transition(&event)?.change {
                result_id = Some(id);
            }
        }
        let result_id =
            result_id.ok_or_else(|| conflict("A collected Task log has no finished result"))?;
        Ok(Some(CollectedTask {
            collected,
            result_id,
        }))
    }

    /// Every collected Task of the Store, in label order.
    pub fn collected_tasks(&self) -> Result<Vec<CollectedTask>, StoreError> {
        let mut out = Vec::new();
        for run_id in self.run_ids()? {
            if let Some(collected) = self.run_tombstone(&run_id)? {
                out.push(
                    self.collected_task(&collected.task_id)?
                        .ok_or_else(|| conflict("Task tombstone disappeared"))?,
                );
            }
        }
        out.sort_by(|left, right| left.collected.task_id.cmp(&right.collected.task_id));
        Ok(out)
    }

    /// The tombstone of a Task run, checked against the run's identity; `None` for a run
    /// that is not a Task log or not collected.
    pub(super) fn run_tombstone(
        &self,
        run_id: &str,
    ) -> Result<Option<TaskCollectedV1>, StoreError> {
        tombstone_in(&self.conn, run_id)
    }

    /// Every Task the bindings records in `revision`'s provenance name.
    pub(super) fn bound_tasks_of_revision(
        cas: &Cas,
        revision: &TaskRevisionV1,
    ) -> Result<BTreeSet<String>, StoreError> {
        let mut named = BTreeSet::new();
        for id in &revision.provenance.input_artifact_ids {
            let Some(found) = cas
                .get_optional_artifact(id)
                .map_err(|error| StoreError::Artifact(error.to_string()))?
            else {
                continue;
            };
            if found.artifact_type != TASK_INPUT_BINDINGS_V1 {
                continue;
            }
            let record: TaskInputBindingsV1 = serde_json::from_value(found.payload)?;
            record.validate().map_err(conflict)?;
            for binding in record.bindings.values() {
                bound_tasks(binding, &mut named);
            }
        }
        named.remove(&revision.task_id);
        Ok(named)
    }

    /// The Store's totals and each uncollected Task's footprint (`af task list --sizes`).
    pub fn store_inventory(&self, cas: &Cas) -> Result<StoreInventory, StoreError> {
        let reach = self.reach(cas)?;
        let tasks = self
            .map_tasks(cas, |task| task.task_id)?
            .into_iter()
            .map(|task_id| {
                let footprint = reach.footprint(&task_run_id(&task_id)?);
                Ok((task_id, footprint))
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(StoreInventory {
            totals: reach.totals(),
            tasks,
        })
    }

    /// Read every uncollected record's roots and walk the CAS from them.
    fn reach(&self, cas: &Cas) -> Result<Reach, StoreError> {
        let objects = cas
            .filed_objects()
            .map_err(|error| StoreError::Artifact(error.to_string()))?;
        let index: HashMap<&str, usize> = objects
            .iter()
            .enumerate()
            .map(|(position, object)| (object.digest.as_str(), position))
            .collect();
        let mut runs: BTreeSet<String> = self.run_ids()?.into_iter().collect();
        {
            let mut walls = self
                .conn
                .prepare("SELECT DISTINCT run_id FROM attempt_wall ORDER BY run_id")?;
            let rows = walls.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                runs.insert(row?);
            }
        }
        let mut edges: Vec<Option<Vec<usize>>> = vec![None; objects.len()];
        let mut seen = vec![0_u32; objects.len()];
        let mut count = vec![0_u32; objects.len()];
        let mut closures = BTreeMap::new();
        let mut stamp = 0_u32;
        for run_id in runs {
            if self.run_tombstone(&run_id)?.is_some() {
                continue;
            }
            let mut roots = BTreeSet::new();
            {
                let mut events = self
                    .conn
                    .prepare("SELECT artifact_refs, payload FROM events WHERE run_id=?1")?;
                let rows = events.query_map([&run_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                for row in rows {
                    let (refs, payload) = row?;
                    spelled(&refs, &mut roots);
                    spelled(&payload, &mut roots);
                }
                let mut walls = self.conn.prepare(
                    "SELECT COALESCE(usage_v3_json,''), COALESCE(usage_observation_v1_json,'')
                     FROM attempt_wall WHERE run_id=?1",
                )?;
                let rows = walls.query_map([&run_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                for row in rows {
                    let (usage, observation) = row?;
                    spelled(&usage, &mut roots);
                    spelled(&observation, &mut roots);
                }
            }
            stamp += 1;
            let mut closure = Vec::new();
            let mut stack: Vec<usize> = roots
                .iter()
                .filter_map(|digest| index.get(digest.as_str()).copied())
                .collect();
            while let Some(position) = stack.pop() {
                if seen[position] == stamp {
                    continue;
                }
                seen[position] = stamp;
                closure.push(position);
                count[position] += 1;
                if edges[position].is_none() {
                    let spelled = cas
                        .spelled_digests(&objects[position].digest)
                        .map_err(|error| StoreError::Artifact(error.to_string()))?;
                    edges[position] = Some(
                        spelled
                            .iter()
                            .filter_map(|digest| index.get(digest.as_str()).copied())
                            .collect(),
                    );
                }
                stack.extend(
                    edges[position]
                        .iter()
                        .flatten()
                        .filter(|next| seen[**next] != stamp),
                );
            }
            closures.insert(run_id, closure);
        }
        drop(index);
        Ok(Reach {
            objects,
            runs: closures,
            count,
        })
    }

    /// Every Task the bindings records of `task`'s revisions name.
    fn bindings_of(
        &self,
        cas: &Cas,
        task: &TaskProjection,
    ) -> Result<BTreeSet<String>, StoreError> {
        let mut revisions = BTreeSet::from([task.revision_id.clone()]);
        for event in self.replay(&task_run_id(&task.task_id)?)? {
            match read_task_transition(&event)?.change {
                TaskChangeV1::Opened { revision_id, .. }
                | TaskChangeV1::SourceRefreshed { revision_id, .. }
                | TaskChangeV1::PlanningCompleted { revision_id, .. } => {
                    revisions.insert(revision_id);
                }
                _ => {}
            }
        }
        let mut named = BTreeSet::new();
        for revision_id in revisions {
            let value = revision(cas, &revision_id)?;
            named.extend(Self::bound_tasks_of_revision(cas, &value)?);
        }
        named.remove(&task.task_id);
        Ok(named)
    }

    /// What `af task gc` would do now: every uncollected Task's disposition and footprint, the
    /// Tasks already collected, the live writers, and the bytes a sweep after it removes.
    /// Writes nothing.
    pub fn plan_task_collection(
        &self,
        cas: &Cas,
        older_than_ms: u64,
        keep: usize,
    ) -> Result<CollectionPlan, StoreError> {
        self.plan_at(cas, older_than_ms, keep, now()?, None)
    }

    /// The plan, with collected bytes counted for the collecting Tasks only: with `only`, the
    /// Tasks the rules would collect that it names, so an object shared with a Task that stays
    /// is never counted as freed.
    fn plan_at(
        &self,
        cas: &Cas,
        older_than_ms: u64,
        keep: usize,
        now_unix_ms: u64,
        only: Option<&BTreeSet<String>>,
    ) -> Result<CollectionPlan, StoreError> {
        struct Seen {
            task: TaskProjection,
            result: Option<TaskResultV1>,
            bindings: BTreeSet<String>,
        }
        let mut seen = Vec::new();
        for task in self.map_tasks(cas, |task| task)? {
            let result = match &task.phase {
                TaskPhaseV1::Finished { result_id } => Some(payload::<TaskResultV1>(
                    cas,
                    result_id,
                    task::TASK_RESULT_V1,
                )?),
                _ => None,
            };
            let bindings = self.bindings_of(cas, &task)?;
            seen.push(Seen {
                task,
                result,
                bindings,
            });
        }
        let mut bound_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in &seen {
            for named in &entry.bindings {
                bound_by
                    .entry(named.clone())
                    .or_default()
                    .push(entry.task.task_id.clone());
            }
        }
        let mut finished: Vec<&Seen> = seen
            .iter()
            .filter(|entry| matches!(entry.task.phase, TaskPhaseV1::Finished { .. }))
            .collect();
        finished.sort_by(|left, right| {
            right
                .task
                .last_time
                .cmp(&left.task.last_time)
                .then_with(|| left.task.task_id.cmp(&right.task.task_id))
        });
        let newest: BTreeSet<&str> = finished
            .iter()
            .take(keep)
            .map(|entry| entry.task.task_id.as_str())
            .collect();
        let live_writers: Vec<(String, u64)> = seen
            .iter()
            .filter(|entry| entry.task.lease_until > now_unix_ms)
            .map(|entry| (entry.task.task_id.clone(), entry.task.lease_until))
            .collect();
        let reach = self.reach(cas)?;
        let mut tasks = Vec::new();
        for entry in &seen {
            let task = &entry.task;
            let disposition = match &task.phase {
                TaskPhaseV1::Running {} => CollectionDisposition::Running,
                TaskPhaseV1::Finished { .. } if task.lease_until > now_unix_ms => {
                    CollectionDisposition::WriterLease {
                        until_unix_ms: task.lease_until,
                    }
                }
                TaskPhaseV1::Finished { .. } => match bound_by.get(&task.task_id) {
                    Some(tasks) => CollectionDisposition::BoundBy {
                        tasks: tasks.clone(),
                    },
                    None if newest.contains(task.task_id.as_str()) => {
                        CollectionDisposition::KeptNewest
                    }
                    None if task.last_time.saturating_add(older_than_ms) > now_unix_ms => {
                        CollectionDisposition::KeptRecent
                    }
                    None => CollectionDisposition::Collect,
                },
                _ => CollectionDisposition::Unfinished,
            };
            tasks.push(CollectionCandidate {
                task_id: task.task_id.clone(),
                kind: task.revision.kind.clone(),
                revision_id: task.revision_id.clone(),
                result_id: match &task.phase {
                    TaskPhaseV1::Finished { result_id } => Some(result_id.clone()),
                    _ => None,
                },
                outcome: entry
                    .result
                    .as_ref()
                    .map(|result| result.domain_conclusion.clone()),
                chargeable_tokens: task
                    .execution
                    .as_ref()
                    .map_or(0, |execution| execution.budget.committed_tokens())
                    .to_string(),
                unknown_usage_attempts: task.execution.as_ref().map_or(0, |execution| {
                    execution
                        .attempt_accounting()
                        .iter()
                        .filter(|attempt| attempt.unknown_usage.is_some())
                        .count() as u64
                }),
                last_event_unix_ms: task.last_time,
                disposition,
                footprint: reach.footprint(&task_run_id(&task.task_id)?),
                collected_bytes: 0,
            });
        }
        let collects = |task: &CollectionCandidate| {
            task.disposition.collects() && only.is_none_or(|only| only.contains(&task.task_id))
        };
        let collecting: BTreeSet<String> = tasks
            .iter()
            .filter(|task| collects(task))
            .map(|task| task_run_id(&task.task_id))
            .collect::<Result<_, _>>()?;
        let retained = reach.retained(&collecting);
        for task in tasks.iter_mut().filter(|task| collects(task)) {
            let run = task_run_id(&task.task_id)?;
            task.collected_bytes = reach
                .runs
                .get(&run)
                .into_iter()
                .flatten()
                .filter(|index| retained[**index] == 0)
                .map(|index| reach.objects[*index].len)
                .sum();
        }
        let (reclaimable_objects, reclaimable_bytes) = reach
            .objects
            .iter()
            .zip(&retained)
            .filter(|(_, count)| **count == 0)
            .fold((0, 0), |(objects, bytes), (object, _)| {
                (objects + 1, bytes + object.len)
            });
        Ok(CollectionPlan {
            now_unix_ms,
            older_than_ms,
            keep,
            tasks,
            collected: self.collected_tasks()?,
            live_writers,
            totals: reach.totals(),
            reclaimable_objects,
            reclaimable_bytes,
        })
    }

    /// `af task gc --apply`. Takes the Store's exclusive writer lock — refused while any Task
    /// holds a live writer lease — plans under it, appends one tombstone per collected Task and
    /// commits; then sweeps under the lock again. With `stop_after_tombstones` it returns before
    /// the sweep, as a process stopped there would; the next call finishes it.
    pub fn apply_task_collection(
        &mut self,
        cas: &Cas,
        older_than_ms: u64,
        keep: usize,
        stop_after_tombstones: bool,
    ) -> Result<CollectionOutcome, StoreError> {
        self.apply_collection(cas, older_than_ms, keep, None, stop_after_tombstones)
    }

    /// Collect exactly the named finished Tasks, each only when the existing rules would
    /// collect it with no age or newest-N protection: never a running, leased or bound Task.
    /// The Storage Budget evicts finished Tasks one at a time through this (ADR-0144), so every
    /// eviction keeps its tombstone; a name the rules protect is left and reported by its
    /// disposition in the returned plan.
    pub fn apply_task_collection_of(
        &mut self,
        cas: &Cas,
        task_ids: &BTreeSet<String>,
    ) -> Result<CollectionOutcome, StoreError> {
        self.apply_collection(cas, 0, 0, Some(task_ids), false)
    }

    /// [`Self::apply_task_collection`] restricted to the named Tasks: `af task gc --apply`
    /// collects, under its age and newest-N bounds, only the Tasks whose gate leftovers are gone
    /// (ADR-0144). The plan counts collected bytes for the Tasks it collects only.
    pub fn apply_task_collection_among(
        &mut self,
        cas: &Cas,
        older_than_ms: u64,
        keep: usize,
        task_ids: &BTreeSet<String>,
        stop_after_tombstones: bool,
    ) -> Result<CollectionOutcome, StoreError> {
        self.apply_collection(
            cas,
            older_than_ms,
            keep,
            Some(task_ids),
            stop_after_tombstones,
        )
    }

    fn apply_collection(
        &mut self,
        cas: &Cas,
        older_than_ms: u64,
        keep: usize,
        only: Option<&BTreeSet<String>>,
        stop_after_tombstones: bool,
    ) -> Result<CollectionOutcome, StoreError> {
        let leased = std::time::SystemTime::now();
        let lock = self.store_lease()?;
        let now_unix_ms = now()?;
        let plan = self.plan_at(cas, older_than_ms, keep, now_unix_ms, only)?;
        refuse_live_writers(&plan.live_writers)?;
        let mut tombstoned = Vec::new();
        for task in plan.tasks.iter().filter(|task| {
            task.disposition.collects() && only.is_none_or(|only| only.contains(&task.task_id))
        }) {
            let state = self
                .task_projection(cas, &task.task_id)?
                .ok_or_else(|| conflict("Collected Task disappeared"))?;
            let epoch = state
                .epoch
                .checked_add(1)
                .ok_or_else(|| conflict("Task epoch overflow"))?;
            if now_unix_ms < state.last_time {
                return Err(conflict(
                    "Task collection clock precedes the Task's last event",
                ));
            }
            let transition = TaskTransitionV1 {
                writer: COLLECTOR.into(),
                epoch,
                now_unix_ms,
                change: TaskChangeV1::TaskCollected {
                    collected: TaskCollectedV1 {
                        schema: TASK_COLLECTED_V1.into(),
                        task_id: task.task_id.clone(),
                        kind: task.kind.clone(),
                        revision_id: task.revision_id.clone(),
                        outcome: task
                            .outcome
                            .clone()
                            .ok_or_else(|| conflict("A collected Task has no outcome"))?,
                        chargeable_tokens: task.chargeable_tokens.clone(),
                        unknown_usage_attempts: task.unknown_usage_attempts,
                        last_event_unix_ms: task.last_event_unix_ms,
                        collected_unix_ms: now_unix_ms,
                        collected_bytes: task.collected_bytes,
                    },
                },
            };
            let (event_type, value) = review_handoff::encode_transition(&transition)?;
            review_core::event::validate_event_payload(event_type, &value)
                .map_err(|error| conflict(format!("invalid Task tombstone: {error}")))?;
            let run_id = task_run_id(&task.task_id)?;
            super::super::insert_events(
                &lock,
                cas,
                &run_id,
                &[NewEvent::new(event_type, value)],
                i64::try_from(state.next_sequence)
                    .map_err(|_| conflict("Task sequence exceeds SQLite range"))?,
            )?;
            tombstoned.push(task.task_id.clone());
        }
        lock.commit()?;
        *self.task_cache.borrow_mut() = None;
        if stop_after_tombstones {
            return Ok(CollectionOutcome {
                plan,
                tombstoned,
                swept: false,
                removed_objects: 0,
                removed_bytes: 0,
            });
        }
        let (removed_objects, removed_bytes) = self.sweep(cas, leased)?;
        Ok(CollectionOutcome {
            plan,
            tombstoned,
            swept: true,
            removed_objects,
            removed_bytes,
        })
    }

    /// Remove every CAS object no uncollected record reaches, under the Store's exclusive
    /// writer lock and only while no Task holds a live writer lease. An object filed at or
    /// after `leased` is kept: it can only be an in-flight writer's, whose reference then
    /// commits or fails on its own.
    fn sweep(&self, cas: &Cas, leased: std::time::SystemTime) -> Result<(u64, u64), StoreError> {
        let lock = self.store_lease()?;
        let now_unix_ms = now()?;
        let live: Vec<(String, u64)> = self
            .map_tasks(cas, |task| (task.task_id.clone(), task.lease_until))?
            .into_iter()
            .filter(|(_, until)| *until > now_unix_ms)
            .collect();
        refuse_live_writers(&live)?;
        let reach = self.reach(cas)?;
        let mut removed = (0, 0);
        for (object, count) in reach.objects.iter().zip(&reach.count) {
            if *count != 0 || object.modified >= leased {
                continue;
            }
            removed.1 += cas
                .remove_unreachable(&object.digest)
                .map_err(|error| StoreError::Artifact(error.to_string()))?;
            removed.0 += 1;
        }
        lock.commit()?;
        Ok(removed)
    }

    /// The Store's exclusive writer lock: every append takes the same SQLite writer lock, so
    /// no event commits while it is held.
    fn store_lease(&self) -> Result<rusqlite::Transaction<'_>, StoreError> {
        rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(failure, _)
                    if failure.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    conflict("Another writer holds the Store; no Store lease was taken")
                }
                other => StoreError::Sqlite(other),
            })
    }
}

fn refuse_live_writers(live: &[(String, u64)]) -> Result<(), StoreError> {
    match live.first() {
        None => Ok(()),
        Some((task_id, until)) => Err(conflict(format!(
            "Task `{task_id}` holds a live writer lease until {}; no Store lease can be taken \
             while a writer is live",
            review_core::task::collection::collected_time(*until)
        ))),
    }
}
