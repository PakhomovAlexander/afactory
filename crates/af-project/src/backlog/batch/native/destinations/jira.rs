use super::*;
use af_process::{ExitPolicy, run_supervised_duplex_cancellable_until};
use std::process::Command;

const CONFIG: &str = "backlog-jira-destination-v1.json";
const EXECUTABLE: &str = "backlog-jira-adapter";
const CREDENTIAL: &str = "backlog-jira-credential";
const EXECUTABLE_LIMIT: u64 = 8 * 1024 * 1024;
const CREDENTIAL_LIMIT: u64 = 4096;
const RESPONSE_LIMIT: u64 = 4096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    schema: String,
    project_id: String,
    site: String,
    jira_project_id: String,
    issue_type_id: String,
    publisher_account_id: String,
    executable_digest: String,
}

fn numeric(value: &str) -> bool {
    !value.starts_with('0')
        && value.len() <= 20
        && !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

impl Config {
    pub(super) fn load(store: &ProjectStore, destination: &str) -> Result<Self, String> {
        let bytes = owned_file(&store.directory.join(CONFIG), 4096)?;
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid Jira destination")?;
        let site = config
            .site
            .strip_suffix(".atlassian.net")
            .unwrap_or_default();
        if config.schema != "af/backlog-jira-destination@1"
            || config.project_id != store.project_id
            || !(1..=63).contains(&site.len())
            || site.starts_with('-')
            || site.ends_with('-')
            || !site
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !numeric(&config.jira_project_id)
            || !numeric(&config.issue_type_id)
            || !(1..=128).contains(&config.publisher_account_id.len())
            || !config
                .publisher_account_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
            || !af_contracts::is_digest(&config.executable_digest)
            || value_id(&config)? != destination
        {
            return Err("Jira destination differs from pinned Project authority".into());
        }
        Ok(config)
    }

    pub(super) fn prepare(self, store: &ProjectStore) -> Result<Adapter, Pending> {
        let read = |name, limit| {
            owned_file(&store.directory.join(name), limit).map_err(|_| Pending::AdapterUnavailable)
        };
        let bytes = read(EXECUTABLE, EXECUTABLE_LIMIT)?;
        if af_store::canonical::blob_content_id(&bytes) != self.executable_digest {
            return Err(Pending::AuthorityUnavailable);
        }
        let credential = read(CREDENTIAL, CREDENTIAL_LIMIT)?;
        let pin = af_store::canonical::blob_content_id(&credential);
        let credential =
            std::str::from_utf8(&credential).map_err(|_| Pending::AuthorityUnavailable)?;
        if credential.contains('\0') {
            return Err(Pending::AuthorityUnavailable);
        }
        let (directory, mut command) = stage_adapter(&store.directory, EXECUTABLE, &bytes)
            .map_err(|_| Pending::AdapterUnavailable)?;
        command.env("AF_BACKLOG_JIRA_CREDENTIAL", credential).env(
            "AF_BACKLOG_JIRA_SCOPE",
            serde_json::to_string(&self).map_err(|_| Pending::AdapterUnavailable)?,
        );
        Ok(Adapter {
            _directory: directory,
            command,
            pin,
            config: self,
        })
    }
}

pub(super) struct Adapter {
    _directory: tempfile::TempDir,
    command: Command,
    pin: String,
    config: Config,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    schema: String,
    request_id: String,
    outcome: Outcome,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    Absent,
    Conflict,
    Unavailable,
    Observed {
        id: String,
        key: String,
        closed: bool,
        fields_id: String,
    },
}

impl Adapter {
    fn check_authority(&self, store: &ProjectStore, destination: &str) -> Result<(), Pending> {
        let matches = |name, limit, pin: &str| {
            owned_file(&store.directory.join(name), limit)
                .is_ok_and(|bytes| af_store::canonical::blob_content_id(&bytes) == pin)
        };
        Config::load(store, destination).map_err(|_| Pending::AuthorityUnavailable)?;
        if !matches(EXECUTABLE, EXECUTABLE_LIMIT, &self.config.executable_digest)
            || !matches(CREDENTIAL, CREDENTIAL_LIMIT, &self.pin)
        {
            return Err(Pending::AuthorityUnavailable);
        }
        Ok(())
    }

    pub(super) fn deliver(
        mut self,
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
        self.check_authority(store, &publication.destination_id)?;
        let issue = mapped
            .map(|remote| {
                remote
                    .strip_prefix("jira:")
                    .filter(|id| numeric(id))
                    .ok_or(Pending::Conflict)
            })
            .transpose()?;
        let request_id = publication.attempt_ids.last().ok_or(Pending::Conflict)?;
        let fields = matches!(operation, Operation::Create).then_some(&item.public);
        let request = serde_json::to_vec(&json!({
            "schema": "af/backlog-jira-request@1",
            "request_id": request_id,
            "scope": self.config,
            "operation": operation,
            "item_id": item.item_id,
            "issue_id": issue,
            "fields": fields,
        }))
        .map_err(|_| Pending::AdapterUnavailable)?;
        let result = run_supervised_duplex_cancellable_until(
            &mut self.command,
            deadline,
            ExitPolicy::KillProcessGroup,
            cancellation,
            |input| input.write_all(&request),
            |output| {
                let mut bytes = Vec::new();
                output.take(RESPONSE_LIMIT + 1).read_to_end(&mut bytes)?;
                Ok::<_, std::io::Error>(bytes)
            },
        )
        .map_err(|_| Pending::AdapterUnavailable)?;
        let bytes = result.output.map_err(|_| Pending::AdapterUnavailable)?;
        if !result.status.success() || result.input.is_err() || bytes.len() as u64 > RESPONSE_LIMIT
        {
            return Err(Pending::AdapterUnavailable);
        }
        let response: Response = serde_json::from_slice(&bytes).map_err(|_| Pending::Conflict)?;
        if response.schema != "af/backlog-jira-response@1" || response.request_id != *request_id {
            return Err(Pending::Conflict);
        }
        match response.outcome {
            Outcome::Observed {
                id,
                key,
                closed,
                fields_id,
            } if numeric(&id)
                && issue.is_none_or(|expected| expected == id)
                && key.split_once('-').is_some_and(|(prefix, number)| {
                    (1..=32).contains(&prefix.len())
                        && prefix.as_bytes()[0].is_ascii_uppercase()
                        && prefix
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                        && numeric(number)
                })
                && af_contracts::is_digest(&fields_id) =>
            {
                self.check_authority(store, &publication.destination_id)?;
                Ok(Observation {
                    remote_id: format!("jira:{id}"),
                    issue_key: Some(key),
                    closed,
                    fields_id,
                })
            }
            Outcome::Conflict | Outcome::Observed { .. } => Err(Pending::Conflict),
            Outcome::Absent => Err(Pending::NotObserved),
            Outcome::Unavailable => Err(Pending::AdapterUnavailable),
        }
    }
}
