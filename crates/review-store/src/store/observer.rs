//! The post-commit observer (design §7.5, ADR-0153): told of every transaction the Store
//! commits, once, after its `COMMIT` returned, with that transaction's events in committed
//! order, and never of a transaction that rolled back.
//!
//! It is how Telemetry mirrors the log without the Store knowing Telemetry exists: this crate
//! links no telemetry, the observer reads what it is handed and the Store's own read API, and
//! nothing it does reaches back into a transaction. It runs after the writer lock is released,
//! so no transaction is longer for it; what it returns is ignored, and a panic in it is caught
//! and dropped, so a mirror failure is never a Store error. With no observer installed (every
//! process that records no Telemetry) a commit costs one atomic load more.

use std::sync::{Arc, OnceLock};

use review_core::RunEvent;
use review_core::task::usage::TaskUsageObservationV1;

use super::task::TaskProjection;
use super::{EventStore, TaskAttemptWall};
use crate::cas::Cas;

/// One committed transaction.
pub struct Commit<'a> {
    /// The Store that committed it, for reads only.
    pub store: &'a EventStore,
    pub cas: &'a Cas,
    /// Its events in committed order: one run's, or, for a Round publication or an atomic
    /// integration, a Campaign's and a Task's in the order they were inserted.
    pub events: &'a [RunEvent],
    /// For one Task transition, the Task projection as that transition left it: the state the
    /// Store validated and committed, at exactly the committed event's sequence.
    pub task: Option<&'a TaskProjection>,
}

/// One committed write of the `attempt_wall` sidecar: the row as it now stands, merged with the
/// row's earlier values (`record_wall` merges component maxima and keeps an incomplete
/// observation sticky). The sidecar is not an event and has no sequence of its own.
pub struct WallCommit<'a> {
    pub store: &'a EventStore,
    pub wall: &'a TaskAttemptWall,
    pub observation: Option<&'a TaskUsageObservationV1>,
}

/// Told of every commit. Implementations must not write to the Store.
pub trait CommitObserver: Send + Sync {
    fn committed(&self, commit: &Commit<'_>);

    fn wall_recorded(&self, _commit: &WallCommit<'_>) {}
}

static PROCESS_OBSERVER: OnceLock<Arc<dyn CommitObserver>> = OnceLock::new();

/// Install the observer every Store of this process tells, unless a Store has one of its own
/// ([`EventStore::observe_commits`]). Once per process; `false` when one is installed already.
pub fn install_commit_observer(observer: Arc<dyn CommitObserver>) -> bool {
    PROCESS_OBSERVER.set(observer).is_ok()
}

fn quietly(work: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
}

impl EventStore {
    /// Tell `observer` of every commit through this Store and every Store [`Self::reopen`]
    /// derives from it, instead of the process's observer.
    pub fn observe_commits(&mut self, observer: Arc<dyn CommitObserver>) {
        self.commit_observer = Some(observer);
    }

    fn observer(&self) -> Option<&Arc<dyn CommitObserver>> {
        self.commit_observer
            .as_ref()
            .or_else(|| PROCESS_OBSERVER.get())
    }

    /// After a `COMMIT` returned: never inside a transaction, never for a rollback.
    pub(super) fn committed(&self, cas: &Cas, events: &[RunEvent], task: Option<&TaskProjection>) {
        if events.is_empty() {
            return;
        }
        if let Some(observer) = self.observer() {
            quietly(|| {
                observer.committed(&Commit {
                    store: self,
                    cas,
                    events,
                    task,
                })
            });
        }
    }

    /// After the sidecar's `COMMIT` returned.
    pub(super) fn wall_committed(
        &self,
        wall: &TaskAttemptWall,
        observation: Option<&TaskUsageObservationV1>,
    ) {
        if let Some(observer) = self.observer() {
            quietly(|| {
                observer.wall_recorded(&WallCommit {
                    store: self,
                    wall,
                    observation,
                })
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewEvent;
    use review_core::EventType;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Seen {
        commits: Mutex<Vec<Vec<(String, u64, String)>>>,
        walls: Mutex<Vec<TaskAttemptWall>>,
    }

    impl CommitObserver for Seen {
        fn committed(&self, commit: &Commit<'_>) {
            // Every event it is told of is already visible: the transaction committed.
            for event in commit.events {
                assert!(commit.store.len(&event.run_id).unwrap() > event.sequence);
            }
            self.commits.lock().unwrap().push(
                commit
                    .events
                    .iter()
                    .map(|event| {
                        (
                            event.run_id.clone(),
                            event.sequence,
                            event.event_type.as_str().to_string(),
                        )
                    })
                    .collect(),
            );
        }

        fn wall_recorded(&self, commit: &WallCommit<'_>) {
            self.walls.lock().unwrap().push(commit.wall.clone());
        }
    }

    struct Panics;

    impl CommitObserver for Panics {
        fn committed(&self, _: &Commit<'_>) {
            panic!("a mirror failure");
        }
    }

    fn fixture() -> (tempfile::TempDir, EventStore, Cas) {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        (dir, store, cas)
    }

    fn captured(index: u64) -> NewEvent {
        NewEvent::new(
            EventType::SourceCapturedV1,
            serde_json::json!({ "index": index }),
        )
    }

    #[test]
    fn every_commit_is_told_once_in_committed_order_and_no_rollback_is() {
        let (_dir, mut store, cas) = fixture();
        let seen = Arc::new(Seen::default());
        store.observe_commits(seen.clone());
        store.append("run", &cas, captured(1)).unwrap();
        store
            .append_batch("run", &cas, &[captured(2), captured(3)])
            .unwrap();
        // Refused inside the transaction, after its first event was validated: a Round's
        // generation without a Round. The whole batch rolls back and is never told.
        let before = store.replay("run").unwrap();
        store
            .append_batch(
                "run",
                &cas,
                &[
                    captured(4),
                    NewEvent::new(
                        EventType::GenerationAdvancedV1,
                        serde_json::json!({ "round": 1 }),
                    ),
                ],
            )
            .unwrap_err();
        assert_eq!(store.replay("run").unwrap(), before, "rolled back whole");
        let commits = seen.commits.lock().unwrap().clone();
        assert_eq!(
            commits,
            vec![
                vec![("run".to_string(), 0, "SourceCaptured@1".to_string())],
                vec![
                    ("run".to_string(), 1, "SourceCaptured@1".to_string()),
                    ("run".to_string(), 2, "SourceCaptured@1".to_string()),
                ],
            ]
        );
    }

    #[test]
    fn an_observer_failure_is_never_a_store_error() {
        let (_dir, mut store, cas) = fixture();
        store.observe_commits(Arc::new(Panics));
        let appended = store.append("run", &cas, captured(1)).unwrap();
        assert_eq!(appended.sequence, 0);
        assert_eq!(store.len("run").unwrap(), 1);
    }

    #[test]
    fn a_sidecar_write_is_told_with_its_merged_row_and_a_reopened_store_keeps_the_observer() {
        let (_dir, mut store, _cas) = fixture();
        let seen = Arc::new(Seen::default());
        store.observe_commits(seen.clone());
        let usage = |input: u128, charge: u128| review_core::task::usage::TaskTokenUsageV3 {
            input_tokens: Some(input.into()),
            chargeable_tokens: charge.into(),
            ..Default::default()
        };
        let wall = |usage| TaskAttemptWall {
            run_id: "task:run".into(),
            attempt_id: "attempt".into(),
            node_id: "node".into(),
            round: 1,
            epoch: 1,
            started_unix_ms: 1,
            elapsed_ms: 2,
            usage: Some(usage),
        };
        store
            .record_task_attempt_wall(&wall(usage(10, 10)))
            .unwrap();
        let reopened = store.reopen(std::time::Duration::from_secs(1)).unwrap();
        reopened
            .record_task_attempt_wall(&wall(usage(4, 12)))
            .unwrap();
        let walls = seen.walls.lock().unwrap().clone();
        assert_eq!(walls.len(), 2);
        // The second write is told as the merged row: input 10 stays, the charge rises.
        assert_eq!(walls[1].usage, Some(usage(10, 12)));
    }
}
