//! Receiver-owned transport for the narrow Project backlog publication action.

use super::*;
use af_contracts::is_digest;
use af_process::{ExitPolicy, run_supervised_duplex_cancellable_until};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use std::process::Command;

const CONFIG: &str = "backlog-github-destination-v1.json";
const EXECUTABLE: &str = "backlog-github-adapter";
const CREDENTIAL: &str = "backlog-github-token";
const MAX_RESPONSE: u64 = 4096;
const MAX_EXECUTABLE: u64 = 8 * 1024 * 1024;
const MAX_CREDENTIAL: u64 = 1024;
const MAX_ATTEMPTS: usize = 16;

fn numeric_id(id: u64) -> bool {
    (1..=af_contracts::json::SAFE_INTEGER_MAX as u64).contains(&id)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::backlog::batch) struct Config {
    schema: String,
    project_id: String,
    pub(in crate::backlog::batch) repository: String,
    pub(in crate::backlog::batch) repository_id: u64,
    publisher_id: u64,
    executable_digest: String,
}

impl Config {
    pub(in crate::backlog::batch) fn load(
        store: &ProjectStore,
        destination: &str,
    ) -> Result<Self, String> {
        let bytes = owned_file(&store.directory.join(CONFIG), MAX_RESPONSE)?;
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid GitHub destination")?;
        let segment = |part: &str| {
            !matches!(part, "" | "." | "..")
                && part.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
                })
        };
        if config.schema != "af/backlog-github-destination@1"
            || config.project_id != store.project_id
            || config.repository.len() > 141
            || !config
                .repository
                .split_once('/')
                .is_some_and(|(owner, repo)| segment(owner) && segment(repo))
            || !numeric_id(config.repository_id)
            || !numeric_id(config.publisher_id)
            || !is_digest(&config.executable_digest)
            || value_id(&config)? != destination
        {
            return Err("GitHub destination differs from pinned Project authority".into());
        }
        Ok(config)
    }

    fn command(
        &self,
        store: &ProjectStore,
        destination: &str,
    ) -> Result<(tempfile::TempDir, Command, String), String> {
        let bytes = owned_file(&store.directory.join(EXECUTABLE), MAX_EXECUTABLE)?;
        if af_store::canonical::blob_content_id(&bytes) != self.executable_digest {
            return Err("GitHub backlog executable differs from pinned bytes".into());
        }
        let token = owned_file(&store.directory.join(CREDENTIAL), MAX_CREDENTIAL)?;
        let token_pin = af_store::canonical::blob_content_id(&token);
        let token = std::str::from_utf8(&token)
            .map_err(|_| "Invalid GitHub backlog credential")?
            .trim_end_matches('\n');
        if token.is_empty()
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err("Invalid GitHub backlog credential".into());
        }
        let (directory, mut command) = stage_adapter(&store.directory, EXECUTABLE, &bytes)
            .map_err(|_| "Could not stage GitHub adapter")?;
        command.env("AF_BACKLOG_GITHUB_TOKEN", token).env(
            "AF_BACKLOG_GITHUB_SCOPE",
            json!({
                "project_id": self.project_id, "destination_id": destination,
                "repository": self.repository, "repository_id": self.repository_id,
                "publisher_id": self.publisher_id,
            })
            .to_string(),
        );
        Ok((directory, command, token_pin))
    }
}

pub(super) fn check_destination(store: &ProjectStore, destination: &str) -> bool {
    Config::load(store, destination).is_ok()
}

pub(super) fn check_fields(public: &PublicFields) -> Result<(), String> {
    if public.body.contains("<!-- af-backlog-item:")
        || public.title.contains("<!-- af-backlog-item:")
    {
        return Err("Approved backlog text contains a reserved publication marker".into());
    }
    Ok(())
}

fn matches_pin(path: &Path, max_bytes: u64, pin: &str) -> bool {
    owned_file(path, max_bytes)
        .is_ok_and(|bytes| af_store::canonical::blob_content_id(&bytes) == pin)
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Create,
    Reconcile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    id: u64,
    number: u64,
}

impl Issue {
    fn stored(self) -> String {
        format!("{}:{}", self.id, self.number)
    }

    fn parse(value: &str) -> Result<Self, String> {
        let (id, number) = value.split_once(':').ok_or("Invalid stored GitHub issue")?;
        let issue = Self {
            id: id.parse().map_err(|_| "Invalid stored GitHub issue ID")?,
            number: number
                .parse()
                .map_err(|_| "Invalid stored GitHub issue number")?,
        };
        if !numeric_id(issue.id) || !numeric_id(issue.number) || issue.stored() != value {
            return Err("Invalid stored GitHub issue".into());
        }
        Ok(issue)
    }
}

#[derive(Serialize)]
struct Request<'a> {
    schema: &'static str,
    request_id: &'a str,
    operation: Operation,
    project_id: &'a str,
    destination_id: &'a str,
    repository: &'a str,
    repository_id: u64,
    publisher_id: u64,
    item_id: &'a str,
    revision_id: &'a str,
    marker: &'a str,
    issue: Option<Issue>,
    fields: Option<&'a PublicFields>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    schema: String,
    request_id: String,
    repository: String,
    repository_id: u64,
    publisher_id: u64,
    outcome: Outcome,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    Absent,
    Conflict,
    Unavailable,
    Observed {
        issue: Issue,
        marker: String,
        author_id: u64,
        state: IssueState,
        fields_id: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IssueState {
    Open,
    Closed,
}

fn exchange(
    command: &mut Command,
    request: &Request<'_>,
    approved_fields_id: &str,
    deadline: Instant,
    cancellation: &AtomicBool,
) -> (Option<Issue>, serde_json::Value) {
    let unavailable = || (None, json!({"kind":"transport_unavailable"}));
    let Ok(bytes) = serde_json::to_vec(request) else {
        return unavailable();
    };
    let Ok(result) = run_supervised_duplex_cancellable_until(
        command,
        deadline,
        ExitPolicy::KillProcessGroup,
        cancellation,
        |input| input.write_all(&bytes),
        |output| {
            let mut bytes = Vec::new();
            output.take(MAX_RESPONSE + 1).read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        },
    ) else {
        return unavailable();
    };
    let Ok(output) = result.output else {
        return unavailable();
    };
    if !result.status.success() || result.input.is_err() || output.len() as u64 > MAX_RESPONSE {
        return unavailable();
    }
    let Ok(response) = serde_json::from_slice::<Response>(&output) else {
        return (None, json!({"kind":"invalid_response"}));
    };
    if response.schema != "af/backlog-github-response@1"
        || response.request_id != request.request_id
        || response.repository != request.repository
        || response.repository_id != request.repository_id
        || response.publisher_id != request.publisher_id
    {
        return (None, json!({"kind":"identity_conflict"}));
    }
    match response.outcome {
        Outcome::Observed {
            issue,
            marker,
            author_id,
            state,
            fields_id,
        } if numeric_id(issue.id)
            && numeric_id(issue.number)
            && request.issue.is_none_or(|expected| expected == issue)
            && marker == request.marker
            && author_id == request.publisher_id
            && is_digest(&fields_id)
            && (request.operation != Operation::Create || fields_id == approved_fields_id) =>
        {
            (
                Some(issue),
                json!({
                    "kind":"observed", "issue":issue, "state":state, "fields_id":fields_id,
                    "approved_fields_match":fields_id == approved_fields_id
                }),
            )
        }
        Outcome::Observed { .. } | Outcome::Conflict => (None, json!({"kind":"identity_conflict"})),
        Outcome::Absent => (None, json!({"kind":"not_observed"})),
        Outcome::Unavailable => unavailable(),
    }
}

impl ProjectStore {
    pub(super) fn publish_github_backlog(
        &mut self,
        proof: &NativeAssessment<'_>,
        state: &State,
        publication: &mut Publication,
        item: &VerifiedItem,
        mapped_items: &BTreeMap<String, String>,
        cancellation: &AtomicBool,
    ) -> Result<bool, String> {
        let batch = proof.collection;
        let batch_id = &batch.reference.batch_id;
        let live = || -> Result<(), String> {
            let pin =
                self.check_native_batch(state, &batch.reference, &batch.scenario_id, proof.limits)?;
            if pin.policy.hold_policy != HoldPolicy::AllowLocalReservations
                || state.batches[batch_id].closed_unix_ms.is_some()
                || pin.policy.publication_limit == 0
                || item.item_id != publication.item_id
                || item.revision_id != publication.revision_id
            {
                return Err(
                    "GitHub backlog intent is outside current publication authority".into(),
                );
            }
            running(cancellation, proof.deadline)
        };
        live()?;
        if publication.attempt_ids.len() >= MAX_ATTEMPTS {
            return self.github_pending(batch_id, publication, "attempt_limit");
        }
        let issue = mapped_items
            .get(&item.item_id)
            .map(|remote| Issue::parse(remote))
            .transpose()?;
        let operation = if issue.is_none() && publication.attempt_ids.is_empty() {
            Operation::Create
        } else {
            Operation::Reconcile
        };
        let marker = format!(
            "<!-- af-backlog-item:{}:{} -->",
            self.project_id, item.item_id
        );
        check_fields(&item.public)?;
        let Ok(config) = Config::load(self, &publication.destination_id) else {
            return self.github_pending(batch_id, publication, "authority_unavailable");
        };
        let Ok((_staged, mut command, token_pin)) =
            config.command(self, &publication.destination_id)
        else {
            return self.github_pending(batch_id, publication, "adapter_unavailable");
        };
        let approved_fields_id = value_id(&(&item.public.title, &item.public.body))?;
        let attempt_id = value_id(&(
            "af/backlog-github-attempt@1",
            &publication.intent_id,
            publication.attempt_ids.len(),
            operation,
        ))?;
        live()?;
        // A committed attempt is ambiguous even if a crash prevented spawn. Only an intent
        // with no attempt may issue create. Every subsequent invocation is read-only.
        publication.attempt_ids.push(attempt_id.clone());
        self.record_github_state(batch_id, publication, &json!({
            "schema":"af/backlog-github-state@1", "attempt_id":attempt_id, "operation":operation,
            "outcome":{"kind":"attempt_prepared"}
        }), false)?;

        self.check_native_batch(state, &batch.reference, &batch.scenario_id, proof.limits)?;
        if Config::load(self, &publication.destination_id).is_err()
            || !matches_pin(
                &self.directory.join(EXECUTABLE),
                MAX_EXECUTABLE,
                &config.executable_digest,
            )
            || !matches_pin(&self.directory.join(CREDENTIAL), MAX_CREDENTIAL, &token_pin)
        {
            return self.github_pending(batch_id, publication, "authority_unavailable");
        }
        running(cancellation, proof.deadline)?;
        let request = Request {
            schema: "af/backlog-github-request@1",
            request_id: &attempt_id,
            operation,
            project_id: &self.project_id,
            destination_id: &publication.destination_id,
            repository: &config.repository,
            repository_id: config.repository_id,
            publisher_id: config.publisher_id,
            item_id: &item.item_id,
            revision_id: &item.revision_id,
            marker: &marker,
            issue,
            fields: (operation == Operation::Create).then_some(&item.public),
        };
        let (issue, outcome) = exchange(
            &mut command,
            &request,
            &approved_fields_id,
            proof.deadline,
            cancellation,
        );
        let remote_id = issue.map(Issue::stored);
        let conflict = remote_id.as_ref().is_some_and(|remote| {
            mapped_items
                .iter()
                .any(|(other, mapped)| other != &item.item_id && mapped == remote)
        });
        let outcome = if conflict {
            json!({"kind":"identity_conflict"})
        } else {
            publication.remote_id = remote_id;
            outcome
        };
        let resolved = publication.remote_id.is_some();
        self.record_github_state(batch_id, publication, &json!({
            "schema":"af/backlog-github-state@1", "attempt_id":attempt_id, "operation":operation,
            "outcome":outcome
        }), resolved)?;
        Ok(resolved)
    }

    fn github_pending(
        &mut self,
        batch_id: &str,
        publication: &mut Publication,
        reason: &str,
    ) -> Result<bool, String> {
        self.record_github_state(
            batch_id,
            publication,
            &json!({
                "schema":"af/backlog-github-state@1", "attempt_id":publication.attempt_ids.last(),
                "operation":null, "outcome":{"kind":reason}
            }),
            false,
        )?;
        Ok(false)
    }

    fn record_github_state(
        &mut self,
        batch_id: &str,
        publication: &mut Publication,
        outcome: &serde_json::Value,
        resolved: bool,
    ) -> Result<(), String> {
        let state = self.cas.put_json(outcome).map_err(|e| e.to_string())?;
        if publication.provider_state_id.as_ref() == Some(&state) {
            return Ok(());
        }
        publication.provider_state_id = Some(state);
        if resolved {
            publication.receipt_id =
                Some(value_id(&("af/backlog-github-receipt@1", &publication))?);
        }
        self.append_processing(
            batch_id,
            vec![BacklogRecordV2::Publication(publication.clone())],
        )
    }
}
