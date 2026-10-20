//! A sweep's one plan (ADR-0144, ADR-0152 clause 13). [`make`] takes one inventory snapshot and
//! returns, in order, every entry the sweep removes with the bytes it frees, its rule
//! (`recovery`, `collection`, `keep_surveys`, `budget`) and the total after it, and every file
//! or Survey it leaves with its reason (`held`, `foreign`, `oversize`, `unreadable_name`). It
//! reads nothing itself: it works over an in-memory model of the snapshot that its own removals
//! update, so the bytes of a record an interruption moved under `cas/removing` are subtracted
//! once, from the Project Store's entry that holds them, and never counted again. The preview
//! prints the plan; the applied sweep runs it entry by entry with [`run`] and reports what each
//! entry actually did, a Survey's deviation (the lock lost, its record replaced) marked on that
//! entry, and never plans again.

use std::path::{Path, PathBuf};

use super::{Entry, HOUR_MS, Kind, Removal, Rule, Stop, SweepReport};
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
}

/// What running one planned entry did: the bytes it freed and, for a Survey, why it was
/// collected `unreadable`; or why it did not.
pub(crate) type Outcome = Result<(u64, Option<String>), String>;

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
    /// The snapshot's Project Stores, in inventory order.
    pub(crate) projects: &'a [(PathBuf, collect::Look)],
    /// The finished Tasks collection takes, in order.
    pub(crate) tasks: Vec<TaskPick>,
    pub(crate) collection: bool,
    pub(crate) max_bytes: u64,
    pub(crate) keep_campaigns: usize,
    pub(crate) keep_surveys: usize,
    pub(crate) idle_ms: u64,
    pub(crate) now: u64,
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
    /// The model's total after the last step.
    pub(crate) total_after: u64,
    pub(crate) stop: Stop,
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

    /// The bytes of the Survey entry at `object`; 0 when the snapshot holds none.
    fn survey_bytes(&self, object: &Path) -> u64 {
        self.entries
            .iter()
            .find(|entry| entry.kind == Kind::Survey && entry.path == object)
            .map_or(0, |entry| entry.bytes)
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
        removal.total_after = model.total;
        self.steps.push(Planned { removal, job });
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
        bytes,
        rule,
        total_after: 0,
        unreadable: None,
        deviation: None,
    }
}

/// Plan one sweep over one snapshot: first the deletions interruptions left in Project Stores
/// (`recovery`); then, when `collection`, the finished Tasks the Stores' rules take, the
/// campaigns beyond `keep_campaigns` and the unreadable Stores idle at least `idle_ms`
/// (`collection`) and the Surveys of each Project beyond `keep_surveys` (`keep_surveys`); then,
/// while the model holds more than `max_bytes`, the least recently used entry not in use and not
/// used within the hour (`budget`). `judge` reads a Survey the plan picks; nothing else is read.
/// The Surveys of a Store whose lock is held are left `held`, a record over the ceiling
/// `oversize`, and what a Store's look found foreign or unreadably named is left as it was found.
pub(crate) fn make(mut inputs: Inputs<'_>, judge: &mut dyn FnMut(&Path, &str) -> Judged) -> Plan {
    let mut model = Model::new(std::mem::take(&mut inputs.entries));
    let mut plan = Plan {
        total_before: model.total,
        steps: Vec::new(),
        left: Vec::new(),
        failures: Vec::new(),
        total_after: 0,
        stop: Stop::Fits,
    };
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
            if let Some(reason) = &look.held {
                plan.held(pending.object.clone(), pending.bytes, reason);
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
                    Kind::Survey,
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
        collect_into(&mut plan, &mut model, &inputs, judge);
    }
    if model.total > inputs.max_bytes {
        let mut candidates: Vec<Entry> = model
            .entries
            .iter()
            .filter(|entry| {
                !matches!(entry.kind, Kind::TaskStore | Kind::ProjectStore)
                    && entry.in_use.is_none()
                    && entry.last_use_unix_ms.saturating_add(HOUR_MS) <= inputs.now
            })
            .cloned()
            .collect();
        candidates.sort_by(|a, b| {
            a.last_use_unix_ms
                .cmp(&b.last_use_unix_ms)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        for entry in candidates {
            if model.total <= inputs.max_bytes {
                break;
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
            match judge(&store, &survey_id) {
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
fn collect_into(
    plan: &mut Plan,
    model: &mut Model,
    inputs: &Inputs<'_>,
    judge: &mut dyn FnMut(&Path, &str) -> Judged,
) {
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
            let bytes = model.survey_bytes(&object);
            if let Some(reason) = &look.held {
                plan.held(object, bytes, reason);
                continue;
            }
            match judge(store, &live.survey_id) {
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

/// Run `plan` entry by entry with `execute`, never planning again, into `report`, whose
/// `total_before` already holds the snapshot's total. Each entry reports what it actually freed
/// and the total after it; one that failed is a failure, and a Survey's stays in its place with
/// nothing freed and the deviation marked on it. The stop is what the total after the last entry
/// says.
pub(crate) fn run(
    plan: Plan,
    report: &mut SweepReport,
    execute: &mut dyn FnMut(&Planned) -> Outcome,
) {
    let mut total = plan.total_before;
    report
        .left
        .extend(plan.left.into_iter().map(super::Left::of));
    report.failures.extend(plan.failures);
    for planned in plan.steps {
        let mut removal = planned.removal.clone();
        match execute(&planned) {
            Ok((freed, unreadable)) => {
                total = total.saturating_sub(freed);
                removal.bytes = freed;
                removal.unreadable = unreadable;
                removal.total_after = total;
                report.removals.push(removal);
            }
            Err(why) => {
                report.failures.push(why.clone());
                if removal.kind == Kind::Survey {
                    removal.bytes = 0;
                    removal.unreadable = None;
                    removal.total_after = total;
                    removal.deviation = Some(why);
                    report.removals.push(removal);
                }
            }
        }
    }
    report.total_after = total;
    report.stop = stop(total, report.max_bytes);
}

/// The plan as a preview reports it: every entry as planned.
pub(crate) fn preview(plan: Plan, report: &mut SweepReport) {
    report
        .left
        .extend(plan.left.into_iter().map(super::Left::of));
    report.failures.extend(plan.failures);
    report
        .removals
        .extend(plan.steps.into_iter().map(|planned| planned.removal));
    report.total_after = plan.total_after;
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
            identity: None,
        }
    }

    fn report(max_bytes: u64, applied: bool) -> SweepReport {
        SweepReport {
            applied,
            collection: true,
            max_bytes,
            total_before: 0,
            total_after: 0,
            stop: Stop::Fits,
            removals: Vec::new(),
            left: Vec::new(),
            gate_cleanups: Vec::new(),
            registry_dropped: Vec::new(),
            failures: Vec::new(),
        }
    }

    const STORE: &str = "/state/af/project/aa";

    fn survey_id(n: u8) -> String {
        format!("sha256:{}", format!("{n:02x}").repeat(32))
    }

    fn identity(inode: u64) -> review_sandbox::Identity {
        review_sandbox::Identity { device: 1, inode }
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
            projects: &projects,
            tasks: Vec::new(),
            collection: true,
            max_bytes: 85,
            keep_campaigns: 10,
            keep_surveys: 50,
            idle_ms: 14 * DAY,
            now: NOW,
        };
        let mut judge = |_: &Path, _: &str| -> Judged { unreachable!("no Survey is picked") };
        let plan = make(inputs(), &mut judge);
        assert_eq!(plan.total_before, 110);
        let steps: Vec<(Rule, u64, u64)> = plan
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
        assert_eq!(steps, [(Rule::Recovery, 10, 100), (Rule::Budget, 20, 80)]);
        assert_eq!(plan.total_after, 80);
        assert_eq!(plan.stop, Stop::Fits);

        let mut previewed = report(85, false);
        previewed.total_before = plan.total_before;
        preview(plan.clone(), &mut previewed);
        let mut applied = report(85, true);
        applied.total_before = plan.total_before;
        run(plan, &mut applied, &mut |planned: &Planned| {
            Ok((planned.removal.bytes, None))
        });
        for document in [&previewed, &applied] {
            let removals: Vec<(Rule, u64, u64)> = document
                .removals
                .iter()
                .map(|removal| (removal.rule, removal.bytes, removal.total_after))
                .collect();
            assert_eq!(removals, steps);
            assert_eq!(document.total_after, 80);
            assert_eq!(document.stop, Stop::Fits);
        }
        let as_json = |document: &SweepReport| serde_json::to_value(&document.removals).unwrap();
        assert_eq!(as_json(&previewed), as_json(&applied), "entry for entry");
    }

    /// The applied sweep never chooses again: a Survey whose job finds its record replaced keeps
    /// its place with nothing freed and the deviation marked, and no other entry takes its place.
    #[test]
    fn a_deviation_is_marked_on_its_entry_and_nothing_is_planned_again() {
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
        let projects = vec![(PathBuf::from(STORE), collect::Look::default())];
        let plan = make(
            Inputs {
                entries,
                projects: &projects,
                tasks: Vec::new(),
                collection: false,
                max_bytes: 50,
                keep_campaigns: 10,
                keep_surveys: 50,
                idle_ms: 14 * DAY,
                now: NOW,
            },
            &mut |_, _| Judged::Take {
                unreadable: None,
                identity: Some(identity(9)),
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
                Job::Survey(_) => Err("Survey x is refused (replaced during collection)".into()),
                _ => Ok((planned.removal.bytes, None)),
            },
        );
        assert_eq!(applied.removals.len(), 2, "{:?}", applied.removals);
        assert_eq!(applied.removals[0].bytes, 0);
        assert!(applied.removals[0].deviation.is_some());
        assert_eq!(applied.removals[1].total_after, 84);
        assert_eq!(applied.total_after, 84);
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
                projects: &projects,
                tasks: Vec::new(),
                collection: true,
                max_bytes: u64::MAX,
                keep_campaigns: 10,
                keep_surveys: 0,
                idle_ms: 0,
                now: NOW,
            },
            &mut |_, _| unreachable!("a held Store's Surveys are not read"),
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
}
