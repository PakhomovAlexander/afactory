//! Receiver-owned Markdown and Jira publication under the native Project journal.

use super::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_ATTEMPTS: usize = 16;

mod jira;
mod markdown;
pub(super) use markdown::History as MarkdownHistory;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::backlog::batch) enum Kind {
    Local,
    GitHub,
    Markdown,
    Jira,
}

impl Kind {
    pub(in crate::backlog::batch) fn resolve(
        store: &ProjectStore,
        destination: &str,
    ) -> Result<Self, String> {
        if destination == local_destination(&store.project_id)? {
            return Ok(Self::Local);
        }
        // Each configuration has a distinct schema and is bound by its full content ID.
        for kind in [Self::Markdown, Self::Jira, Self::GitHub] {
            if kind.admitted(store, destination) {
                return Ok(kind);
            }
        }
        Err("No receiver-owned backlog destination matches the pinned policy".into())
    }

    pub(in crate::backlog::batch) fn from_resource(
        store: &ProjectStore,
        destination: &str,
        resource: &str,
    ) -> Result<Self, String> {
        let kinds: &[Self] = if destination == local_destination(&store.project_id)? {
            &[Self::Local]
        } else {
            &[Self::GitHub, Self::Markdown, Self::Jira]
        };
        kinds
            .iter()
            .copied()
            .find(|kind| kind.resource(&store.project_id, destination) == resource)
            .ok_or_else(|| {
                "Backlog action destination differs from the receiving Project policy".into()
            })
    }

    pub(in crate::backlog::batch) fn resource(self, project: &str, destination: &str) -> String {
        match self {
            Self::Local => format!("local-backlog:{project}"),
            Self::GitHub => format!("github-backlog:{project}:{destination}"),
            Self::Markdown => format!("markdown-backlog:{project}:{destination}"),
            Self::Jira => format!("jira-backlog:{project}:{destination}"),
        }
    }

    pub(super) fn intent_domain(self) -> &'static str {
        match self {
            Self::Local => "af/backlog-local-intent@1",
            Self::GitHub => "af/backlog-github-intent@1",
            Self::Markdown => "af/backlog-markdown-intent@2",
            Self::Jira => "af/backlog-jira-intent@1",
        }
    }

    pub(super) fn admitted(self, store: &ProjectStore, destination: &str) -> bool {
        match self {
            Self::Local => local_destination(&store.project_id).is_ok_and(|id| id == destination),
            Self::GitHub => github::check_destination(store, destination),
            Self::Markdown => markdown::Config::load(store, destination).is_ok(),
            Self::Jira => jira::Config::load(store, destination).is_ok(),
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Create,
    Reconcile,
}

struct Delivery<'a> {
    publication: &'a Publication,
    item: &'a VerifiedItem,
    mapped: Option<&'a str>,
    operation: Operation,
    cancellation: &'a AtomicBool,
    deadline: Instant,
}

#[derive(Serialize)]
struct Observation {
    remote_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_key: Option<String>,
    closed: bool,
    fields_id: String,
}

#[derive(Clone, Copy)]
enum Pending {
    AdapterUnavailable,
    AuthorityUnavailable,
    Conflict,
    PrewriteConflict,
    NotObserved,
    AttemptLimit,
}

impl Pending {
    fn kind(self) -> &'static str {
        match self {
            Self::AdapterUnavailable => "adapter_unavailable",
            Self::AuthorityUnavailable => "authority_unavailable",
            Self::Conflict => "identity_conflict",
            Self::PrewriteConflict => "prewrite_conflict",
            Self::NotObserved => "not_observed",
            Self::AttemptLimit => "attempt_limit",
        }
    }
}

impl ProjectStore {
    pub(super) fn publish_configured_backlog(
        &mut self,
        proof: &NativeAssessment<'_>,
        state: &State,
        publication: &mut Publication,
        item: &VerifiedItem,
        mappings: (&BTreeMap<String, String>, &MarkdownHistory),
        cancellation: &AtomicBool,
    ) -> Result<bool, String> {
        let (mapped_items, markdown_history) = mappings;
        let collection = proof.collection;
        let batch = &collection.reference.batch_id;
        let destination = publication.destination_id.clone();
        let Ok(kind) = Kind::resolve(self, &destination) else {
            return self.record_destination_state(
                batch,
                publication,
                None,
                json!({"kind": Pending::AuthorityUnavailable.kind()}),
            );
        };
        let live = |store: &Self| {
            let pin = store.check_native_batch(
                state,
                &collection.reference,
                &collection.scenario_id,
                proof.limits,
            )?;
            if pin.policy.hold_policy != HoldPolicy::AllowLocalReservations
                || pin.policy.destination_id != destination
                || pin.policy.publication_limit == 0
                || state.batches[batch].closed_unix_ms.is_some()
            {
                return Err("Backlog intent is outside current publication authority".into());
            }
            running(cancellation, proof.deadline)
        };
        live(self)?;
        let previous_attempts = publication.attempt_ids.len();
        let capped = previous_attempts >= MAX_ATTEMPTS;
        if capped && kind != Kind::Markdown {
            return self.record_destination_state(
                batch,
                publication,
                None,
                json!({"kind": Pending::AttemptLimit.kind()}),
            );
        }
        let mapped = mapped_items.get(&item.item_id).map(String::as_str);
        let retry_prewrite = kind == Kind::Markdown
            && publication.provider_state_id.as_ref().is_some_and(|id| {
                self.cas.get_json(id).is_ok_and(|saved| {
                    saved["schema"] == "af/backlog-configured-state@2"
                        && saved["attempt_id"].as_str()
                            == publication.attempt_ids.last().map(String::as_str)
                        && saved["outcome"]["kind"] == Pending::PrewriteConflict.kind()
                })
            });
        let may_append = !capped && (publication.attempt_ids.is_empty() || retry_prewrite);
        let operation = if mapped.is_none() && may_append {
            Operation::Create
        } else {
            Operation::Reconcile
        };
        let journal = |store: &mut Self, publication: &mut Publication| {
            live(store)?;
            publication.attempt_ids.push(value_id(&(
                "af/backlog-configured-attempt@1",
                &publication.intent_id,
                previous_attempts,
                operation,
            ))?);
            store.record_destination_state(
                batch,
                publication,
                Some(operation),
                json!({"kind": "attempt_prepared"}),
            )?;
            live(store)
        };
        let delivery = |publication| Delivery {
            publication,
            item,
            mapped,
            operation,
            cancellation,
            deadline: proof.deadline,
        };
        let outcome = match kind {
            Kind::Markdown => {
                let config = markdown::Config::load(self, &destination)?;
                if may_append {
                    journal(self, publication)?;
                }
                config.deliver(self, delivery(publication), markdown_history, may_append)
            }
            Kind::Jira => {
                let config = jira::Config::load(self, &destination)?;
                match config.prepare(self) {
                    Ok(adapter) => {
                        journal(self, publication)?;
                        adapter.deliver(self, delivery(publication))
                    }
                    Err(reason) => Err(reason),
                }
            }
            _ => return Err("Invalid configured backlog destination".into()),
        };
        if kind == Kind::Markdown {
            live(self)?;
        }
        let approved = value_id(&(&item.public.title, &item.public.body))?;
        let outcome = match outcome {
            Ok(observation)
                if (kind != Kind::Markdown || observation.fields_id == approved)
                    && mapped.is_none_or(|expected| expected == observation.remote_id)
                    && !mapped_items.iter().any(|(other, remote)| {
                        other != &item.item_id && remote == &observation.remote_id
                    }) =>
            {
                publication.remote_id = Some(observation.remote_id.clone());
                json!({
                    "kind": "observed", "observation": observation,
                    "approved_fields_match": observation.fields_id == approved,
                })
            }
            Ok(_) => json!({"kind": Pending::Conflict.kind()}),
            Err(reason) => json!({"kind": reason.kind()}),
        };
        let operation = (kind == Kind::Markdown
            || publication.attempt_ids.len() > previous_attempts)
            .then_some(operation);
        let observed_markdown = kind == Kind::Markdown && outcome["kind"] == "observed";
        match self.record_destination_state(batch, publication, operation, outcome) {
            // The file may already contain the entry. Resume from the durable
            // attempt and observe it again if the receipt could not be persisted.
            Err(_) if observed_markdown => Ok(false),
            result => result,
        }
    }

    fn record_destination_state(
        &mut self,
        batch: &str,
        publication: &mut Publication,
        operation: Option<Operation>,
        outcome: serde_json::Value,
    ) -> Result<bool, String> {
        let state = self
            .cas
            .put_json(&json!({
                "schema": "af/backlog-configured-state@2",
                "attempt_id": publication.attempt_ids.last(),
                "operation": operation,
                "outcome": outcome,
            }))
            .map_err(|error| error.to_string())?;
        let resolved = publication.remote_id.is_some();
        if publication.provider_state_id.as_ref() != Some(&state) {
            publication.provider_state_id = Some(state);
            if resolved {
                publication.receipt_id = Some(value_id(&(
                    "af/backlog-configured-receipt@2",
                    &publication,
                ))?);
            }
            self.append_processing(
                batch,
                vec![BacklogRecordV2::Publication(publication.clone())],
            )?;
        }
        Ok(resolved)
    }
}
