use super::native::{github, owned_file, stage_adapter};
use super::{BatchReference, ProjectStore};
use af_contracts::backlog::batch::assessment::ProviderIndex;
use af_contracts::backlog::batch::provider_index::{MAX_INDEX_BYTES, Observation};
use af_contracts::is_digest;
use af_store::canonical::blob_content_id;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) const PIN_FILE: &str = "backlog-provider-index-v1.json";
const OBSERVER: &str = "backlog-github-observer";
pub(in crate::backlog::batch) const READ_CREDENTIAL: &str = "backlog-github-read-token";
const MAX_EXECUTABLE: u64 = 8 * 1024 * 1024;
pub(in crate::backlog::batch) const MAX_READ_CREDENTIAL: u64 = 4096;

#[derive(Clone, Copy)]
pub enum IndexMode {
    Manual,
    RecurringUnindexed,
    RecurringIndexed,
}

pub(super) fn batch_path(store: &ProjectStore, reference: &BatchReference) -> PathBuf {
    store.directory.join(format!(
        "backlog-provider-index-{}.json",
        &reference.batch_id["sha256:".len()..]
    ))
}

fn observation_path(store: &ProjectStore, id: &str) -> PathBuf {
    store.directory.join(format!(
        "backlog-open-issues-{}.json",
        &id["sha256:".len()..]
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    schema: Schema,
    project_id: String,
    batch_id: String,
    source_revision: String,
    destination_id: String,
    observation_id: String,
}

#[derive(Deserialize, Serialize)]
enum Schema {
    #[serde(rename = "af/backlog-provider-index-pin@1")]
    V1,
}

pub(super) fn collect(
    store: &ProjectStore,
    reference: &BatchReference,
    source_revision: &str,
    destination_id: &str,
    path: &Path,
) -> Result<Option<ProviderIndex>, String> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Backlog provider index pin unavailable".into()),
        Ok(_) => {}
    }
    let pin: Pin = serde_json::from_slice(
        &owned_file(path, 4096)
            .map_err(|_| "Backlog provider index pin is unavailable or untrusted")?,
    )
    .map_err(|_| "Invalid backlog provider index pin")?;
    let Schema::V1 = pin.schema;
    if pin.project_id != reference.project_id
        || pin.batch_id != reference.batch_id
        || pin.source_revision != source_revision
        || pin.destination_id != destination_id
        || !is_digest(&pin.observation_id)
    {
        return Err(
            "Backlog provider index pin differs from the Project, batch, source or destination"
                .into(),
        );
    }

    let bytes = owned_file(
        &observation_path(store, &pin.observation_id),
        MAX_INDEX_BYTES,
    )
    .map_err(|_| "Backlog provider observation is unavailable, over-bound or untrusted")?;
    if blob_content_id(&bytes) != pin.observation_id {
        return Err("Backlog provider observation differs from pinned bytes".into());
    }
    let destination = github::Config::load(store, destination_id)?;
    decode(&destination, &pin, &bytes).map(Some)
}

fn decode(destination: &github::Config, pin: &Pin, bytes: &[u8]) -> Result<ProviderIndex, String> {
    let observation: Observation =
        serde_json::from_slice(bytes).map_err(|_| "Invalid backlog provider observation")?;
    observation.validate()?;
    if !observation
        .repository
        .eq_ignore_ascii_case(&destination.repository)
        || observation.repository_id != destination.repository_id
    {
        return Err("Backlog provider observation belongs to a different GitHub repository".into());
    }
    Ok(ProviderIndex {
        observation_id: pin.observation_id.clone(),
        destination_id: pin.destination_id.clone(),
        observation,
    })
}

pub(super) fn observer_pin(store: &ProjectStore, destination_id: &str) -> Result<String, String> {
    github::Config::load(store, destination_id)?;
    Ok(blob_content_id(&observer(store)?))
}

fn observer(store: &ProjectStore) -> Result<Vec<u8>, String> {
    owned_file(&store.directory.join(OBSERVER), MAX_EXECUTABLE)
        .map_err(|_| "GitHub backlog observer is unavailable, over-bound or untrusted".into())
}

pub(super) struct Pending {
    pin: Pin,
    bytes: Vec<u8>,
}

pub(super) fn observe(
    store: &ProjectStore,
    reference: &BatchReference,
    source_revision: &str,
    destination_id: &str,
    executable_pin: &str,
) -> Result<(ProviderIndex, Pending), String> {
    let destination = github::Config::load(store, destination_id)?;
    let executable = observer(store)?;
    if blob_content_id(&executable) != executable_pin {
        return Err("GitHub backlog observer differs from enrolled bytes".into());
    }
    let credential_path = store.directory.join(READ_CREDENTIAL);
    let token = owned_file(&credential_path, MAX_READ_CREDENTIAL)
        .map_err(|_| "GitHub backlog Issues-read credential unavailable or untrusted")?;
    github::check_distinct_credentials(store, &token, github::CREDENTIAL)?;
    let secret = std::str::from_utf8(&token)
        .map_err(|_| "Invalid GitHub backlog Issues-read credential")?
        .trim_end_matches('\n');
    if !(20..=255).contains(&secret.len())
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err("Invalid GitHub backlog Issues-read credential".into());
    }

    let (directory, mut command) = stage_adapter(&store.directory, OBSERVER, &executable)
        .map_err(|_| "Could not stage GitHub backlog observer")?;
    let output_path = directory.path().join("observation.json");
    command
        .arg("--repository")
        .arg(&destination.repository)
        .arg("--repository-id")
        .arg(destination.repository_id.to_string())
        .arg("--github-token-file")
        .arg(&credential_path)
        .arg("--output")
        .arg(&output_path);
    let result = af_process::run_supervised_captured_bounded(
        &mut command,
        None,
        Duration::from_secs(125),
        af_process::ExitPolicy::KillProcessGroup,
        None,
        4096,
    )
    .into_result()
    .map_err(|_| "GitHub backlog observation unavailable")?;
    if !result.status.success() {
        if result.stderr == b"backlog-open-issues: GitHub repository identity refused\n" {
            return Err("GitHub backlog observer refused a different GitHub repository".into());
        }
        return Err(
            "GitHub backlog observation unavailable; check the Issues-read service and credential"
                .into(),
        );
    }
    if !result.stdout.is_empty() || !result.stderr.is_empty() || result.stderr_held {
        return Err("GitHub backlog observer produced unexpected process output".into());
    }
    if !owned_file(&credential_path, MAX_READ_CREDENTIAL).is_ok_and(|current| current == token) {
        return Err("GitHub backlog Issues-read credential changed during observation".into());
    }
    let bytes = owned_file(&output_path, MAX_INDEX_BYTES)
        .map_err(|_| "GitHub backlog observation is unavailable, over-bound or untrusted")?;
    if bytes
        .windows(secret.len())
        .any(|window| window == secret.as_bytes())
    {
        return Err("GitHub backlog observation contains the Issues-read credential".into());
    }
    let pin = Pin {
        schema: Schema::V1,
        project_id: reference.project_id.clone(),
        batch_id: reference.batch_id.clone(),
        source_revision: source_revision.into(),
        destination_id: destination_id.into(),
        observation_id: blob_content_id(&bytes),
    };
    let index = decode(&destination, &pin, &bytes)?;
    Ok((index, Pending { pin, bytes }))
}

impl Pending {
    pub(super) fn commit(
        self,
        store: &ProjectStore,
        reference: &BatchReference,
    ) -> Result<(), String> {
        let raw = observation_path(store, &self.pin.observation_id);
        match fs::symlink_metadata(&raw) {
            Err(error) if error.kind() == ErrorKind::NotFound => {
                persist(&store.directory, &raw, &self.bytes)?
            }
            Ok(_) if owned_file(&raw, MAX_INDEX_BYTES).is_ok_and(|bytes| bytes == self.bytes) => {}
            _ => {
                return Err(
                    "Existing GitHub backlog observation differs from acquired bytes".into(),
                );
            }
        }
        let pin = serde_json::to_vec(&self.pin)
            .map_err(|_| "Could not encode GitHub backlog index pin")?;
        persist(&store.directory, &batch_path(store, reference), &pin)
    }
}

fn persist(root: &Path, target: &Path, bytes: &[u8]) -> Result<(), String> {
    let write = || -> io::Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(root)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(target)?;
        File::open(root)?.sync_all()
    };
    write()
        .map_err(|_| "Could not persist immutable GitHub backlog observation or batch pin".into())
}
