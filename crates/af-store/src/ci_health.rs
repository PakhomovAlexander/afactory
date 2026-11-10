//! Deterministic CI identities over canonical JSON tuples, shared by native admission.
use af_contracts::ci_health::{FINGERPRINT_RULE, Snapshot};
use sha2::{Digest, Sha256};
fn hash(value: &impl serde::Serialize) -> Result<String, String> {
    let bytes = crate::canonicalize(&serde_json::to_value(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "sha256:{}",
        af_contracts::hex::encode(&Sha256::digest(bytes))
    ))
}
pub fn validate_snapshot(snapshot: &Snapshot) -> Result<(), String> {
    snapshot.validate()?;
    for f in &snapshot.failures {
        let occurrence = hash(&(
            &snapshot.repository,
            f.workflow_id,
            f.run_id,
            f.run_attempt,
            f.job_id,
            &f.head_sha,
        ))?;
        let signature = if let Some(fingerprint) = &f.normalized_fingerprint {
            hash(&(
                FINGERPRINT_RULE,
                &snapshot.repository,
                &f.workflow_file,
                &f.job_name,
                &f.platform,
                fingerprint,
            ))?
        } else {
            hash(&(
                FINGERPRINT_RULE,
                &snapshot.repository,
                "occurrence-only",
                &occurrence,
            ))?
        };
        if f.occurrence_id != occurrence || f.signature_id != signature {
            return Err("CI occurrence or signature identity mismatch".into());
        }
    }
    Ok(())
}

/// The selected occurrence is captured before the Worker runs, beside the exact snapshot.
pub fn validate_selection(
    cas: &crate::Cas,
    inputs: &std::collections::BTreeMap<String, af_contracts::task::ArtifactInputV1>,
) -> Result<(String, Snapshot, af_contracts::ci_health::Selection), String> {
    use af_contracts::ci_health::{SNAPSHOT_TYPE, Selection};
    let snapshots: Vec<_> = inputs
        .values()
        .filter(|p| p.artifact_type == SNAPSHOT_TYPE)
        .collect();
    if snapshots.len() != 1
        || snapshots[0].cardinality != af_contracts::PortCardinality::One
        || snapshots[0].artifact_ids.len() != 1
        || snapshots[0].snapshot_id.is_some()
    {
        return Err("CI diagnosis requires one exact unbound CI health snapshot input".into());
    }
    let id = &snapshots[0].artifact_ids[0];
    let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
    if envelope.artifact_type != SNAPSHOT_TYPE || envelope.subject_snapshot_id.is_some() {
        return Err("CI diagnosis snapshot envelope type or affinity differs".into());
    }
    let snapshot: Snapshot = serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
    validate_snapshot(&snapshot)?;
    let requirements = inputs
        .get("requirements")
        .ok_or("CI diagnosis lacks its captured selection")?;
    if requirements.artifact_type != "af/Requirements@1"
        || requirements.artifact_ids.len() != 1
        || requirements.cardinality != af_contracts::PortCardinality::One
        || requirements.snapshot_id.is_some()
    {
        return Err("CI diagnosis requires exact native Requirements".into());
    }
    let envelope = cas
        .get_artifact(&requirements.artifact_ids[0])
        .map_err(|e| e.to_string())?;
    if envelope.artifact_type != "af/Requirements@1" || envelope.subject_snapshot_id.is_some() {
        return Err("CI diagnosis selection Requirements envelope differs".into());
    }
    let selection: Selection = serde_json::from_value(
        envelope
            .payload
            .get("specification")
            .ok_or("CI diagnosis lacks its versioned selection")?
            .clone(),
    )
    .map_err(|e| e.to_string())?;
    selection.validate(&snapshot)?;
    Ok((id.clone(), snapshot, selection))
}
/// Check both immutable input selection and output evidence before admission, acceptance,
/// replay and native Scenario capture. Another occurrence of the same signature is not enough.
pub fn validate_diagnosis(
    cas: &crate::Cas,
    payload: &serde_json::Value,
    inputs: &std::collections::BTreeMap<String, af_contracts::task::ArtifactInputV1>,
) -> Result<af_contracts::ci_health::Diagnosis, String> {
    let diagnosis: af_contracts::ci_health::Diagnosis =
        serde_json::from_value(payload.clone()).map_err(|e| e.to_string())?;
    let (id, snapshot, selection) = validate_selection(cas, inputs)?;
    diagnosis.validate_against(&id, &snapshot)?;
    if diagnosis.occurrence_id != selection.occurrence_id
        || diagnosis.signature_id != selection.signature_id
        || diagnosis.source_sha != selection.source_sha
    {
        return Err("CI diagnosis differs from the Task's exact selected occurrence".into());
    }
    Ok(diagnosis)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ci_health_python_fixture_and_native_occurrence_signature_identity_are_identical() {
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let mut snapshot: Snapshot = serde_json::from_slice(
            &std::fs::read(root.join("fixtures/scenarios/maintenance-ci-health/snapshot.json"))
                .unwrap(),
        )
        .unwrap();
        validate_snapshot(&snapshot).unwrap();
        for field in [
            "workflow",
            "attempt",
            "job",
            "head",
            "platform",
            "fingerprint",
        ] {
            let mut changed = snapshot.clone();
            match field {
                "workflow" => changed.failures[0].workflow_file.push_str(".changed"),
                "attempt" => changed.failures[0].run_attempt += 1,
                "job" => changed.failures[0].job_id += 1,
                "head" => changed.failures[0].head_sha = "f".repeat(40),
                "platform" => changed.failures[0].platform = "macos".into(),
                _ => changed.failures[0].normalized_fingerprint = Some("other test".into()),
            }
            assert!(validate_snapshot(&changed).is_err(), "{field}");
        }
        snapshot.failures[0].normalized_fingerprint = None;
        snapshot.failures[0].signature_id = hash(&(
            FINGERPRINT_RULE,
            &snapshot.repository,
            "occurrence-only",
            &snapshot.failures[0].occurrence_id,
        ))
        .unwrap();
        validate_snapshot(&snapshot).unwrap();
    }
    #[test]
    fn ci_health_diagnosis_reads_one_exact_cas_snapshot_and_refuses_foreign_metadata() {
        use af_contracts::ci_health::{Category, Diagnosis, Evidence};
        use af_contracts::task::ArtifactInputV1;
        use af_contracts::{PortCardinality, Producer};
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let mut payload: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("fixtures/scenarios/maintenance-ci-health/snapshot.json"))
                .unwrap(),
        )
        .unwrap();
        // Main and the release job deliberately differ; CAS admission must retain the
        // occurrence's revision through selection, evidence and same-signature siblings.
        payload["source_sha"] = serde_json::json!("b".repeat(40));
        payload["release_pr"] =
            serde_json::json!({"number":7,"head_sha":payload["failures"][0]["head_sha"]});
        payload["failures"][0]["release_blocking"] = serde_json::json!(true);
        let snapshot: Snapshot = serde_json::from_value(payload.clone()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let cas = crate::Cas::open(directory.path().join("cas")).unwrap();
        let id = cas
            .put_artifact(
                af_contracts::ci_health::SNAPSHOT_TYPE,
                Producer::KernelOperation {
                    run_id: "ci-fixture".into(),
                    node_id: None,
                    operation_id: "ci-observe-v1".into(),
                },
                vec![],
                None,
                payload,
            )
            .unwrap()
            .0;
        let diagnosis = Diagnosis {
            schema: af_contracts::ci_health::DIAGNOSIS_SCHEMA.into(),
            snapshot_id: id.clone(),
            source_artifact_ids: vec![id.clone()],
            occurrence_id: snapshot.failures[0].occurrence_id.clone(),
            signature_id: snapshot.failures[0].signature_id.clone(),
            source_sha: snapshot.failures[0].head_sha.clone(),
            category: Category::ProductBug,
            rationale: "exact evidence".into(),
            evidence: vec![Evidence {
                snapshot_id: id.clone(),
                source_sha: snapshot.failures[0].head_sha.clone(),
                reference: "snapshot".into(),
            }],
        };
        let input = ArtifactInputV1 {
            artifact_type: af_contracts::ci_health::SNAPSHOT_TYPE.into(),
            artifact_ids: vec![id],
            cardinality: PortCardinality::One,
            snapshot_id: None,
        };
        let selection = af_contracts::ci_health::Selection {
            schema: "af.ci-diagnosis-selection/1".into(),
            occurrence_id: diagnosis.occurrence_id.clone(),
            signature_id: diagnosis.signature_id.clone(),
            source_sha: diagnosis.source_sha.clone(),
        };
        let requirement_id=cas.put_artifact("af/Requirements@1", Producer::KernelOperation {
            run_id:"ci-fixture".into(),node_id:None,operation_id:"capture-requirements".into()},vec![],None,
            serde_json::json!({"text":"Diagnose selected occurrence","specification":selection})).unwrap().0;
        let inputs = std::collections::BTreeMap::from([
            ("snapshot".into(), input.clone()),
            (
                "requirements".into(),
                ArtifactInputV1 {
                    artifact_type: "af/Requirements@1".into(),
                    artifact_ids: vec![requirement_id],
                    cardinality: PortCardinality::One,
                    snapshot_id: None,
                },
            ),
        ]);
        let value = serde_json::to_value(&diagnosis).unwrap();
        assert_eq!(
            validate_diagnosis(&cas, &value, &inputs).unwrap(),
            diagnosis
        );
        for change in ["absent", "many", "affinity", "foreign"] {
            let mut bad = inputs.clone();
            match change {
                "absent" => {
                    bad.clear();
                }
                "many" => {
                    bad.insert("other".into(), input.clone());
                }
                "affinity" => {
                    bad.get_mut("snapshot").unwrap().snapshot_id = Some(hash(&"source").unwrap());
                }
                _ => {
                    bad.get_mut("snapshot").unwrap().artifact_ids = vec![hash(&"foreign").unwrap()];
                }
            }
            assert!(validate_diagnosis(&cas, &value, &bad).is_err(), "{change}");
        }
        let mut bad = value.clone();
        bad["signature_id"] = serde_json::json!(hash(&"foreign").unwrap());
        assert!(validate_diagnosis(&cas, &bad, &inputs).is_err());
        let mut wrong_revision = value.clone();
        wrong_revision["source_sha"] = serde_json::json!(snapshot.source_sha);
        wrong_revision["evidence"][0]["source_sha"] = serde_json::json!(snapshot.source_sha);
        assert!(validate_diagnosis(&cas, &wrong_revision, &inputs).is_err());

        // A valid answer about a sibling occurrence of the same signature is still refused.
        let mut two = snapshot.clone();
        let mut second = two.failures[0].clone();
        second.run_attempt += 1;
        second.job_id += 1;
        second.occurrence_id = hash(&(
            &two.repository,
            second.workflow_id,
            second.run_id,
            second.run_attempt,
            second.job_id,
            &second.head_sha,
        ))
        .unwrap();
        two.failures.push(second.clone());
        validate_snapshot(&two).unwrap();
        let producer = Producer::KernelOperation {
            run_id: "ci-fixture".into(),
            node_id: None,
            operation_id: "capture-two-occurrences".into(),
        };
        let two_id = cas
            .put_artifact(
                af_contracts::ci_health::SNAPSHOT_TYPE,
                producer.clone(),
                vec![],
                None,
                serde_json::to_value(&two).unwrap(),
            )
            .unwrap()
            .0;
        let selection_id = cas
            .put_artifact(
                "af/Requirements@1",
                producer,
                vec![],
                None,
                serde_json::json!({"text":"Diagnose second occurrence","specification":{
                "schema":"af.ci-diagnosis-selection/1","occurrence_id":second.occurrence_id,
                "signature_id":second.signature_id,"source_sha":second.head_sha}}),
            )
            .unwrap()
            .0;
        let mut selected = inputs.clone();
        selected.get_mut("snapshot").unwrap().artifact_ids = vec![two_id.clone()];
        selected.get_mut("requirements").unwrap().artifact_ids = vec![selection_id];
        let mut answer = diagnosis.clone();
        answer.snapshot_id = two_id.clone();
        answer.source_artifact_ids = vec![two_id.clone()];
        answer.evidence[0].snapshot_id = two_id;
        assert!(
            validate_diagnosis(&cas, &serde_json::to_value(&answer).unwrap(), &selected).is_err()
        );
        answer.occurrence_id = second.occurrence_id;
        validate_diagnosis(&cas, &serde_json::to_value(answer).unwrap(), &selected).unwrap();
    }
}
