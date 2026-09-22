//! Explicit conversion at the legacy Review boundary. Never infer an artifact's encoding
//! from JSON keys: a legacy payload can itself contain `type`, `producer`, or `payload`.

use review_core::{ArtifactEnvelope, Producer};
use review_store::{Cas, content_id, validate_envelope};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewArtifactCodec {
    Flat { artifact_type: String },
    Envelope { artifact_type: String },
}

impl ReviewArtifactCodec {
    pub fn artifact_type(&self) -> &str {
        match self {
            Self::Flat { artifact_type } | Self::Envelope { artifact_type } => artifact_type,
        }
    }

    /// Lift one exact legacy artifact into a Task port. Existing envelopes keep their own
    /// producer, inputs, Snapshot and identity, including a previous Round's Finding Set.
    /// The producer and Snapshot arguments apply only to a newly created flat wrapper.
    pub fn capture(
        &self,
        cas: &Cas,
        legacy_id: &str,
        producer: Producer,
        snapshot_id: Option<String>,
    ) -> Result<String, String> {
        self.validate()?;
        match self {
            Self::Envelope { .. } => {
                self.read(cas, legacy_id)?;
                Ok(legacy_id.into())
            }
            Self::Flat { artifact_type } => {
                let payload = cas.get_json(legacy_id).map_err(|e| e.to_string())?;
                if !payload.is_object()
                    || content_id(&payload).map_err(|e| e.to_string())? != legacy_id
                {
                    return Err("Flat Review input must be an exact canonical JSON object".into());
                }
                cas.put_artifact(
                    artifact_type,
                    producer,
                    vec![legacy_id.into()],
                    snapshot_id,
                    payload,
                )
                .map(|(id, _)| id)
                .map_err(|e| e.to_string())
            }
        }
    }

    /// Restore the original CAS reference for a legacy operation. This checks both the
    /// wrapper and raw bytes on every use; an ID-shaped payload field grants no provenance.
    pub fn restore(&self, cas: &Cas, task_id: &str) -> Result<String, String> {
        let envelope = self.read(cas, task_id)?;
        match self {
            Self::Envelope { .. } => Ok(task_id.into()),
            Self::Flat { .. } => {
                if envelope.input_artifacts != [envelope.content_id.clone()] {
                    return Err(
                        "Flat Review wrapper must retain exactly its original raw ID".into(),
                    );
                }
                let raw = cas
                    .get_json(&envelope.content_id)
                    .map_err(|e| e.to_string())?;
                if raw != envelope.payload
                    || content_id(&raw).map_err(|e| e.to_string())? != envelope.content_id
                {
                    return Err("Flat Review wrapper differs from its retained raw artifact".into());
                }
                Ok(envelope.content_id)
            }
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !review_core::is_artifact_type(self.artifact_type()) {
            return Err("Review codec requires an exact versioned artifact type".into());
        }
        Ok(())
    }

    fn read(&self, cas: &Cas, id: &str) -> Result<ArtifactEnvelope, String> {
        self.validate()?;
        let envelope: ArtifactEnvelope =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        validate_envelope(&envelope)?;
        if envelope.artifact_id != id || envelope.artifact_type != self.artifact_type() {
            return Err("Review artifact differs from its compiled identity or type".into());
        }
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::contract;
    use serde_json::json;

    fn producer(operation: &str) -> Producer {
        Producer::KernelOperation {
            run_id: "captured-review".into(),
            node_id: Some("generation".into()),
            operation_id: operation.into(),
        }
    }

    #[test]
    fn flat_payload_with_envelope_keys_round_trips_without_shape_inference() {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path()).unwrap();
        let raw = json!({"type": "user data", "payload": {}, "producer": "text"});
        let raw_id = cas.put_json(&raw).unwrap();
        let codec = ReviewArtifactCodec::Flat {
            artifact_type: contract::GATE_DECISION_V1.into(),
        };
        let task_id = codec
            .capture(&cas, &raw_id, producer("lift@1"), None)
            .unwrap();
        assert_ne!(task_id, raw_id);
        assert_eq!(codec.restore(&cas, &task_id).unwrap(), raw_id);
        assert_eq!(
            codec
                .capture(&cas, &raw_id, producer("lift@1"), None)
                .unwrap(),
            task_id
        );
        assert_eq!(cas.get_json(&raw_id).unwrap(), raw);
        // An existing envelope is not a raw payload just because its JSON is an object.
        assert!(
            codec
                .capture(&cas, &task_id, producer("lift@1"), None)
                .is_err()
        );
        drop(cas);
        let reopened = Cas::open_existing(temp.path()).unwrap();
        assert_eq!(codec.restore(&reopened, &task_id).unwrap(), raw_id);
    }

    #[test]
    fn enveloped_history_keeps_its_original_snapshot_producer_and_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path()).unwrap();
        let historical_snapshot = content_id(&json!({"snapshot": 1})).unwrap();
        let current_snapshot = content_id(&json!({"snapshot": 2})).unwrap();
        let (id, original) = cas
            .put_artifact(
                contract::FINDING_SET_V1,
                producer("reduce@1"),
                vec![],
                Some(historical_snapshot),
                json!({"findings": []}),
            )
            .unwrap();
        let codec = ReviewArtifactCodec::Envelope {
            artifact_type: contract::FINDING_SET_V1.into(),
        };
        let bytes = cas.get(&id).unwrap();
        assert_eq!(
            codec
                .capture(&cas, &id, producer("lift@1"), Some(current_snapshot))
                .unwrap(),
            id
        );
        assert_eq!(codec.restore(&cas, &id).unwrap(), id);
        assert_eq!(cas.get(&id).unwrap(), bytes);
        assert_eq!(codec.read(&cas, &id).unwrap(), original);
        assert!(
            codec
                .capture(&cas, &original.content_id, producer("lift@1"), None)
                .is_err()
        );
    }

    #[test]
    fn mismatched_type_missing_raw_and_forged_raw_reference_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path()).unwrap();
        let codec = ReviewArtifactCodec::Flat {
            artifact_type: contract::CHANGE_SET_V1.into(),
        };
        let payload = json!({"prior_findings": []});
        let raw = cas.put_json(&payload).unwrap();
        for references in [vec![], vec![raw.clone(), raw.clone()]] {
            let (id, _) = cas
                .put_artifact(
                    codec.artifact_type(),
                    producer("lift@1"),
                    references,
                    None,
                    payload.clone(),
                )
                .unwrap();
            assert!(codec.restore(&cas, &id).is_err());
        }
        let (wrong_type, _) = cas
            .put_artifact(
                contract::REPORT_SET_V1,
                producer("lift@1"),
                vec![raw.clone()],
                None,
                payload,
            )
            .unwrap();
        assert!(codec.restore(&cas, &wrong_type).is_err());
        let id = codec.capture(&cas, &raw, producer("lift@1"), None).unwrap();
        let path = |id: &str| {
            let hex = id.strip_prefix("sha256:").unwrap();
            temp.path().join("objects").join(&hex[..2]).join(&hex[2..])
        };
        std::fs::remove_file(path(&raw)).unwrap();
        assert!(codec.restore(&cas, &id).is_err());
        // Corrupting the wrapper itself is also detected during CAS identity verification.
        std::fs::write(path(&id), b"{}").unwrap();
        assert!(codec.restore(&cas, &id).is_err());
    }
}
