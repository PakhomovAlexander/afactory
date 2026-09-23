//! The common Task Store journal behind the local delivery transaction and its recovery.
use super::*;
use review_core::task::delivery::*;
use review_core::task::optimization_light::{
    OPTIMIZATION_ADOPTION_RECEIPT_V1, OPTIMIZATION_RESULT_V1, OptimizationAdoptionReceiptV1,
};
use review_core::task::{TASK_RESULT_V1, TaskAcceptanceV1, TaskPhaseV1, TaskResultV1};
use review_store::EventStore;
use review_store::store::task::{TaskLease, TaskProjection};
use std::sync::{Arc, Mutex, mpsc};

pub(super) struct CommonDelivery {
    store: Arc<Mutex<EventStore>>,
    cas: Cas,
    lease: TaskLease,
    result_id: String,
    stop: mpsc::Sender<()>,
    heartbeat: Option<std::thread::JoinHandle<Result<(), String>>>,
    optimization_adoption: Option<OptimizationAdoptionContext>,
    prepared_delivery_record_id: Option<String>,
}

struct OptimizationAdoptionContext {
    task_id: String,
    optimization_result_id: String,
    source_snapshot_id: String,
    delivered_snapshot_id: String,
    delivered_tree_id: String,
}

impl CommonDelivery {
    pub(super) fn open(state: &Path, task: &TaskProjection) -> Result<Self, String> {
        let TaskPhaseV1::Finished { result_id } = &task.phase else {
            return Err("Task has no finished result".into());
        };
        let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
        let heartbeat_cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
        let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
        let lease = store
            .take_task_lease(
                &cas,
                &task.task_id,
                &format!("delivery-{}", std::process::id()),
                15_000,
            )
            .map_err(|e| e.to_string())?;
        let current = store
            .task_projection(&cas, &task.task_id)
            .map_err(|e| e.to_string())?
            .ok_or("Unknown Task")?;
        if current.revision_id != task.revision_id || current.phase != task.phase {
            let _ = store.release_task_lease(&cas, &lease);
            return Err("Task result changed before delivery acquired its lease".into());
        }
        let optimization_adoption = if current.revision.kind == "optimize" {
            let result: TaskResultV1 = {
                let envelope = cas.get_artifact(result_id).map_err(|e| e.to_string())?;
                serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?
            };
            let optimization_result_id = result
                .outputs
                .get("result")
                .filter(|port| {
                    port.artifact_type == OPTIMIZATION_RESULT_V1 && port.artifact_ids.len() == 1
                })
                .and_then(|port| port.artifact_ids.first())
                .cloned();
            let source_snapshot_id = current
                .revision
                .inputs
                .get("source")
                .and_then(|port| port.snapshot_id.clone());
            let delivered_snapshot_id = result
                .outputs
                .get("snapshot")
                .and_then(|port| port.snapshot_id.clone());
            match (
                optimization_result_id,
                source_snapshot_id,
                delivered_snapshot_id,
            ) {
                (
                    Some(optimization_result_id),
                    Some(source_snapshot_id),
                    Some(delivered_snapshot_id),
                ) => {
                    let (snapshot, _) =
                        review_source_git::task::read_snapshot(&cas, &delivered_snapshot_id)?;
                    Some(OptimizationAdoptionContext {
                        task_id: current.task_id.clone(),
                        optimization_result_id,
                        source_snapshot_id,
                        delivered_snapshot_id,
                        delivered_tree_id: snapshot.content_digest,
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        let prepared_delivery_record_id = current
            .deliveries
            .iter()
            .rev()
            .find(|(_, record)| {
                record.result_id == *result_id && record.status == TaskDeliveryStatusV1::Prepared
            })
            .map(|(id, _)| id.clone());
        let store = Arc::new(Mutex::new(store));
        let heartbeat_store = Arc::clone(&store);
        let heartbeat_lease = lease.clone();
        let (stop, stopped) = mpsc::channel();
        let heartbeat = std::thread::spawn(move || -> Result<(), String> {
            loop {
                match stopped.recv_timeout(Duration::from_secs(1)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let mut store = heartbeat_store.lock().expect("Delivery Store");
                heartbeat_tick(&mut store, &heartbeat_cas, &heartbeat_lease)?;
            }
        });
        Ok(Self {
            store,
            cas,
            lease,
            result_id: result_id.clone(),
            stop,
            heartbeat: Some(heartbeat),
            optimization_adoption,
            prepared_delivery_record_id,
        })
    }
}

fn heartbeat_tick(store: &mut EventStore, cas: &Cas, lease: &TaskLease) -> Result<(), String> {
    let lease_until = store.task_lease_state(lease).map_err(|e| e.to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64;
    if lease_until < now.saturating_add(10_000) {
        store
            .renew_task_lease(cas, lease, 15_000)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Drop for CommonDelivery {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.heartbeat.take() {
            let _ = thread.join();
        }
        let _ = self
            .store
            .lock()
            .expect("Delivery Store")
            .release_task_lease(&self.cas, &self.lease);
    }
}

impl CommonDelivery {
    /// Record one delivery transition, whose receipt is already in the CAS, in the Task log.
    pub(super) fn append(
        &mut self,
        cas: &Cas,
        task_id: &str,
        event: &str,
        artifact: &str,
    ) -> Result<(), String> {
        let receipt = cas.get_json(artifact).map_err(|e| e.to_string())?;
        let status = match event {
            "TaskDeliveryPrepared@1" => TaskDeliveryStatusV1::Prepared,
            "TaskDelivered@1" => TaskDeliveryStatusV1::Delivered,
            "TaskDeliveryFailed@1" => TaskDeliveryStatusV1::Failed,
            _ => return Err("Unknown Task delivery transition".into()),
        };
        let value = TaskDeliveryRecordV1 {
            task_id: task_id.into(),
            result_id: self.result_id.clone(),
            status,
            source_snapshot_id: receipt["source_snapshot_id"]
                .as_str()
                .ok_or("Missing delivery source")?
                .into(),
            derived_snapshot_id: receipt["derived_snapshot_id"]
                .as_str()
                .ok_or("Missing delivery output")?
                .into(),
            target_id: cas
                .put_json(receipt.get("target").ok_or("Missing delivery target")?)
                .map_err(|e| e.to_string())?,
            receipt_id: artifact.into(),
        };
        value.validate()?;
        let adoption_id = if status == TaskDeliveryStatusV1::Delivered {
            let context = self.optimization_adoption.as_ref();
            let prepared = self.prepared_delivery_record_id.as_ref();
            match (context, prepared) {
                (Some(context), Some(prepared)) => {
                    let adoption = OptimizationAdoptionReceiptV1 {
                        schema: "af.optimization-adoption-receipt/1".into(),
                        task_id: context.task_id.clone(),
                        result_id: context.optimization_result_id.clone(),
                        source_snapshot_id: context.source_snapshot_id.clone(),
                        delivered_snapshot_id: context.delivered_snapshot_id.clone(),
                        delivery_record_id: prepared.clone(),
                        delivered_tree_id: context.delivered_tree_id.clone(),
                    };
                    adoption.validate()?;
                    Some(
                        cas.put_artifact(
                            OPTIMIZATION_ADOPTION_RECEIPT_V1,
                            review_core::Producer::KernelOperation {
                                run_id: review_store::store::task::task_run_id(task_id)
                                    .map_err(|e| e.to_string())?,
                                node_id: None,
                                operation_id: "optimization-adoption-delivery-v1".into(),
                            },
                            vec![
                                adoption.result_id.clone(),
                                adoption.source_snapshot_id.clone(),
                                adoption.delivered_snapshot_id.clone(),
                                adoption.delivery_record_id.clone(),
                                adoption.delivered_tree_id.clone(),
                            ],
                            None,
                            serde_json::to_value(adoption).map_err(|e| e.to_string())?,
                        )
                        .map_err(|e| e.to_string())?
                        .0,
                    )
                }
                _ => None,
            }
        } else {
            None
        };
        let mut refs = value
            .references()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        refs.extend(adoption_id);
        let id = cas
            .put_artifact(
                TASK_DELIVERY_RECORD_V1,
                review_core::Producer::KernelOperation {
                    run_id: review_store::store::task::task_run_id(task_id)
                        .map_err(|e| e.to_string())?,
                    node_id: None,
                    operation_id: "local-delivery@1".into(),
                },
                refs,
                None,
                serde_json::to_value(value).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        self.store
            .lock()
            .expect("Delivery Store")
            .record_task_delivery(cas, &self.lease, &id)
            .map_err(|e| e.to_string())?;
        if status == TaskDeliveryStatusV1::Prepared {
            self.prepared_delivery_record_id = Some(id);
        }
        Ok(())
    }
}

pub(super) fn projection(
    state: &Path,
    cas: &Cas,
    id: &str,
) -> Result<Option<TaskProjection>, String> {
    if !state.join("events.sqlite").is_file() {
        return Ok(None);
    }
    EventStore::open_read_only(state.join("events.sqlite"))
        .map_err(|e| e.to_string())?
        .task_projection(cas, id)
        .map_err(|e| e.to_string())
}

pub(super) fn events(task: &TaskProjection) -> Vec<TaskEvent> {
    let TaskPhaseV1::Finished { result_id } = &task.phase else {
        return vec![];
    };
    task.deliveries
        .iter()
        .filter(|(_, value)| &value.result_id == result_id)
        .map(|(_, value)| TaskEvent {
            event_type: match value.status {
                TaskDeliveryStatusV1::Prepared => "TaskDeliveryPrepared@1",
                TaskDeliveryStatusV1::Delivered => "TaskDelivered@1",
                TaskDeliveryStatusV1::Failed => "TaskDeliveryFailed@1",
            }
            .into(),
            artifact_id: value.receipt_id.clone(),
        })
        .collect()
}

/// Where *this* Task's source was bound from, for the one sentence a re-rooted source earns.
///
/// The current revision's `af/TaskInputBindings@1` record answers it, because a parentless
/// generation-2 Snapshot is carried verbatim when it is re-bound: its origin still names the
/// Task that re-rooted it first, which is not what this Task's file referenced. The origin's
/// `bound_from` remains the answer for a revision that records no binding — a Task planned
/// before the record existed, or one whose source came from somewhere this Store no longer
/// projects (ADR-0114).
fn bound_source(
    cas: &Cas,
    task: &TaskProjection,
    origin: &review_source_git::task::TaskSourceOrigin,
) -> Result<Option<review_source_git::task::TaskSourceBoundFromV1>, String> {
    let recorded = crate::task_execution::recorded_input_bindings(cas, &task.revision)?;
    let current = recorded.as_ref().and_then(|r| r.bindings.get("source"));
    if let Some(binding) = current
        && let Some(snapshot_id) = binding.snapshot_id.clone()
    {
        return Ok(Some(review_source_git::task::TaskSourceBoundFromV1 {
            artifact_id: binding.artifact_id.clone(),
            snapshot_id,
            task: binding.task.clone(),
        }));
    }
    Ok(origin.bound_from().cloned())
}

/// The one sentence a re-rooted source earns: what it was bound to, and what to do about it.
fn bound_source_refusal(bound: &review_source_git::task::TaskSourceBoundFromV1) -> String {
    let origin = match &bound.task {
        Some(task) => format!("task {}/{}", task.task_id, task.port),
        None => format!("artifact {}", bound.artifact_id),
    };
    let reason = "the tree has no commit for the target repository's HEAD";
    let remedy = "commit the tree and plan this Task from the commit";
    let head = format!("source was bound to {origin}; {reason} to equal");
    format!("{head}, so {remedy}")
}

pub(super) fn assets(cas: &Cas, task: &TaskProjection) -> Result<DeliveryAssets, String> {
    let TaskPhaseV1::Finished { result_id } = &task.phase else {
        return Err("Task has no completed outcome".into());
    };
    let artifact: review_core::ArtifactEnvelope =
        serde_json::from_value(cas.get_json(result_id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    review_store::validate_envelope(&artifact)?;
    if artifact.artifact_id != *result_id || artifact.artifact_type != TASK_RESULT_V1 {
        return Err("Invalid Task result identity".into());
    }
    let result: TaskResultV1 =
        serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
    if result.acceptance != TaskAcceptanceV1::Satisfied {
        return Err("only a verified Task can be delivered".into());
    }
    review_store::store::task::validate_optimization_delivery(cas, task, &result)
        .map_err(|error| error.to_string())?;
    let source_snapshot_id = task
        .revision
        .inputs
        .get("source")
        .and_then(|s| s.snapshot_id.clone())
        .ok_or("Task has no source Snapshot")?;
    let derived_snapshot_id = result
        .outputs
        .get("snapshot")
        .and_then(|s| s.snapshot_id.clone())
        .ok_or("Task has no verified Snapshot")?;
    let (source, source_manifest) =
        review_source_git::task::read_snapshot(cas, &source_snapshot_id)?;
    let (derived, derived_manifest) =
        review_source_git::task::read_snapshot(cas, &derived_snapshot_id)?;
    if source.parent_snapshot_id.is_some()
        || derived.origin_id != source.origin_id
        || !review_source_git::task::descends_from(cas, &derived_snapshot_id, &source_snapshot_id)?
    {
        return Err("Task delivery requires exact source ancestry".into());
    }
    // Either recorded generation reads here. Generation two describes a re-rooted source and
    // deliberately carries no `source_revision`, so the existing rule "delivery requires a
    // committed source Snapshot" fires on its own — `deliver` turns it into one sentence
    // naming the referenced Task, before the prepared record and any Git mutation (ADR-0114).
    let origin = review_source_git::task::read_origin(cas, &source.origin_id)?;
    if origin.content_digest() != source.content_digest {
        return Err("Unsupported Task source origin".into());
    }
    let repository_id = origin.repository_id().to_owned();
    if repository_id.trim().is_empty() {
        return Err("Task origin has no repository".into());
    }
    // A generation-two origin describes a re-rooted source and deliberately carries no
    // `source_revision`, so "delivery requires a committed source Snapshot" fires here, before
    // any prepared record and any Git mutation, as one sentence naming what the source was
    // bound to (ADR-0117).
    let Some(source_revision) = origin.source_revision().map(str::to_owned) else {
        return Err(match bound_source(cas, task, &origin)? {
            Some(bound) => bound_source_refusal(&bound),
            None => "delivery requires a committed source Snapshot".to_owned(),
        });
    };
    let convert = |snapshot: review_source_git::task::TaskSnapshot| SnapshotReceipt {
        content_digest: snapshot.content_digest,
        manifest_artifact_id: snapshot.manifest_id,
        repository_id: repository_id.clone(),
        source_revision: source_revision.clone(),
    };
    Ok(DeliveryAssets {
        source_snapshot_id,
        derived_snapshot_id,
        source: convert(source),
        derived: convert(derived),
        source_manifest,
        derived_manifest,
    })
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;

    #[test]
    fn delivery_tick_refuses_replaced_writer_before_renewal_threshold() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let mut task: review_core::task::TaskRevisionV1 = serde_json::from_str(include_str!(
            "../../../../fixtures/task-contracts/v1/task-revision.json"
        ))
        .unwrap();
        let policy = cas
            .put_json(&serde_json::json!({"fixture":"delivery lease"}))
            .unwrap();
        task.inputs.clear();
        task.provenance.input_artifact_ids.clear();
        task.provenance.adapter_id = policy.clone();
        task.authority.policy_id = policy.clone();
        task.acceptance.get_mut("checked").unwrap().verifier_policy = policy;
        let revision = cas
            .put_artifact(
                review_core::task::TASK_REVISION_V1,
                review_core::Producer::KernelOperation {
                    run_id: "fixture".into(),
                    node_id: None,
                    operation_id: "capture".into(),
                },
                vec![],
                None,
                serde_json::to_value(&task).unwrap(),
            )
            .unwrap()
            .0;
        let old = store.open_task(&cas, &revision, "old", 60_000).unwrap();
        heartbeat_tick(&mut store, &cas, &old).unwrap();
        store.release_task_lease(&cas, &old).unwrap();
        assert!(heartbeat_tick(&mut store, &cas, &old).is_err());
        let current = store
            .take_task_lease(&cas, &task.task_id, "new", 60_000)
            .unwrap();
        let run = review_store::store::task::task_run_id(&task.task_id).unwrap();
        let prefix = store.replay(&run).unwrap();
        assert!(heartbeat_tick(&mut store, &cas, &old).is_err());
        heartbeat_tick(&mut store, &cas, &current).unwrap();
        assert_eq!(
            store.replay(&run).unwrap(),
            prefix,
            "ticks cannot acquire or renew another writer"
        );
    }
}
