//! Pinned, language-independent Worker knowledge companion.
use af_contracts::task::knowledge::*;
use af_contracts::task::pipeline::PortAffinityV1;
use af_graph::task::OperatorSignature;
use std::collections::BTreeMap;

pub fn parse_knowledge(
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Option<KnowledgePolicyV1>, String> {
    let Some(bytes) = files.get(KNOWLEDGE_FILE) else {
        return Ok(None);
    };
    if bytes.len() > CONTROL_BYTES as usize {
        return Err("Knowledge policy exceeds capture bound".into());
    }
    let policy: KnowledgePolicyV1 =
        toml::from_str(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)
            .map_err(|e| crate::excerpt::bounded(&e.to_string()))?;
    policy.validate()?;
    Ok(Some(policy))
}

pub fn validate_ports(
    policy: Option<&KnowledgePolicyV1>,
    signature: &OperatorSignature,
) -> Result<(), String> {
    let inputs = &signature.contract.inputs;
    let outputs = &signature.contract.outputs;
    if outputs
        .values()
        .any(|p| matches!(p.artifact_type.as_str(), INDEX_TYPE | CONTEXT_TYPE))
    {
        return Err("Knowledge indexes and contexts are kernel constructed".into());
    }
    let Some(policy) = policy else {
        if inputs.values().chain(outputs.values()).any(|p| {
            matches!(
                p.artifact_type.as_str(),
                INDEX_TYPE | CONTEXT_TYPE | QUERY_TYPE
            )
        }) {
            return Err("Knowledge ports require a pinned knowledge.toml policy".into());
        }
        return Ok(());
    };
    if !signature.effects.contains("read-source")
        || !inputs.get("source").is_some_and(|p| {
            p.artifact_type == af_source_git::task::SOURCE_TREE_V1
                && !p.optional
                && p.cardinality == af_contracts::PortCardinality::One
        })
    {
        return Err("Knowledge requires declared read-source and one required source input".into());
    }
    let context = inputs
        .get(KNOWLEDGE_PORT)
        .ok_or("Knowledge policy requires the kernel knowledge input")?;
    if context.artifact_type != CONTEXT_TYPE
        || !context.optional
        || context.cardinality != af_contracts::PortCardinality::One
        || context.affinity != (PortAffinityV1::Unbound {})
    {
        return Err("Knowledge input must be one optional unbound KnowledgeContext@1".into());
    }
    for (name, port) in inputs.iter().chain(outputs) {
        if port.artifact_type == QUERY_TYPE
            && (name != QUERY_PORT
                || !port.optional
                || port.cardinality != af_contracts::PortCardinality::One
                || port.affinity != (PortAffinityV1::Unbound {}))
        {
            return Err(
                "Knowledge queries must use the optional unbound knowledge_query port".into(),
            );
        }
        if port.artifact_type == INDEX_TYPE
            || port.artifact_type == CONTEXT_TYPE && name != KNOWLEDGE_PORT
        {
            return Err("Knowledge contexts belong only to the kernel knowledge input".into());
        }
    }
    for (name, port) in outputs {
        if port.artifact_type == QUERY_TYPE
            && (!port.covers.is_empty()
                || signature.evidence.get(name).is_some_and(|v| !v.is_empty())
                || signature.outcome_port.as_ref() == Some(name)
                || signature.worker_output_type.as_deref() == Some(QUERY_TYPE))
        {
            return Err("Knowledge queries cannot claim acceptance or branching authority".into());
        }
    }
    if policy.private_notes == PrivateNotesV1::Disabled
        && inputs
            .values()
            .any(|p| p.artifact_type == af_contracts::task::WORKER_NOTES_V1)
    {
        return Err("Disabled private knowledge cannot declare a Notes input".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::catalog::TaskWorkerManifest;
    fn fixture() -> (
        KnowledgePolicyV1,
        OperatorSignature,
        BTreeMap<String, Vec<u8>>,
    ) {
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/worker-knowledge");
        let files = BTreeMap::from([(
            KNOWLEDGE_FILE.into(),
            std::fs::read(root.join(KNOWLEDGE_FILE)).unwrap(),
        )]);
        let manifest: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(root.join("worker.toml")).unwrap()).unwrap();
        (
            parse_knowledge(&files).unwrap().unwrap(),
            manifest.signature,
            files,
        )
    }
    #[test]
    fn knowledge_companion_is_optional_versioned_bounded_and_pinned_with_the_worker() {
        let (policy, signature, mut files) = fixture();
        validate_ports(Some(&policy), &signature).unwrap();
        assert!(parse_knowledge(&BTreeMap::new()).unwrap().is_none());
        assert!(validate_ports(None, &signature).is_err());
        let pin = crate::lock::package_digest_from_files(&files);
        files
            .get_mut(KNOWLEDGE_FILE)
            .unwrap()
            .extend_from_slice(b"\n# Different captured package bytes\n");
        assert_ne!(crate::lock::package_digest_from_files(&files), pin);
        for invalid in [
            b"schema='af.worker-knowledge/0'".to_vec(),
            vec![b'x'; CONTROL_BYTES as usize + 1],
            b"schema='af.worker-knowledge/1'\nhost_path='/private'".to_vec(),
        ] {
            files.insert(KNOWLEDGE_FILE.into(), invalid);
            assert!(parse_knowledge(&files).is_err());
        }
    }
    #[test]
    fn knowledge_requests_cannot_widen_source_context_or_acceptance_authority() {
        let (policy, signature, _) = fixture();
        let mut no_source = signature.clone();
        no_source.effects.clear();
        assert!(validate_ports(Some(&policy), &no_source).is_err());
        let mut no_context = signature.clone();
        no_context.contract.inputs.remove(KNOWLEDGE_PORT);
        assert!(validate_ports(Some(&policy), &no_context).is_err());
        let mut forged = signature.clone();
        forged
            .contract
            .outputs
            .get_mut("output")
            .unwrap()
            .artifact_type = INDEX_TYPE.into();
        assert!(validate_ports(Some(&policy), &forged).is_err());
        let mut branching = signature.clone();
        branching.outcome_port = Some(QUERY_PORT.into());
        assert!(validate_ports(Some(&policy), &branching).is_err());
        let mut acceptance = signature.clone();
        acceptance.evidence.insert(
            QUERY_PORT.into(),
            std::collections::BTreeSet::from([format!("sha256:{}", "a".repeat(64))]),
        );
        assert!(validate_ports(Some(&policy), &acceptance).is_err());
        let mut wrong_port = signature.clone();
        wrong_port
            .contract
            .inputs
            .get_mut(QUERY_PORT)
            .unwrap()
            .optional = false;
        assert!(validate_ports(Some(&policy), &wrong_port).is_err());
        let mut disabled = policy;
        disabled.private_notes = PrivateNotesV1::Disabled;
        assert!(validate_ports(Some(&disabled), &signature).is_err());
    }
}
