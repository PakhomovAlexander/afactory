use super::*;
use std::fs;

const CONFIG: &str = "backlog-markdown-destination-v1.json";
const MARKER: &str = "<!-- af-backlog-item:";
const MAX_ENTRY: u64 = 64 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    schema: String,
    project_id: String,
}

impl Config {
    pub(super) fn load(store: &ProjectStore, destination: &str) -> Result<Self, String> {
        let bytes = owned_file(&store.directory.join(CONFIG), 4096)?;
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid Markdown destination")?;
        if config.schema != "af/backlog-markdown-destination@1"
            || config.project_id != store.project_id
            || value_id(&config)? != destination
        {
            return Err("Markdown destination differs from pinned Project authority".into());
        }
        Ok(config)
    }

    pub(super) fn deliver(
        self,
        store: &ProjectStore,
        delivery: Delivery<'_>,
    ) -> Result<Observation, Pending> {
        let Delivery {
            publication,
            item,
            mapped,
            operation,
            cancellation,
            deadline,
        } = delivery;
        Self::load(store, &publication.destination_id)
            .map_err(|_| Pending::AuthorityUnavailable)?;
        let directory = store
            .directory
            .canonicalize()
            .map_err(|_| Pending::AuthorityUnavailable)?;
        let metadata = fs::metadata(&directory).map_err(|_| Pending::AuthorityUnavailable)?;
        // The destination is machine-owned and cannot target a checkout.
        if metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
            || directory.ancestors().any(|ancestor| {
                !matches!(fs::symlink_metadata(ancestor.join(".git")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound)
                    || (ancestor.join("HEAD").is_file() && ancestor.join("objects").is_dir())
            })
        {
            return Err(Pending::AuthorityUnavailable);
        }
        let filename = format!("backlog-item-{}.md", &item.item_id["sha256:".len()..]);
        if mapped.is_some_and(|expected| expected != filename) {
            return Err(Pending::Conflict);
        }
        let path = directory.join(&filename);
        let prefix = format!("{MARKER}{}:{}:", self.project_id, item.item_id);
        let marker = format!("{prefix}{} -->", publication.intent_id);
        let content = format!("# {}\n\n{}\n", item.public.title, item.public.body);
        if matches!(operation, Operation::Create) {
            let mut staged = tempfile::NamedTempFile::new_in(&directory)
                .map_err(|_| Pending::AdapterUnavailable)?;
            staged
                .write_all(format!("{marker}\n{content}").as_bytes())
                .and_then(|()| staged.as_file().sync_all())
                .map_err(|_| Pending::AdapterUnavailable)?;
            Self::load(store, &publication.destination_id)
                .map_err(|_| Pending::AuthorityUnavailable)?;
            running(cancellation, deadline).map_err(|_| Pending::AuthorityUnavailable)?;
            staged.persist_noclobber(&path).map_err(|error| {
                if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                    Pending::Conflict
                } else {
                    Pending::AdapterUnavailable
                }
            })?;
        }
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Pending::NotObserved);
            }
            Err(_) => return Err(Pending::AdapterUnavailable),
            Ok(_) => {}
        }
        let bytes = owned_file(&path, MAX_ENTRY).map_err(|_| Pending::Conflict)?;
        let observed = std::str::from_utf8(&bytes).map_err(|_| Pending::Conflict)?;
        let mut markers = observed.lines().filter(|line| line.contains(MARKER));
        let found = markers.next().ok_or(Pending::Conflict)?;
        let intent = found
            .strip_prefix(&prefix)
            .and_then(|line| line.strip_suffix(" -->"))
            .filter(|intent| af_contracts::is_digest(intent))
            .ok_or(Pending::Conflict)?;
        if markers.next().is_some() || (mapped.is_none() && intent != publication.intent_id) {
            return Err(Pending::Conflict);
        }
        fs::File::open(&directory)
            .and_then(|file| file.sync_all())
            .map_err(|_| Pending::AdapterUnavailable)?;
        let fields_id = if observed
            .split_once('\n')
            .is_some_and(|(header, rest)| header == found && rest == content)
        {
            value_id(&(&item.public.title, &item.public.body)).map_err(|_| Pending::Conflict)?
        } else {
            af_store::canonical::blob_content_id(&bytes)
        };
        Ok(Observation {
            remote_id: filename,
            issue_key: None,
            closed: false,
            fields_id,
        })
    }
}
