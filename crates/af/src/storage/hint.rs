//! A note per Task Store of the last collection that found nothing to collect, so the sweep
//! after every run does not plan an unchanged Store again before anything in it can become
//! collectable. Kept below `$XDG_STATE_HOME/af/storage/hints`, never inside the Store; best
//! effort both ways: a missing or stale note only means the Store is planned again.

use std::path::{Path, PathBuf};

use review_store::store::task::collection::{CollectionDisposition, CollectionPlan};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::StoragePolicy;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Hint {
    /// The log's length and modification time, and its write-ahead log's, when it was planned.
    log: Vec<(u64, i64, i64)>,
    keep_days: u64,
    keep_tasks: usize,
    /// Nothing in the Store can become collectable before this time unless its log changes.
    not_before_unix_ms: u64,
    /// Some finished Task still has gate leftovers to retry.
    gate_pending: bool,
}

fn fingerprint(state: &Path) -> Vec<(u64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    ["events.sqlite", "events.sqlite-wal"]
        .iter()
        .map(|name| {
            // An empty write-ahead log is what a reader leaves; it says nothing was written.
            std::fs::symlink_metadata(state.join(name))
                .ok()
                .filter(|metadata| metadata.len() > 0)
                .map_or((0, 0, 0), |metadata| {
                    (metadata.len(), metadata.mtime(), metadata.mtime_nsec())
                })
        })
        .collect()
}

fn location(roots: &super::Roots, state: &Path) -> Option<PathBuf> {
    let digest = Sha256::digest(state.as_os_str().as_encoded_bytes());
    Some(
        roots
            .registry
            .parent()?
            .join("storage")
            .join("hints")
            .join(format!("{}.json", &review_core::hex::encode(&digest)[..32])),
    )
}

impl Hint {
    pub(super) fn read(roots: &super::Roots, state: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(location(roots, state)?).ok()?).ok()
    }

    /// Whether the Store can be passed over now: unchanged since the note, under the same
    /// policy, nothing collectable before the noted time, and no gate leftovers to retry.
    pub(super) fn quiet(&self, state: &Path, now: u64, policy: &StoragePolicy) -> bool {
        !self.gate_pending
            && now < self.not_before_unix_ms
            && self.keep_days == policy.keep_days
            && self.keep_tasks == policy.keep_tasks
            && self.log == fingerprint(state)
    }

    /// The note a plan that collected nothing leaves: the earliest a Task kept for being recent
    /// or leased can become collectable. A Task kept among the newest becomes collectable only
    /// when a newer one finishes, which changes the log.
    pub(super) fn after(
        plan: &CollectionPlan,
        state: &Path,
        policy: &StoragePolicy,
        gate_pending: bool,
    ) -> Self {
        let not_before_unix_ms = plan
            .tasks
            .iter()
            .filter_map(|task| match &task.disposition {
                CollectionDisposition::KeptRecent => {
                    Some(task.last_event_unix_ms.saturating_add(plan.older_than_ms))
                }
                CollectionDisposition::WriterLease { until_unix_ms } => Some(*until_unix_ms),
                _ => None,
            })
            .min()
            .unwrap_or(u64::MAX);
        Self {
            log: fingerprint(state),
            keep_days: policy.keep_days,
            keep_tasks: policy.keep_tasks,
            not_before_unix_ms,
            gate_pending,
        }
    }

    pub(super) fn write(&self, roots: &super::Roots, state: &Path) {
        let Some(path) = location(roots, state) else {
            return;
        };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                let staged = path.with_extension("json.staged");
                std::fs::write(&staged, serde_json::to_vec(self).unwrap_or_default())?;
                std::fs::rename(staged, &path)
            });
        if let Err(error) = written {
            eprintln!("af storage: warning: a collection note was not kept: {error}");
        }
    }
}
