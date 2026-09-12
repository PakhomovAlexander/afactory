use super::*;
use review_core::task::delivery::*;

impl TaskProjection {
    pub(super) fn apply_delivery(&mut self, cas: &Cas, id: &str) -> Result<(), StoreError> {
        let value: TaskDeliveryRecordV1 = payload(cas, id, TASK_DELIVERY_RECORD_V1)?;
        value.validate().map_err(conflict)?;
        let TaskPhaseV1::Finished { result_id } = &self.phase else {
            return Err(conflict("Delivery requires a finished Task"));
        };
        let result: TaskResultV1 = payload(cas, result_id, task::TASK_RESULT_V1)?;
        let snapshot = |ports: &BTreeMap<String, task::ArtifactInputV1>, name: &str| {
            ports
                .get(name)
                .filter(|v| v.artifact_type == "af/SourceTree@1" && v.artifact_ids.len() == 1)
                .and_then(|v| v.snapshot_id.clone())
        };
        if result.acceptance != TaskAcceptanceV1::Satisfied
            || value.task_id != self.task_id
            || value.result_id != *result_id
            || snapshot(&self.revision.inputs, "source").as_ref() != Some(&value.source_snapshot_id)
            || snapshot(&result.outputs, "snapshot").as_ref() != Some(&value.derived_snapshot_id)
        {
            return Err(conflict(
                "Delivery differs from the exact verified Task result",
            ));
        }
        let receipt = cas
            .get_json(&value.receipt_id)
            .map_err(|e| conflict(e.to_string()))?;
        let target = cas
            .get_json(&value.target_id)
            .map_err(|e| conflict(e.to_string()))?;
        let expected_schema = if value.status == TaskDeliveryStatusV1::Prepared {
            "af/task-delivery-prepared@1"
        } else {
            "af/task-delivery@1"
        };
        let expected_outcome = match value.status {
            TaskDeliveryStatusV1::Prepared => None,
            TaskDeliveryStatusV1::Delivered => Some("delivered"),
            TaskDeliveryStatusV1::Failed => Some("failed"),
        };
        if receipt["schema"] != expected_schema
            || receipt["task_id"] != value.task_id
            || receipt
                .get("result_id")
                .is_some_and(|id| id.as_str() != Some(value.result_id.as_str()))
            || receipt["source_snapshot_id"] != value.source_snapshot_id
            || receipt["derived_snapshot_id"] != value.derived_snapshot_id
            || receipt.get("target") != Some(&target)
            || receipt
                .pointer("/outcome/kind")
                .and_then(serde_json::Value::as_str)
                != expected_outcome
            || (expected_outcome.is_some() && receipt.get("remote_actions") != Some(&json!([])))
        {
            return Err(conflict(
                "Delivery receipt contradicts its recorded identity or local scope",
            ));
        }
        let previous = self
            .deliveries
            .iter()
            .rev()
            .find(|(_, previous)| previous.result_id == value.result_id);
        match (previous, value.status) {
            (None, TaskDeliveryStatusV1::Prepared) => {}
            (Some((_, previous)), TaskDeliveryStatusV1::Prepared)
                if previous.status == TaskDeliveryStatusV1::Failed => {}
            (
                Some((_, previous)),
                TaskDeliveryStatusV1::Delivered | TaskDeliveryStatusV1::Failed,
            ) if previous.status == TaskDeliveryStatusV1::Prepared
                && previous.target_id == value.target_id =>
            {
                let prepared = cas
                    .get_json(&previous.receipt_id)
                    .map_err(|e| conflict(e.to_string()))?;
                if receipt["delivery_id"] != prepared["delivery_id"]
                    || receipt.get("result_id") != prepared.get("result_id")
                {
                    return Err(conflict(
                        "Delivery terminal receipt changed prepared operation",
                    ));
                }
            }
            _ => {
                return Err(conflict(
                    "Delivery transition has no matching prepared operation",
                ));
            }
        }
        self.deliveries.push((id.into(), value));
        Ok(())
    }
}

impl EventStore {
    pub fn record_task_delivery(
        &mut self,
        cas: &Cas,
        lease: &TaskLease,
        record_id: &str,
    ) -> Result<RunEvent, StoreError> {
        self.task_change(
            cas,
            lease,
            TaskChangeV1::DeliveryRecorded {
                record_id: record_id.into(),
            },
            now()?,
        )
    }
}
