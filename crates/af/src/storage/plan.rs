//! A sweep's one plan (ADR-0144, ADR-0159 clause 13). [`make`] takes one inventory snapshot and
//! returns, in order, every entry the sweep removes with the bytes it frees, its rule
//! (`recovery`, `collection`, `keep_surveys`, `budget`) and the total after it, and every file
//! or Survey it leaves with its reason (`held`, `foreign`, `oversize`, `unreadable_name`,
//! `too_deep`, `unmeasurable`). It
//! reads nothing itself but the Surveys it picks, each compared with the identity the snapshot
//! measured: it works over an in-memory model of the snapshot that its own removals update, so
//! the bytes of a record an interruption moved under `cas/removing` are subtracted once, from
//! the Project Store's entry that holds them, and never counted again. What an interruption
//! left, the claims of dead processes and the deletions in Project Stores, is in the snapshot
//! and planned first, so nothing is removed that the plan does not list: a claim inside an entry
//! the snapshot measured whole (a Project Store) is counted once, in that entry, and a dead
//! pass's container is an entry of its own after the records it holds. A registration whose path
//! holds no Store is dropped by the plan too, after the inventory reached its claims, never
//! before. A directory nested deeper than the removal walk is left `too_deep`, and a claim the
//! snapshot could not measure `unmeasurable`. The preview prints
//! the plan; the applied sweep runs it entry by entry with [`run`] and reports what each entry
//! actually did, an entry that failed (the lock lost, what it names replaced or gone) kept in its
//! place as `failed: <reason>` with what it actually freed (nothing, unless a deletion stopped
//! part way), and never plans again. Its running total reconciles an entry that grew or shrank
//! since the snapshot: what a removal measured its entry to hold when it started replaces the
//! snapshot's bytes before what left the disk is taken out, and a deletion that stopped part
//! way leaves the total `unknown` when what it left cannot be measured.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use review_telemetry::budget;

use super::{
    Entry, Evicted, FAILED, HOUR_MS, Kind, Removal, Rule, Stop, SweepReport, TelemetryChanged,
};
use crate::projects::survey::collect::{self, Judged, LeftReason};

/// How the applied sweep removes one planned entry.
#[derive(Debug, Clone)]
pub(crate) enum Job {
    /// A finished Task, through its Store's own collection.
    Task,
    /// The entry the snapshot measured, through the rule that owns it.
    Evict(Box<Entry>),
    /// A Survey's tombstone and deletion, or the deletion an interruption left.
    Survey(collect::Job),
    /// A telemetry segment, evicted through its directory's model (ADR-0153 clause 8).
    Telemetry(Box<Entry>),
}

/// What running one planned entry did, or why it did not, and what it freed before it stopped.
pub(crate) type Outcome = Result<Ran, Failed>;

/// What one planned entry that ran did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ran {
    /// The allocated bytes that left the disk.
    pub(crate) freed: u64,
    /// A Survey collected `unreadable`: why.
    pub(crate) unreadable: Option<String>,
    /// What the entry held when its removal started, stated through its own descriptor
    /// immediately before anything was unlinked; `None` where nothing measured it (a Survey's
    /// file, a Task's collection), whose freed bytes are then all the total loses.
    pub(crate) started: Option<u64>,
    /// A telemetry entry found changed when its eviction checked it again: left in place whole
    /// and reported under `telemetry.changed`, never as removed (ADR-0144).
    pub(crate) changed: Option<Box<TelemetryChanged>>,
}

impl Ran {
    /// An entry that freed `freed` bytes and measured nothing else.
    pub(crate) fn freed(freed: u64) -> Self {
        Self {
            freed,
            unreadable: None,
            started: None,
            changed: None,
        }
    }
}

/// Why a planned entry failed, the allocated bytes that left the disk before it did (none,
/// unless a deletion stopped part way), and what the entry still holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failed {
    pub(crate) reason: String,
    pub(crate) freed: u64,
    pub(crate) remains: Remains,
}

/// What a planned entry that failed still holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Remains {
    /// Nothing of it was removed: it holds what the snapshot counted.
    Counted,
    /// A deletion stopped part way and what it left was measured: these bytes, in place of the
    /// snapshot's.
    Measured(u64),
    /// A deletion stopped part way and what it left cannot be measured: from here on the total
    /// is unknown.
    Unmeasured,
}

impl From<String> for Failed {
    fn from(reason: String) -> Self {
        Self {
            reason,
            freed: 0,
            remains: Remains::Counted,
        }
    }
}

/// One entry of the plan.
#[derive(Debug, Clone)]
pub(crate) struct Planned {
    pub(crate) removal: Removal,
    pub(crate) job: Job,
}

/// A finished Task its Store's collection rules take, as the Store's read-only plan names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskPick {
    pub(crate) state: PathBuf,
    pub(crate) task_id: String,
    pub(crate) bytes: u64,
}

/// What one plan is made from: the snapshot and the rules.
pub(crate) struct Inputs<'a> {
    /// The snapshot's entries; a Task the run executed is in use.
    pub(crate) entries: Vec<Entry>,
    /// The removals dead processes left claimed, as the snapshot found them (kind `claim`):
    /// counted in the total before, and finished first.
    pub(crate) claims: Vec<Entry>,
    /// The snapshot's Project Stores, in inventory order.
    pub(crate) projects: &'a [(PathBuf, collect::Look)],
    /// The finished Tasks collection takes, in order.
    pub(crate) tasks: Vec<TaskPick>,
    /// The registrations whose path holds no Store, as the snapshot found them: dropped by the
    /// plan, after the inventory reached through each its parent and any claim there.
    pub(crate) registrations: Vec<PathBuf>,
    pub(crate) collection: bool,
    pub(crate) max_bytes: u64,
    pub(crate) keep_campaigns: usize,
    pub(crate) keep_surveys: usize,
    pub(crate) idle_ms: u64,
    pub(crate) now: u64,
    pub(crate) telemetry: Telemetry,
}

/// The telemetry kind's part of a plan (ADR-0153 clause 8, design §6 "Budget").
#[derive(Debug, Clone, Default)]
pub(crate) struct Telemetry {
    /// Every telemetry directory's model by its canonical root, an overlapping one's `None`: a
    /// segment's step frees what it alone holds in it now, its blobs going with the last segment
    /// that names them, and is taken out of the model of the directory it was taken from only.
    pub(crate) models: BTreeMap<PathBuf, Option<budget::Directory>>,
    /// `[telemetry] max_bytes`; past it telemetry is evicted least recently used first, as past
    /// the whole budget.
    pub(crate) max_bytes: Option<u64>,
    /// `af telemetry gc`: nothing but telemetry is planned.
    pub(crate) only: bool,
}

/// One file or Survey the plan leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Left {
    pub(crate) kind: Kind,
    pub(crate) left: collect::Left,
}

/// The plan.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// The snapshot's total.
    pub(crate) total_before: u64,
    pub(crate) steps: Vec<Planned>,
    pub(crate) left: Vec<Left>,
    /// What the plan could not judge, with why; the next sweep tries again.
    pub(crate) failures: Vec<String>,
    /// The registrations it drops, each its own entry of the plan: `registry_dropped`.
    pub(crate) registrations: Vec<PathBuf>,
    /// The model's total after the last step.
    pub(crate) total_after: u64,
    pub(crate) stop: Stop,
    /// The telemetry kind's total after the last step, and why its own cap's step stopped.
    pub(crate) telemetry_total_after: u64,
    pub(crate) telemetry_stop: Stop,
}

/// Which entry of the model a step takes, by kind, path and Task.
type Key<'a> = (Kind, &'a Path, Option<&'a str>);

/// The snapshot as the plan's removals leave it.
struct Model {
    entries: Vec<Entry>,
    total: u64,
}

impl Model {
    fn new(entries: Vec<Entry>) -> Self {
        let total = entries
            .iter()
            .map(|entry| entry.bytes)
            .fold(0, u64::saturating_add);
        Self { entries, total }
    }

    /// Take a step's bytes out of the model: the entry `key` names, gone whole; otherwise (a
    /// record under `cas/removing`, a Task of a Store not told apart) `bytes` out of the entry
    /// `within` names, the one that counts them.
    fn take(&mut self, key: Option<Key<'_>>, within: Option<(Kind, &Path)>, bytes: u64) {
        if let Some((kind, path, task_id)) = key
            && let Some(index) = self.entries.iter().position(|entry| {
                entry.kind == kind && entry.path == path && entry.task_id.as_deref() == task_id
            })
        {
            let entry = self.entries.remove(index);
            self.total = self.total.saturating_sub(entry.bytes);
            return;
        }
        if let Some((kind, path)) = within
            && let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| entry.kind == kind && entry.path == path && entry.task_id.is_none())
        {
            let taken = bytes.min(entry.bytes);
            entry.bytes -= taken;
            self.total = self.total.saturating_sub(taken);
        }
    }

    /// Take a telemetry segment out of the model: its entry gone, and what its eviction frees
    /// now out of the total, which its entry's bytes need not be once another segment sharing
    /// its blobs went first.
    fn take_telemetry(&mut self, path: &Path, freed: u64) {
        self.entries
            .retain(|entry| !(entry.kind == Kind::Telemetry && entry.path == path));
        self.total = self.total.saturating_sub(freed);
    }

    /// The bytes and the identity of the Survey entry at `object`, as the snapshot measured
    /// them; nothing when the snapshot holds none.
    fn survey(&self, object: &Path) -> (u64, Option<review_sandbox::Identity>) {
        self.entries
            .iter()
            .find(|entry| entry.kind == Kind::Survey && entry.path == object)
            .map_or((0, None), |entry| (entry.bytes, entry.identity))
    }
}

impl Plan {
    fn push(
        &mut self,
        model: &mut Model,
        mut removal: Removal,
        job: Job,
        key: Option<Key<'_>>,
        within: Option<(Kind, &Path)>,
    ) {
        model.take(key, within, removal.bytes);
        removal.total_after = Some(model.total);
        self.steps.push(Planned { removal, job });
    }

    /// Plan the eviction of the telemetry entry `entry` by `rule`: what it frees now, from its
    /// directory's model, which it leaves. `None` when it cannot be planned, a failure said.
    fn push_telemetry(
        &mut self,
        model: &mut Model,
        telemetry: &mut Telemetry,
        entry: &Entry,
        rule: Rule,
    ) -> Option<u64> {
        match super::evict_telemetry(&mut telemetry.models, entry, false) {
            Ok(Evicted::Freed(freed)) => {
                model.take_telemetry(&entry.path, freed);
                let mut removal = super::removal(entry, freed, rule);
                removal.total_after = Some(model.total);
                self.steps.push(Planned {
                    removal,
                    job: Job::Telemetry(Box::new(entry.clone())),
                });
                Some(freed)
            }
            // A preview checks nothing on disk, so it finds nothing changed.
            Ok(Evicted::Changed(_)) => None,
            Err(error) => {
                self.failures.push(error);
                None
            }
        }
    }

    fn held(&mut self, object: PathBuf, bytes: u64, reason: &str) {
        self.left.push(Left {
            kind: Kind::Survey,
            left: collect::Left {
                path: object,
                bytes,
                reason: LeftReason::Held,
                detail: format!("{reason}; the next pass tries again"),
            },
        });
    }
}

fn removal(kind: Kind, path: PathBuf, task_id: Option<String>, bytes: u64, rule: Rule) -> Removal {
    Removal {
        kind,
        path,
        task_id,
        segment_id: None,
        directory: None,
        bytes,
        rule,
        total_after: None,
        unreadable: None,
        outcome: None,
    }
}

/// What judges a Survey the plan picks: its Project Store, what the snapshot verified of it and
/// the identity the snapshot measured for its record.
pub(crate) type Judge<'a> =
    dyn FnMut(&Path, &collect::Live, Option<review_sandbox::Identity>) -> Judged + 'a;

/// The entry of the snapshot that measured a claim's bytes as part of its own, when one did: the
/// deepest directory entry (a Project Store, chiefly) whose path holds the claim. Its bytes are
/// taken out of that entry, never counted again as a claim's.
pub(crate) fn holder<'a>(entries: &'a [Entry], claim: &Path) -> Option<&'a Entry> {
    entries
        .iter()
        .filter(|entry| {
            entry.task_id.is_none()
                && !matches!(entry.kind, Kind::Survey | Kind::Claim | Kind::Task)
                && claim != entry.path
                && claim.starts_with(&entry.path)
        })
        .max_by_key(|entry| entry.path.components().count())
}

/// The plan's entry for a directory the snapshot found nested deeper than the removal walk.
fn too_deep(entry: &Entry) -> Left {
    Left {
        kind: entry.kind,
        left: collect::Left {
            path: entry.path.clone(),
            bytes: entry.bytes,
            reason: LeftReason::TooDeep,
            detail: format!(
                "deeper than the removal walk: a directory below it is nested more than {} \
                 levels deep, so no removal could take it whole and it is left for the operator",
                review_sandbox::REMOVAL_MAX_DEPTH
            ),
        },
    }
}

/// The plan's entry for a claim the snapshot's walk could not measure: it holds no bytes the
/// plan could account for, so it is left, never planned.
fn unmeasurable(entry: &Entry, why: &str) -> Left {
    Left {
        kind: entry.kind,
        left: collect::Left {
            path: entry.path.clone(),
            bytes: entry.bytes,
            reason: LeftReason::Unmeasurable,
            detail: format!(
                "unmeasurable: {why}; what its removal would free cannot be said, so it is left \
                 for the operator"
            ),
        },
    }
}

/// Plan one sweep over one snapshot: first what interruptions left (`recovery`), the claims of
/// dead processes and then the deletions in Project Stores; then, when `collection`, the
/// finished Tasks the Stores' rules take, the campaigns beyond `keep_campaigns` and the
/// unreadable Stores idle at least `idle_ms` (`collection`), the Surveys of each Project
/// beyond `keep_surveys` (`keep_surveys`) and the telemetry segments idle at least `idle_ms`
/// (`collection`); then, while the model holds more than `max_bytes`, the least recently used
/// entry not in use and not used within the hour, a corrupt telemetry segment first among equals
/// (`budget`), and telemetry past its own `[telemetry] max_bytes` under the same rules. A
/// telemetry segment frees what it alone holds in its directory's model now, so a blob two
/// segments share goes with the second; `af telemetry gc` plans telemetry alone. `judge`
/// reads a Survey the plan picks against the identity the snapshot measured; nothing else is
/// read. The Surveys of a Store whose lock is held are left `held`, a record over the ceiling
/// `oversize`, and what a Store's look found foreign or unreadably named is left as it was found;
/// an entry or a claim nested deeper than the removal walk is left `too_deep`, and a claim the
/// snapshot could not measure `unmeasurable`. The registrations
/// the snapshot found holding no Store are the plan's to drop.
pub(crate) fn make(mut inputs: Inputs<'_>, judge: &mut Judge<'_>) -> Plan {
    let claims = std::mem::take(&mut inputs.claims);
    let mut entries = std::mem::take(&mut inputs.entries);
    let mut telemetry = std::mem::take(&mut inputs.telemetry);
    // A claim inside an entry measured whole is in that entry's bytes already: counted once.
    let held_by: Vec<Option<(Kind, PathBuf)>> = claims
        .iter()
        .map(|claim| holder(&entries, &claim.path).map(|entry| (entry.kind, entry.path.clone())))
        .collect();
    entries.extend(
        claims
            .iter()
            .zip(&held_by)
            .filter(|(_, held)| held.is_none())
            .map(|(claim, _)| claim.clone()),
    );
    let mut model = Model::new(entries);
    let mut plan = Plan {
        total_before: model.total,
        steps: Vec::new(),
        left: Vec::new(),
        failures: Vec::new(),
        registrations: std::mem::take(&mut inputs.registrations),
        total_after: 0,
        stop: Stop::Fits,
        telemetry_total_after: 0,
        telemetry_stop: Stop::Fits,
    };
    plan.left.extend(
        model
            .entries
            .iter()
            .filter(|entry| entry.too_deep && entry.kind != Kind::Claim)
            .map(too_deep),
    );
    for (claim, held) in claims.iter().zip(&held_by) {
        if claim.too_deep {
            plan.left.push(too_deep(claim));
            continue;
        }
        if let Some(why) = &claim.unmeasurable {
            plan.left.push(unmeasurable(claim, why));
            continue;
        }
        let step = removal(
            Kind::Claim,
            claim.path.clone(),
            None,
            claim.bytes,
            Rule::Recovery,
        );
        let (key, within) = match held {
            Some((kind, path)) => (None, Some((*kind, path.as_path()))),
            None => (Some((Kind::Claim, claim.path.as_path(), None)), None),
        };
        plan.push(
            &mut model,
            step,
            Job::Evict(Box::new(claim.clone())),
            key,
            within,
        );
    }
    for (store, look) in inputs.projects {
        plan.failures.extend(
            look.refused
                .iter()
                .map(|(path, why)| format!("finishing {}: {why}", path.display())),
        );
        plan.left.extend(look.left.iter().cloned().map(|left| Left {
            kind: Kind::Survey,
            left,
        }));
        for pending in &look.recovery {
            // A container a dead pass left empty is a claim; everything else a Survey's record.
            let kind = match pending.job {
                collect::Job::Container { .. } => Kind::Claim,
                _ => Kind::Survey,
            };
            if let Some(reason) = &look.held {
                // An empty container of a held Store holds no record to leave `held`; the next
                // pass removes it.
                if kind == Kind::Survey {
                    plan.held(pending.object.clone(), pending.bytes, reason);
                }
                continue;
            }
            let (key, within) = match pending.counted {
                collect::Counted::Survey => {
                    (Some((Kind::Survey, pending.object.as_path(), None)), None)
                }
                collect::Counted::Store => (None, Some((Kind::ProjectStore, store.as_path()))),
            };
            plan.push(
                &mut model,
                removal(
                    kind,
                    pending.object.clone(),
                    None,
                    pending.bytes,
                    Rule::Recovery,
                ),
                Job::Survey(pending.job.clone()),
                key,
                within,
            );
        }
    }
    if inputs.collection {
        // `af telemetry gc` collects nothing but telemetry.
        if !telemetry.only {
            collect_into(&mut plan, &mut model, &inputs, judge);
        }
        collect_telemetry(&mut plan, &mut model, &mut telemetry, &inputs);
    }
    if model.total > inputs.max_bytes {
        let mut candidates: Vec<Entry> = model
            .entries
            .iter()
            .filter(|entry| {
                !matches!(
                    entry.kind,
                    Kind::TaskStore | Kind::ProjectStore | Kind::Claim
                ) && (!telemetry.only || entry.kind == Kind::Telemetry)
                    && entry.in_use.is_none()
                    && entry.last_use_unix_ms.saturating_add(HOUR_MS) <= inputs.now
            })
            .cloned()
            .collect();
        candidates.sort_by(super::least_recently_used);
        for entry in candidates {
            if model.total <= inputs.max_bytes {
                break;
            }
            if entry.kind == Kind::Telemetry {
                plan.push_telemetry(&mut model, &mut telemetry, &entry, Rule::Budget);
                continue;
            }
            let key = Some((entry.kind, entry.path.as_path(), entry.task_id.as_deref()));
            if entry.kind != Kind::Survey {
                let step = removal(
                    entry.kind,
                    entry.path.clone(),
                    entry.task_id.clone(),
                    entry.bytes,
                    Rule::Budget,
                );
                plan.push(
                    &mut model,
                    step,
                    Job::Evict(Box::new(entry.clone())),
                    key,
                    None,
                );
                continue;
            }
            let Some((store, survey_id)) = collect::survey_of_object(&entry.path) else {
                plan.failures.push(format!(
                    "removing {}: names no Survey of a Project Store",
                    entry.path.display()
                ));
                continue;
            };
            // What the snapshot verified of it: the plan reads nothing of its event again.
            let Some(live) = inputs
                .projects
                .iter()
                .filter(|(path, _)| *path == store)
                .flat_map(|(_, look)| &look.live)
                .find(|live| live.survey_id == survey_id)
            else {
                plan.failures.push(format!(
                    "removing {}: Survey {survey_id} is no uncollected Survey of its Project \
                     Store's look",
                    entry.path.display()
                ));
                continue;
            };
            match judge(&store, live, entry.identity) {
                Judged::Take {
                    unreadable,
                    identity,
                } => {
                    let mut step = removal(
                        Kind::Survey,
                        entry.path.clone(),
                        None,
                        entry.bytes,
                        Rule::Budget,
                    );
                    step.unreadable = unreadable;
                    let job = collect::Job::Take {
                        store,
                        survey_id,
                        identity,
                    };
                    plan.push(&mut model, step, Job::Survey(job), key, None);
                }
                Judged::Left(reason, detail) => plan.left.push(Left {
                    kind: Kind::Survey,
                    left: collect::Left {
                        path: entry.path.clone(),
                        bytes: entry.bytes,
                        reason,
                        detail: format!("Survey {survey_id} is {detail}"),
                    },
                }),
                Judged::Refused(why) => plan.failures.push(format!(
                    "removing {}: Survey {survey_id} is {why}",
                    entry.path.display()
                )),
            }
        }
    }
    // Telemetry past its own cap, under the same rules.
    let mut held = super::models_total(&telemetry.models, &model.entries);
    if let Some(cap) = telemetry.max_bytes {
        let mut more: Vec<Entry> = model
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == Kind::Telemetry
                    && entry.in_use.is_none()
                    && entry.last_use_unix_ms.saturating_add(HOUR_MS) <= inputs.now
            })
            .cloned()
            .collect();
        more.sort_by(super::least_recently_used);
        let mut more = more.into_iter();
        while held > cap {
            let Some(entry) = more.next() else {
                plan.telemetry_stop = Stop::NothingEvictable;
                break;
            };
            if let Some(freed) =
                plan.push_telemetry(&mut model, &mut telemetry, &entry, Rule::Budget)
            {
                held = held.saturating_sub(freed);
            }
        }
    }
    plan.telemetry_total_after = held;
    plan.total_after = model.total;
    plan.stop = stop(model.total, inputs.max_bytes);
    plan
}

fn stop(total: u64, max_bytes: u64) -> Stop {
    if total > max_bytes {
        Stop::NothingEvictable
    } else {
        Stop::Fits
    }
}

/// The collection part of [`make`].
fn collect_into(plan: &mut Plan, model: &mut Model, inputs: &Inputs<'_>, judge: &mut Judge<'_>) {
    let idle = |entry: &Entry| {
        entry.in_use.is_none()
            && entry.last_use_unix_ms.saturating_add(inputs.idle_ms) <= inputs.now
    };
    for pick in &inputs.tasks {
        let step = removal(
            Kind::Task,
            pick.state.clone(),
            Some(pick.task_id.clone()),
            pick.bytes,
            Rule::Collection,
        );
        plan.push(
            model,
            step,
            Job::Task,
            Some((
                Kind::Task,
                pick.state.as_path(),
                Some(pick.task_id.as_str()),
            )),
            Some((Kind::TaskStore, pick.state.as_path())),
        );
    }
    let mut campaigns: Vec<Entry> = model
        .entries
        .iter()
        .filter(|entry| entry.kind == Kind::Campaign)
        .cloned()
        .collect();
    campaigns.sort_by(|a, b| b.last_use_unix_ms.cmp(&a.last_use_unix_ms));
    let unreadable: Vec<Entry> = model
        .entries
        .iter()
        .filter(|entry| entry.kind == Kind::UnreadableStore)
        .cloned()
        .collect();
    for entry in campaigns
        .into_iter()
        .skip(inputs.keep_campaigns)
        .chain(unreadable)
        .filter(idle)
    {
        let step = removal(
            entry.kind,
            entry.path.clone(),
            None,
            entry.bytes,
            Rule::Collection,
        );
        let key = Some((entry.kind, entry.path.as_path(), None));
        plan.push(model, step, Job::Evict(Box::new(entry.clone())), key, None);
    }
    for (store, look) in inputs.projects {
        let chosen =
            collect::choose_beyond(&look.live, inputs.keep_surveys, inputs.idle_ms, inputs.now);
        for live in chosen {
            let object = collect::object_path(store, &live.survey_id);
            let (bytes, identity) = model.survey(&object);
            if let Some(reason) = &look.held {
                plan.held(object, bytes, reason);
                continue;
            }
            match judge(store, live, identity) {
                Judged::Take {
                    unreadable,
                    identity,
                } => {
                    let mut step =
                        removal(Kind::Survey, object.clone(), None, bytes, Rule::KeepSurveys);
                    step.unreadable = unreadable;
                    let job = collect::Job::Take {
                        store: store.clone(),
                        survey_id: live.survey_id.clone(),
                        identity,
                    };
                    let key = Some((Kind::Survey, object.as_path(), None));
                    plan.push(model, step, Job::Survey(job), key, None);
                }
                Judged::Left(reason, detail) => plan.left.push(Left {
                    kind: Kind::Survey,
                    left: collect::Left {
                        path: object,
                        bytes,
                        reason,
                        detail: format!("Survey {} is {detail}", live.survey_id),
                    },
                }),
                Judged::Refused(why) => plan.failures.push(format!(
                    "collecting {}: Survey {} is {why}",
                    object.display(),
                    live.survey_id
                )),
            }
        }
    }
}

/// The telemetry part of collection (ADR-0153 clause 8): every segment (and other version's
/// directory) idle at least `idle_ms`, never one in use or used within the hour, least recently
/// used first.
fn collect_telemetry(
    plan: &mut Plan,
    model: &mut Model,
    telemetry: &mut Telemetry,
    inputs: &Inputs<'_>,
) {
    let mut idle: Vec<Entry> = model
        .entries
        .iter()
        .filter(|entry| {
            entry.kind == Kind::Telemetry
                && entry.in_use.is_none()
                && entry.last_use_unix_ms.saturating_add(HOUR_MS) <= inputs.now
                && entry.last_use_unix_ms.saturating_add(inputs.idle_ms) <= inputs.now
        })
        .cloned()
        .collect();
    idle.sort_by(super::least_recently_used);
    for entry in idle {
        plan.push_telemetry(model, telemetry, &entry, Rule::Collection);
    }
}

/// Run `plan` entry by entry with `execute`, never planning again, into `report`, whose
/// `total_before` already holds the snapshot's total. Each entry reports what it actually freed
/// and the total after it; one that failed, of any kind, is a failure and stays in its place
/// with its outcome `failed: <reason>` and what it actually freed: nothing, the running total
/// unchanged by it, unless a deletion stopped part way, whose freed bytes are what left the
/// disk. An entry that grew or shrank since the snapshot is reconciled before what left is taken
/// out: a removal that measured what its entry held when it started puts those bytes in place
/// of the snapshot's (once, in whichever entry counted it), so a whole removal takes exactly the
/// entry's snapshot bytes out of the total and one that stopped part way leaves exactly what it
/// measured left; one that stopped part way and could not measure what it left makes the total
/// `unknown` from there on, and `fits` is never claimed of it. So the document's totals are what
/// the directory holds. The stop is what the total after the last entry says. The plan's
/// registrations are the caller's to drop.
pub(crate) fn run(
    plan: Plan,
    report: &mut SweepReport,
    execute: &mut dyn FnMut(&Planned) -> Outcome,
) {
    let mut total = Some(plan.total_before);
    report
        .left
        .extend(plan.left.into_iter().map(super::Left::of));
    report.failures.extend(plan.failures);
    for planned in plan.steps {
        let mut removal = planned.removal.clone();
        // What the snapshot counted for the entry.
        let counted = planned.removal.bytes;
        match execute(&planned) {
            // Found changed when checked again: left in place whole, never removed.
            Ok(Ran {
                changed: Some(changed),
                ..
            }) => report.telemetry.changed.push(*changed),
            Ok(ran) => {
                total = total.map(|total| match ran.started {
                    Some(started) => total
                        .saturating_add(started)
                        .saturating_sub(counted)
                        .saturating_sub(ran.freed),
                    None => total.saturating_sub(ran.freed),
                });
                removal.bytes = ran.freed;
                removal.unreadable = ran.unreadable;
                removal.total_after = total;
                report.removals.push(removal);
            }
            Err(Failed {
                reason,
                freed,
                remains,
            }) => {
                total = match remains {
                    Remains::Counted => total,
                    Remains::Measured(left) => {
                        total.map(|total| total.saturating_add(left).saturating_sub(counted))
                    }
                    Remains::Unmeasured => None,
                };
                removal.bytes = freed;
                removal.unreadable = None;
                removal.total_after = total;
                removal.outcome = Some(format!("{FAILED}{reason}"));
                report.failures.push(reason);
                report.removals.push(removal);
            }
        }
    }
    report.total_after = total;
    report.stop = total.map_or(Stop::Unknown, |total| stop(total, report.max_bytes));
}

/// The plan as a preview reports it: every entry as planned, the registrations it drops included.
pub(crate) fn preview(plan: Plan, report: &mut SweepReport) {
    report.registry_dropped = plan.registrations.clone();
    report
        .left
        .extend(plan.left.into_iter().map(super::Left::of));
    report.failures.extend(plan.failures);
    report
        .removals
        .extend(plan.steps.into_iter().map(|planned| planned.removal));
    report.total_after = Some(plan.total_after);
    report.stop = plan.stop;
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;
    const DAY: u64 = 86_400_000;

    fn entry(kind: Kind, path: &str, bytes: u64, days_idle: u64) -> Entry {
        Entry {
            kind,
            path: PathBuf::from(path),
            task_id: None,
            bytes,
            last_use_unix_ms: NOW - days_idle * DAY,
            in_use: None,
            reason: None,
            directory: None,
            segment_id: None,
            state: None,
            identity: None,
            found: None,
            too_deep: false,
            unmeasurable: None,
        }
    }

    fn report(max_bytes: u64, applied: bool) -> SweepReport {
        SweepReport {
            applied,
            collection: true,
            max_bytes,
            total_before: 0,
            total_after: Some(0),
            stop: Stop::Fits,
            removals: Vec::new(),
            left: Vec::new(),
            gate_cleanups: Vec::new(),
            registry_dropped: Vec::new(),
            failures: Vec::new(),
            telemetry: super::super::TelemetrySweep {
                max_bytes: None,
                total_before: 0,
                total_after: 0,
                stop: Stop::Fits,
                reclaimed: Vec::new(),
                notes: Vec::new(),
                registry_dropped: Vec::new(),
                changed: Vec::new(),
                problems: Vec::new(),
            },
        }
    }

    const STORE: &str = "/state/af/project/aa";

    fn survey_id(n: u8) -> String {
        format!("sha256:{}", format!("{n:02x}").repeat(32))
    }

    fn identity(inode: u64) -> review_sandbox::Identity {
        review_sandbox::Identity {
            device: 1,
            inode,
            born: Some(1),
        }
    }

    /// The reviewer's numbers: a 10-byte record an interruption moved under `cas/removing`,
    /// counted in its Project Store's entry; 100 bytes once it is gone; a 20-byte least
    /// recently used entry; `max_bytes` 85. The plan deletes the record, then takes the 20-byte
    /// entry and stops at 80, and the applied sweep, running that plan, stops there too.
    #[test]
    fn preview_and_apply_stop_after_the_first_budget_candidate_at_80_bytes() {
        let moved = collect::Pending {
            object: collect::object_path(Path::new(STORE), &survey_id(1)),
            bytes: 10,
            counted: collect::Counted::Store,
            job: collect::Job::Moved {
                store: PathBuf::from(STORE),
                container: "0123".into(),
                survey_id: survey_id(1),
                identity: identity(7),
            },
        };
        let projects = vec![(
            PathBuf::from(STORE),
            collect::Look {
                recovery: vec![moved],
                ..collect::Look::default()
            },
        )];
        let mut store = entry(Kind::ProjectStore, STORE, 10, 1);
        store.in_use = Some("kept".into());
        let entries = vec![
            store,
            entry(Kind::WarmKey, "/cache/af/warm/a/1", 20, 3),
            entry(Kind::WarmKey, "/cache/af/warm/a/2", 30, 2),
            entry(Kind::WarmKey, "/cache/af/warm/a/3", 50, 1),
        ];
        let inputs = || Inputs {
            entries: entries.clone(),
            claims: Vec::new(),
            projects: &projects,
            tasks: Vec::new(),
            registrations: Vec::new(),
            collection: true,
            max_bytes: 85,
            keep_campaigns: 10,
            keep_surveys: 50,
            idle_ms: 14 * DAY,
            now: NOW,
            telemetry: Telemetry::default(),
        };
        let mut judge = |_: &Path,
                         _: &collect::Live,
                         _: Option<review_sandbox::Identity>|
         -> Judged { unreachable!("no Survey is picked") };
        let plan = make(inputs(), &mut judge);
        assert_eq!(plan.total_before, 110);
        let steps: Vec<(Rule, u64, Option<u64>)> = plan
            .steps
            .iter()
            .map(|planned| {
                (
                    planned.removal.rule,
                    planned.removal.bytes,
                    planned.removal.total_after,
                )
            })
            .collect();
        assert_eq!(
            steps,
            [
                (Rule::Recovery, 10, Some(100)),
                (Rule::Budget, 20, Some(80))
            ]
        );
        assert_eq!(plan.total_after, 80);
        assert_eq!(plan.stop, Stop::Fits);

        let mut previewed = report(85, false);
        previewed.total_before = plan.total_before;
        preview(plan.clone(), &mut previewed);
        let mut applied = report(85, true);
        applied.total_before = plan.total_before;
        run(plan, &mut applied, &mut |planned: &Planned| {
            Ok(Ran::freed(planned.removal.bytes))
        });
        for document in [&previewed, &applied] {
            let removals: Vec<(Rule, u64, Option<u64>)> = document
                .removals
                .iter()
                .map(|removal| (removal.rule, removal.bytes, removal.total_after))
                .collect();
            assert_eq!(removals, steps);
            assert_eq!(document.total_after, Some(80));
            assert_eq!(document.stop, Stop::Fits);
        }
        let as_json = |document: &SweepReport| serde_json::to_value(&document.removals).unwrap();
        assert_eq!(as_json(&previewed), as_json(&applied), "entry for entry");
    }

    /// The applied sweep never chooses again: a Survey whose job finds its record replaced keeps
    /// its place as failed with nothing freed, and no other entry takes its place.
    #[test]
    fn a_failed_survey_is_kept_as_failed_and_nothing_is_planned_again() {
        let object = collect::object_path(Path::new(STORE), &survey_id(2));
        let mut store = entry(Kind::ProjectStore, STORE, 4, 1);
        store.in_use = Some("kept".into());
        let mut survey = entry(Kind::Survey, object.to_str().unwrap(), 40, 30);
        survey.identity = Some(identity(9));
        let entries = vec![
            store,
            survey,
            entry(Kind::WarmKey, "/cache/af/warm/a/1", 40, 20),
            entry(Kind::WarmKey, "/cache/af/warm/a/2", 40, 10),
        ];
        let projects = vec![(
            PathBuf::from(STORE),
            collect::Look {
                live: vec![collect::Live {
                    survey_id: survey_id(2),
                    cutoff_ms: NOW - 30 * DAY,
                    protected: false,
                    identity: Some(identity(9)),
                    verdict: collect::Verdict::Agrees,
                }],
                ..collect::Look::default()
            },
        )];
        let plan = make(
            Inputs {
                entries,
                claims: Vec::new(),
                projects: &projects,
                tasks: Vec::new(),
                registrations: Vec::new(),
                collection: false,
                max_bytes: 50,
                keep_campaigns: 10,
                keep_surveys: 50,
                idle_ms: 14 * DAY,
                now: NOW,
                telemetry: Telemetry::default(),
            },
            &mut |_, _, inventoried| {
                assert_eq!(inventoried, Some(identity(9)), "the snapshot's identity");
                Judged::Take {
                    unreadable: None,
                    identity: inventoried,
                }
            },
        );
        let planned: Vec<&Path> = plan
            .steps
            .iter()
            .map(|planned| planned.removal.path.as_path())
            .collect();
        assert_eq!(planned, [object.as_path(), Path::new("/cache/af/warm/a/1")]);
        let mut applied = report(50, true);
        applied.total_before = plan.total_before;
        run(
            plan,
            &mut applied,
            &mut |planned: &Planned| match planned.job {
                Job::Survey(_) => {
                    Err(String::from("Survey x is refused (replaced during collection)").into())
                }
                _ => Ok(Ran::freed(planned.removal.bytes)),
            },
        );
        assert_eq!(applied.removals.len(), 2, "{:?}", applied.removals);
        assert_eq!(applied.removals[0].bytes, 0);
        assert_eq!(
            applied.removals[0].outcome.as_deref(),
            Some("failed: Survey x is refused (replaced during collection)")
        );
        assert_eq!(
            applied.removals[0].total_after,
            Some(124),
            "unchanged by it"
        );
        assert_eq!(applied.removals[1].total_after, Some(84));
        assert_eq!(applied.total_after, Some(84));
        assert_eq!(
            applied.stop,
            Stop::NothingEvictable,
            "the next sweep tries again"
        );
    }

    /// A Project Store whose lock is held: its pending deletions and its Surveys beyond
    /// `keep_surveys` are left `held`, and its foreign files are listed whatever the lock.
    #[test]
    fn a_held_store_leaves_its_surveys_held_and_lists_its_foreign_files() {
        let old = NOW - 30 * DAY;
        let live: Vec<collect::Live> = (1..=3)
            .map(|n| collect::Live {
                survey_id: survey_id(n),
                cutoff_ms: old + u64::from(n),
                protected: n == 3,
                identity: None,
                verdict: collect::Verdict::Agrees,
            })
            .collect();
        let foreign = collect::Left {
            path: PathBuf::from(STORE).join("cas/objects/04/x"),
            bytes: 4,
            reason: LeftReason::Foreign,
            detail: "a file took the CAS name".into(),
        };
        let pending = collect::Pending {
            object: collect::object_path(Path::new(STORE), &survey_id(4)),
            bytes: 8,
            counted: collect::Counted::Store,
            job: collect::Job::Moved {
                store: PathBuf::from(STORE),
                container: "dead".into(),
                survey_id: survey_id(4),
                identity: identity(4),
            },
        };
        let projects = vec![(
            PathBuf::from(STORE),
            collect::Look {
                held: Some(collect::HELD.into()),
                live,
                recovery: vec![pending],
                left: vec![foreign.clone()],
                ..collect::Look::default()
            },
        )];
        let plan = make(
            Inputs {
                entries: Vec::new(),
                claims: Vec::new(),
                projects: &projects,
                tasks: Vec::new(),
                registrations: Vec::new(),
                collection: true,
                max_bytes: u64::MAX,
                keep_campaigns: 10,
                keep_surveys: 0,
                idle_ms: 0,
                now: NOW,
                telemetry: Telemetry::default(),
            },
            &mut |_, _, _| unreachable!("a held Store's Surveys are not read"),
        );
        assert!(plan.steps.is_empty());
        let reasons: Vec<LeftReason> = plan.left.iter().map(|left| left.left.reason).collect();
        assert_eq!(
            reasons,
            [
                LeftReason::Foreign,
                LeftReason::Held,
                LeftReason::Held,
                LeftReason::Held
            ]
        );
        assert_eq!(plan.left[0].left, foreign);
        assert!(plan.left[1].left.detail.starts_with(collect::HELD));
    }

    /// What an interruption left is in the snapshot and planned first: the claim of a dead
    /// process with the bytes the listing measured, then an empty container a dead pass left,
    /// counted in its Project Store's entry. The preview and the applied sweep list the same
    /// recovery entries, entry for entry, and the claim is never a budget candidate.
    #[test]
    fn claims_and_empty_containers_are_planned_first_and_listed_alike() {
        let claim = Entry {
            identity: Some(identity(3)),
            ..entry(
                Kind::Claim,
                "/state/af/review/campaigns/.af-removing-9-1-0",
                64,
                0,
            )
        };
        let container = PathBuf::from(STORE).join("cas/removing/0123456789abcdef0123456789abcdef");
        let projects = vec![(
            PathBuf::from(STORE),
            collect::Look {
                recovery: vec![collect::Pending {
                    object: container.clone(),
                    bytes: 4,
                    counted: collect::Counted::Store,
                    job: collect::Job::Container {
                        store: PathBuf::from(STORE),
                        container: "0123456789abcdef0123456789abcdef".into(),
                        identity: identity(5),
                    },
                }],
                ..collect::Look::default()
            },
        )];
        let mut store = entry(Kind::ProjectStore, STORE, 10, 1);
        store.in_use = Some("kept".into());
        let inputs = || Inputs {
            entries: vec![
                store.clone(),
                entry(Kind::WarmKey, "/cache/af/warm/a/1", 20, 3),
            ],
            claims: vec![claim.clone()],
            projects: &projects,
            tasks: Vec::new(),
            registrations: Vec::new(),
            collection: true,
            max_bytes: 0,
            keep_campaigns: 10,
            keep_surveys: 50,
            idle_ms: 14 * DAY,
            now: NOW,
            telemetry: Telemetry::default(),
        };
        let plan = make(inputs(), &mut |_, _, _| unreachable!("no Survey is picked"));
        assert_eq!(plan.total_before, 94, "the claim is counted");
        let steps: Vec<(Kind, Rule, u64, Option<u64>)> = plan
            .steps
            .iter()
            .map(|planned| {
                let removal = &planned.removal;
                (
                    removal.kind,
                    removal.rule,
                    removal.bytes,
                    removal.total_after,
                )
            })
            .collect();
        assert_eq!(
            steps,
            [
                (Kind::Claim, Rule::Recovery, 64, Some(30)),
                (Kind::Claim, Rule::Recovery, 4, Some(26)),
                (Kind::WarmKey, Rule::Budget, 20, Some(6)),
            ]
        );
        assert_eq!(plan.steps[1].removal.path, container);
        assert!(
            matches!(&plan.steps[0].job, Job::Evict(entry) if entry.identity == Some(identity(3)))
        );
        let mut previewed = report(0, false);
        previewed.total_before = plan.total_before;
        preview(plan.clone(), &mut previewed);
        let mut applied = report(0, true);
        applied.total_before = plan.total_before;
        run(plan, &mut applied, &mut |planned: &Planned| {
            Ok(Ran::freed(planned.removal.bytes))
        });
        let as_json = |document: &SweepReport| serde_json::to_value(&document.removals).unwrap();
        assert_eq!(as_json(&previewed), as_json(&applied), "entry for entry");
    }

    /// Every kind of planned entry whose execution fails stays in the applied document as
    /// `failed: <reason>` with nothing freed and the running total unchanged by it; every
    /// failure is listed too, and the totals are what is left.
    #[test]
    fn a_failed_entry_of_every_kind_stays_as_failed_with_the_total_unchanged() {
        let kinds = [
            Kind::WarmKey,
            Kind::Workspace,
            Kind::Campaign,
            Kind::Task,
            Kind::UnreadableStore,
            Kind::Version,
            Kind::Survey,
            Kind::Claim,
        ];
        let steps: Vec<Planned> = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| Planned {
                removal: removal(
                    *kind,
                    PathBuf::from(format!("/af/{index}")),
                    None,
                    10,
                    Rule::Budget,
                ),
                job: Job::Task,
            })
            .collect();
        let plan = Plan {
            total_before: 200,
            steps,
            left: Vec::new(),
            failures: Vec::new(),
            registrations: Vec::new(),
            total_after: 120,
            stop: Stop::Fits,
            telemetry_total_after: 0,
            telemetry_stop: Stop::Fits,
        };
        let mut applied = report(150, true);
        applied.total_before = plan.total_before;
        // Every other entry fails, whatever its kind.
        run(plan, &mut applied, &mut |planned: &Planned| {
            let index: usize = planned
                .removal
                .path
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            if index % 2 == 0 {
                Err(format!("removing /af/{index}: it changed since it was measured").into())
            } else {
                Ok(Ran::freed(10))
            }
        });
        assert_eq!(
            applied.removals.len(),
            kinds.len(),
            "every planned entry stays"
        );
        let mut total = 200;
        for (index, removal) in applied.removals.iter().enumerate() {
            assert_eq!(removal.kind, kinds[index]);
            if index % 2 == 0 {
                assert_eq!(removal.bytes, 0);
                assert_eq!(
                    removal.outcome.as_deref(),
                    Some(
                        format!("failed: removing /af/{index}: it changed since it was measured")
                            .as_str()
                    )
                );
            } else {
                total -= 10;
                assert_eq!(removal.outcome, None);
            }
            assert_eq!(removal.total_after, Some(total), "{removal:?}");
        }
        assert_eq!(applied.total_after, Some(160));
        assert_eq!(applied.failures.len(), 4);
        assert_eq!(applied.stop, Stop::NothingEvictable);
    }

    /// A claim inside a Project Store (a Workspace's `state` a dead process renamed into its
    /// claim) is in that Store's measured bytes already: the plan counts it once, takes its
    /// bytes out of the Store's entry when it finishes it, and never adds it as an entry of its
    /// own. A claim outside every entry is counted on its own.
    #[test]
    fn a_claim_inside_a_project_store_is_counted_once_in_its_bytes() {
        let mut store = entry(Kind::ProjectStore, STORE, 100, 1);
        store.in_use = Some("kept".into());
        let inside = Entry {
            identity: Some(identity(4)),
            ..entry(
                Kind::Claim,
                &format!("{STORE}/workspaces/w/.af-removing-9-1-0"),
                40,
                0,
            )
        };
        let outside = Entry {
            identity: Some(identity(5)),
            ..entry(
                Kind::Claim,
                "/state/af/review/campaigns/.af-removing-9-2-0",
                8,
                0,
            )
        };
        let projects = vec![(PathBuf::from(STORE), collect::Look::default())];
        let plan = make(
            Inputs {
                entries: vec![store],
                claims: vec![inside.clone(), outside],
                projects: &projects,
                tasks: Vec::new(),
                registrations: vec![PathBuf::from(format!("{STORE}/workspaces/w/state"))],
                collection: false,
                max_bytes: u64::MAX,
                keep_campaigns: 10,
                keep_surveys: 50,
                idle_ms: 14 * DAY,
                now: NOW,
                telemetry: Telemetry::default(),
            },
            &mut |_, _, _| unreachable!("no Survey is picked"),
        );
        assert_eq!(
            plan.total_before, 108,
            "the Store's 100 and the outside claim's 8"
        );
        let steps: Vec<(&Path, u64, Option<u64>)> = plan
            .steps
            .iter()
            .map(|planned| {
                (
                    planned.removal.path.as_path(),
                    planned.removal.bytes,
                    planned.removal.total_after,
                )
            })
            .collect();
        assert_eq!(
            steps,
            [
                (inside.path.as_path(), 40, Some(68)),
                (
                    Path::new("/state/af/review/campaigns/.af-removing-9-2-0"),
                    8,
                    Some(60)
                ),
            ]
        );
        assert_eq!(plan.total_after, 60, "the Store's bytes less the claim's");
        assert_eq!(
            plan.registrations,
            [PathBuf::from(format!("{STORE}/workspaces/w/state"))],
            "the registration's drop is the plan's own entry"
        );
        let mut previewed = report(u64::MAX, false);
        preview(plan, &mut previewed);
        assert_eq!(
            previewed.registry_dropped,
            [PathBuf::from(format!("{STORE}/workspaces/w/state"))]
        );
    }

    /// An entry or a claim nested deeper than the removal walk is left `too_deep`, by every plan
    /// and whatever the budget wants, and counted in the total it holds.
    #[test]
    fn an_entry_deeper_than_the_removal_walk_is_left_too_deep() {
        let mut deep = entry(Kind::Campaign, "/state/af/review/campaigns/c-1", 30, 30);
        deep.too_deep = true;
        deep.in_use = Some("left: deeper than the removal walk".into());
        let claim = Entry {
            too_deep: true,
            identity: Some(identity(6)),
            ..entry(
                Kind::Claim,
                "/state/af/review/campaigns/.af-removing-9-3-0",
                20,
                0,
            )
        };
        let plan = make(
            Inputs {
                entries: vec![deep, entry(Kind::WarmKey, "/cache/af/warm/a/1", 10, 3)],
                claims: vec![claim],
                projects: &[],
                tasks: Vec::new(),
                registrations: Vec::new(),
                collection: true,
                max_bytes: 0,
                keep_campaigns: 0,
                keep_surveys: 50,
                idle_ms: 0,
                now: NOW,
                telemetry: Telemetry::default(),
            },
            &mut |_, _, _| unreachable!("no Survey is picked"),
        );
        assert_eq!(plan.total_before, 60);
        let taken: Vec<&Path> = plan
            .steps
            .iter()
            .map(|planned| planned.removal.path.as_path())
            .collect();
        assert_eq!(taken, [Path::new("/cache/af/warm/a/1")]);
        let left: Vec<(Kind, &Path, LeftReason)> = plan
            .left
            .iter()
            .map(|left| (left.kind, left.left.path.as_path(), left.left.reason))
            .collect();
        assert_eq!(
            left,
            [
                (
                    Kind::Campaign,
                    Path::new("/state/af/review/campaigns/c-1"),
                    LeftReason::TooDeep
                ),
                (
                    Kind::Claim,
                    Path::new("/state/af/review/campaigns/.af-removing-9-3-0"),
                    LeftReason::TooDeep
                ),
            ]
        );
        assert!(
            plan.left[0]
                .left
                .detail
                .starts_with("deeper than the removal walk")
        );
        assert_eq!(plan.total_after, 50);
        assert_eq!(plan.stop, Stop::NothingEvictable);
    }

    /// A claim the snapshot's walk could not measure holds no bytes the plan could account for:
    /// it is left `unmeasurable` with why, never planned, whatever the budget wants.
    #[test]
    fn an_unmeasurable_claim_is_left_and_never_planned() {
        let claim = Entry {
            unmeasurable: Some("permission denied".into()),
            identity: Some(identity(7)),
            ..entry(
                Kind::Claim,
                "/state/af/review/campaigns/.af-removing-9-4-0",
                0,
                0,
            )
        };
        let plan = make(
            Inputs {
                entries: vec![entry(Kind::WarmKey, "/cache/af/warm/a/1", 10, 3)],
                claims: vec![claim],
                projects: &[],
                tasks: Vec::new(),
                registrations: Vec::new(),
                collection: true,
                max_bytes: 0,
                keep_campaigns: 0,
                keep_surveys: 50,
                idle_ms: 0,
                now: NOW,
                telemetry: Telemetry::default(),
            },
            &mut |_, _, _| unreachable!("no Survey is picked"),
        );
        let taken: Vec<&Path> = plan
            .steps
            .iter()
            .map(|planned| planned.removal.path.as_path())
            .collect();
        assert_eq!(taken, [Path::new("/cache/af/warm/a/1")]);
        let [left] = plan.left.as_slice() else {
            panic!("{:?}", plan.left);
        };
        assert_eq!(left.kind, Kind::Claim);
        assert_eq!(left.left.reason, LeftReason::Unmeasurable);
        assert_eq!(left.left.bytes, 0);
        assert!(
            left.left
                .detail
                .starts_with("unmeasurable: permission denied"),
            "{}",
            left.left.detail
        );
    }

    /// A deletion that stopped part way is failed with what left the disk, which the entry keeps,
    /// and what it left takes the place of the snapshot's bytes in the running total.
    #[test]
    fn a_partial_deletion_is_failed_with_the_bytes_it_freed() {
        let plan = Plan {
            total_before: 100,
            steps: vec![Planned {
                removal: removal(
                    Kind::Campaign,
                    PathBuf::from("/af/c"),
                    None,
                    60,
                    Rule::Collection,
                ),
                job: Job::Task,
            }],
            left: Vec::new(),
            failures: Vec::new(),
            registrations: Vec::new(),
            total_after: 40,
            stop: Stop::Fits,
            telemetry_total_after: 0,
            telemetry_stop: Stop::Fits,
        };
        let mut applied = report(u64::MAX, true);
        applied.total_before = plan.total_before;
        run(plan, &mut applied, &mut |_: &Planned| {
            Err(Failed {
                reason: "removing /af/c: only partly removed".into(),
                freed: 25,
                remains: Remains::Measured(35),
            })
        });
        let [removal] = applied.removals.as_slice() else {
            panic!("{:?}", applied.removals);
        };
        assert_eq!(removal.bytes, 25);
        assert_eq!(removal.total_after, Some(75));
        assert_eq!(
            removal.outcome.as_deref(),
            Some("failed: removing /af/c: only partly removed")
        );
        assert_eq!(applied.total_after, Some(75));
    }

    const MIB: u64 = 1 << 20;

    /// One planned claim of 10 MiB beside 20 MiB of other entries.
    fn grown_plan() -> Plan {
        let claim = |path: &str| Planned {
            removal: removal(
                Kind::Claim,
                PathBuf::from(path),
                None,
                10 * MIB,
                Rule::Recovery,
            ),
            job: Job::Task,
        };
        Plan {
            total_before: 30 * MIB,
            steps: vec![
                claim("/af/.af-removing-9-1-0"),
                claim("/af/.af-removing-9-2-0"),
            ],
            left: Vec::new(),
            failures: Vec::new(),
            registrations: Vec::new(),
            total_after: 10 * MIB,
            stop: Stop::Fits,
            telemetry_total_after: 0,
            telemetry_stop: Stop::Fits,
        }
    }

    /// A claim the snapshot measured at 10 MiB that grew to 20 MiB before its removal: what its
    /// removal measured it to hold when it started takes the place of the snapshot's bytes
    /// before what left is taken out. Removed part way, 8 MiB freed, the total keeps the 12 MiB
    /// it still holds; removed whole, the total loses exactly the snapshot's 10 MiB, while the
    /// entry reports the 20 MiB that left the disk.
    #[test]
    fn a_grown_claims_removal_reconciles_the_total_before_what_left_is_taken_out() {
        let mut applied = report(u64::MAX, true);
        applied.total_before = 30 * MIB;
        run(grown_plan(), &mut applied, &mut |planned: &Planned| {
            if planned.removal.path.ends_with(".af-removing-9-1-0") {
                Err(Failed {
                    reason: "removing it: only partly removed".into(),
                    freed: 8 * MIB,
                    remains: Remains::Measured(12 * MIB),
                })
            } else {
                Ok(Ran {
                    freed: 20 * MIB,
                    unreadable: None,
                    started: Some(20 * MIB),
                    changed: None,
                })
            }
        });
        let [partly, whole] = applied.removals.as_slice() else {
            panic!("{:?}", applied.removals);
        };
        assert_eq!(partly.bytes, 8 * MIB);
        assert_eq!(
            partly.total_after,
            Some(32 * MIB),
            "the 20 MiB of the others and the 12 MiB the claim retains"
        );
        assert_eq!(whole.bytes, 20 * MIB, "what left the disk");
        assert_eq!(
            whole.total_after,
            Some(22 * MIB),
            "the snapshot's 10 MiB out"
        );
        assert_eq!(applied.total_after, Some(22 * MIB));
        assert_eq!(applied.stop, Stop::Fits);
    }

    /// A deletion that stopped part way and could not measure what it left makes the total
    /// unknown from there on: the entry, every later one and the document say `unknown`, and the
    /// stop claims neither `fits` nor `nothing_evictable`.
    #[test]
    fn a_partial_deletion_whose_remainder_cannot_be_measured_leaves_the_total_unknown() {
        let mut applied = report(u64::MAX, true);
        applied.total_before = 30 * MIB;
        run(grown_plan(), &mut applied, &mut |planned: &Planned| {
            if planned.removal.path.ends_with(".af-removing-9-1-0") {
                Err(Failed {
                    reason: "removing it: what is left cannot be measured".into(),
                    freed: 0,
                    remains: Remains::Unmeasured,
                })
            } else {
                Ok(Ran::freed(10 * MIB))
            }
        });
        assert!(
            applied
                .removals
                .iter()
                .all(|removal| removal.total_after.is_none()),
            "{:?}",
            applied.removals
        );
        assert_eq!(applied.total_after, None);
        assert_eq!(applied.stop, Stop::Unknown);
        let document = serde_json::to_value(&applied).unwrap();
        assert_eq!(document["total_after"], "unknown");
        assert_eq!(document["stop"], "unknown");
        assert_eq!(document["removals"][0]["total_after"], "unknown");
        assert_eq!(document["removals"][1]["bytes"], 10 * MIB);
    }
}
