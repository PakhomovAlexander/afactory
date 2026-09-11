//! Journal adapter for the same local delivery transaction and recovery implementation.
use super::*;
use review_core::task::delivery::*;
use review_core::task::{TASK_RESULT_V1, TaskAcceptanceV1, TaskPhaseV1, TaskResultV1};
use review_store::EventStore;
use review_store::store::task::{TaskLease, TaskProjection};
use std::sync::{Arc, Mutex, mpsc};

pub(super) trait DeliveryJournal {
    fn append(
        &mut self,
        cas: &Cas,
        task_id: &str,
        event: &str,
        artifact: &str,
    ) -> Result<(), String>;
}
impl DeliveryJournal for TaskStore {
    fn append(
        &mut self,
        cas: &Cas,
        task_id: &str,
        event: &str,
        artifact: &str,
    ) -> Result<(), String> {
        TaskStore::append(self, cas, task_id, event, artifact)
    }
}

pub(super) struct CommonDelivery {
    store: Arc<Mutex<EventStore>>,
    cas: Cas,
    lease: TaskLease,
    result_id: String,
    stop: mpsc::Sender<()>,
    heartbeat: Option<std::thread::JoinHandle<Result<(), String>>>,
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
                let task = store
                    .task_projection(&heartbeat_cas, heartbeat_lease.task_id())
                    .map_err(|e| e.to_string())?
                    .ok_or("Unknown Task")?;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_millis() as u64;
                if task.lease_until_unix_ms() < now.saturating_add(10_000) {
                    store
                        .renew_task_lease(&heartbeat_cas, &heartbeat_lease, 15_000)
                        .map_err(|e| e.to_string())?;
                }
            }
        });
        Ok(Self {
            store,
            cas,
            lease,
            result_id: result_id.clone(),
            stop,
            heartbeat: Some(heartbeat),
        })
    }
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
impl DeliveryJournal for CommonDelivery {
    fn append(
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
        let id = cas
            .put_artifact(
                TASK_DELIVERY_RECORD_V1,
                review_core::Producer::KernelOperation {
                    run_id: review_store::store::task::task_run_id(task_id)
                        .map_err(|e| e.to_string())?,
                    node_id: None,
                    operation_id: "local-delivery@1".into(),
                },
                value.references().into_iter().map(str::to_owned).collect(),
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
    task.deliveries
        .iter()
        .enumerate()
        .map(|(index, (_, value))| TaskEvent {
            sequence: index as u64 + 1,
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
        || derived.parent_snapshot_id.as_ref() != Some(&source_snapshot_id)
    {
        return Err("Task delivery requires exact source ancestry".into());
    }
    let origin = cas.get_json(&source.origin_id).map_err(|e| e.to_string())?;
    if origin["schema"] != "af.task-source-origin/1"
        || origin["content_digest"] != source.content_digest
    {
        return Err("Unsupported Task source origin".into());
    }
    let repository_id = origin["repository_id"]
        .as_str()
        .ok_or("Task origin has no repository")?
        .to_owned();
    let source_revision = origin["source_revision"]
        .as_str()
        .ok_or("delivery requires a committed source Snapshot")?
        .to_owned();
    let convert = |snapshot: review_source_git::task::TaskSnapshot, kind: &str| SnapshotReceipt {
        schema: "af.task-snapshot/1".into(),
        kind: kind.into(),
        content_digest: snapshot.content_digest,
        manifest_artifact_id: snapshot.manifest_id,
        parent_snapshot_id: snapshot.parent_snapshot_id,
        repository_id: repository_id.clone(),
        source_revision: Some(source_revision.clone()),
    };
    Ok(DeliveryAssets {
        source_snapshot_id,
        derived_snapshot_id,
        source: convert(source, "source"),
        derived: convert(derived, "derived"),
        source_manifest,
        derived_manifest,
    })
}
