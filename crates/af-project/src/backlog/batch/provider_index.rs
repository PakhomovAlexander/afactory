use super::native::{github::Config, owned_file};
use super::{BatchReference, ProjectStore};
use af_contracts::backlog::batch::assessment::ProviderIndex;
use af_contracts::backlog::batch::provider_index::{MAX_INDEX_BYTES, Observation};
use af_contracts::is_digest;
use af_store::canonical::blob_content_id;
use serde::Deserialize;
use std::io::ErrorKind;

const PIN_FILE: &str = "backlog-provider-index-v1.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    schema: Schema,
    project_id: String,
    batch_id: String,
    source_revision: String,
    destination_id: String,
    observation_id: String,
}

#[derive(Deserialize)]
enum Schema {
    #[serde(rename = "af/backlog-provider-index-pin@1")]
    V1,
}

pub(super) fn collect(
    store: &ProjectStore,
    reference: &BatchReference,
    source_revision: &str,
    destination_id: &str,
) -> Result<Option<ProviderIndex>, String> {
    let path = store.directory.join(PIN_FILE);
    match path.symlink_metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Backlog provider index pin unavailable".into()),
        Ok(_) => {}
    }
    let pin: Pin = serde_json::from_slice(
        &owned_file(&path, 4096)
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

    let destination = Config::load(store, destination_id)?;
    let name = format!(
        "backlog-open-issues-{}.json",
        &pin.observation_id["sha256:".len()..]
    );
    let bytes = owned_file(&store.directory.join(name), MAX_INDEX_BYTES)
        .map_err(|_| "Backlog provider observation is unavailable, over-bound or untrusted")?;
    if blob_content_id(&bytes) != pin.observation_id {
        return Err("Backlog provider observation differs from pinned bytes".into());
    }
    let observation: Observation =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid backlog provider observation")?;
    observation.validate()?;
    if !observation
        .repository
        .eq_ignore_ascii_case(&destination.repository)
        || observation.repository_id != destination.repository_id
    {
        return Err("Backlog provider observation belongs to a different GitHub repository".into());
    }
    Ok(Some(ProviderIndex {
        observation_id: pin.observation_id,
        destination_id: pin.destination_id,
        observation,
    }))
}
