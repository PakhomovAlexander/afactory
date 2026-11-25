use std::path::PathBuf;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::context::{Binding, write_record};
use super::transport::MAX_REPLY_BYTES;

const SCHEMA: &str = "af/fleet-cache@2";
pub(super) const MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cached<T> {
    schema: String,
    visibility: Visibility,
    binding_id: String,
    pub source: String,
    pub subject: String,
    pub observed_unix_ms: u64,
    pub data: T,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Visibility {
    Infrastructure,
}

pub(super) struct Cache {
    root: PathBuf,
    binding_id: String,
    scope: String,
}

impl Cache {
    pub(super) fn new(alias: &str, binding: &Binding) -> Option<Self> {
        let bytes = serde_json::to_vec(&(alias, binding)).ok()?;
        Some(Self {
            root: crate::config::state_home()
                .ok()?
                .join("af/fleet-cache")
                .join(alias),
            binding_id: af_telemetry::trace::sha256_digest(&bytes),
            scope: binding.descriptor.scope.clone(),
        })
    }

    pub(super) fn remember<T: Serialize>(&self, subject: &str, now: u64, data: &T) {
        let save = || -> Result<(), String> {
            af_store::local_fs::create_private_dir_all(&self.root).map_err(|e| e.to_string())?;
            let cached = Cached {
                schema: SCHEMA.into(),
                visibility: Visibility::Infrastructure,
                binding_id: self.binding_id.clone(),
                source: self.scope.clone(),
                subject: subject.into(),
                observed_unix_ms: now,
                data,
            };
            write_record(&self.root, &cached)?
                .persist(self.root.join(format!("{subject}.json")))
                .map_err(|e| e.error.to_string())?;
            Ok(())
        };
        // A failed local cache write does not erase a live observation.
        let _ = save();
    }

    pub(super) fn last<T: DeserializeOwned>(&self, subject: &str, now: u64) -> Option<Cached<T>> {
        let bytes = af_store::local_fs::read_bounded(
            &self.root.join(format!("{subject}.json")),
            (MAX_REPLY_BYTES * 2) as u64,
        )
        .ok()?;
        let record: Cached<T> = serde_json::from_slice(&bytes).ok()?;
        (record.schema == SCHEMA
            && record.binding_id == self.binding_id
            && record.source == self.scope
            && record.subject == subject
            && now.checked_sub(record.observed_unix_ms)? <= MAX_AGE_MS)
            .then_some(record)
    }
}
