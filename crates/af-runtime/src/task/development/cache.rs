//! Worker cache carry uses the same CAS, closed Build Cache capture, private clones and Warm
//! Set as Review. There is no machine-wide writable cache or cache-index authority.
use super::*;
use af_contracts::task::ArtifactInputV1;
use af_contracts::{BuildCacheKindV1, BuildCacheV2, WarmSetV3, contract::WARM_SET_V3};
use af_source_git::Manifest;
use serde_json::json;

pub(crate) fn kernel_outputs(request: Option<&WorkerDevelopmentV3>) -> BTreeSet<String> {
    request
        .into_iter()
        .flat_map(|request| request.caches.values())
        .filter_map(|policy| policy.output.clone())
        .collect()
}

fn compatibility(
    cas: &Cas,
    request: &WorkerDevelopmentV3,
    policy: &DevelopmentCachePolicyV1,
    manifest: &Manifest,
    source: &str,
) -> Result<String, String> {
    // Fingerprints hash stored Manifest spellings and content identities, never read arbitrary
    // host files. Missing is a real key value and cannot equal a present empty file.
    let fingerprints: Vec<_> = policy
        .fingerprints
        .iter()
        .map(|path| {
            (
                path,
                manifest
                    .get(path)
                    .map(|entry| (&entry.content, entry.size, &entry.kind)),
            )
        })
        .collect();
    cas.put_json(&json!({"domain":"af.worker-cache-compatibility/1", "request":request,
        "platform":[std::env::consts::OS,std::env::consts::ARCH],"policy":policy,"fingerprints":fingerprints,
        "exact_source":(policy.selection == DevelopmentCacheSelectionV1::ExactSource).then_some(source)})).map_err(|error| error.to_string())
}

pub(crate) fn read_manifest(
    cas: &Cas,
    id: &str,
    limits: &af_contracts::BuildCacheLimitsV1,
) -> Result<Manifest, DevelopmentCacheMissV1> {
    let manifest = manifest_metadata(cas, id, limits)?;
    for entry in &manifest.entries {
        if cas.verify(&entry.content).map_err(miss)? != entry.size {
            return Err(DevelopmentCacheMissV1::Corrupt);
        }
    }
    Ok(manifest)
}

fn manifest_metadata(
    cas: &Cas,
    id: &str,
    limits: &af_contracts::BuildCacheLimitsV1,
) -> Result<Manifest, DevelopmentCacheMissV1> {
    // The envelope has a separate control-plane cap. A Manifest cannot allocate from an
    // arbitrary producer object size; its JSON is bounded by the entry/path budget, capped at
    // the CAS control-plane ceiling before parsing.
    let bound = limits
        .max_entries
        .saturating_mul(limits.max_path_bytes.saturating_add(256))
        .min(16 * 1024 * 1024);
    let bytes = cas.get_bounded(id, bound).map_err(miss)?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| DevelopmentCacheMissV1::Corrupt)?;
    af_sandbox::validate_cache_manifest(BuildCacheKindV1::Worker, &manifest, limits).map_err(
        |error| {
            if matches!(
                error.kind(),
                af_sandbox::CacheErrorKind::LimitExceeded
                    | af_sandbox::CacheErrorKind::CopyLimitExceeded
            ) {
                DevelopmentCacheMissV1::OverBound
            } else {
                DevelopmentCacheMissV1::Corrupt
            }
        },
    )?;
    Ok(manifest)
}
fn miss(error: af_store::CasError) -> DevelopmentCacheMissV1 {
    match error {
        af_store::CasError::NotFound { .. } => DevelopmentCacheMissV1::Missing,
        _ => DevelopmentCacheMissV1::Corrupt,
    }
}

pub(crate) fn select(
    cas: &Cas,
    input: &mut TaskInvocationV1,
    request: &WorkerDevelopmentV3,
    binding: &WorkerDevelopmentBindingV3,
    run_id: &str,
    slot: &str,
) -> Result<(), String> {
    if request.caches.is_empty() {
        return Ok(());
    }
    let (source, _, manifest) = source_snapshot(
        cas,
        input
            .inputs
            .get("source")
            .ok_or("Missing development source")?,
    )?;
    let mut set = WarmSetV3 {
        node: input.node.clone(),
        round: 1,
        source_attempt_id: None,
        notes_artifact_id: None,
        head_delta_artifact_id: None,
        head_delta_dropped: None,
        build_cache_artifact_id: None,
        build_cache_dropped: None,
        workspace: None,
        workspace_id: None,
        development: BTreeMap::new(),
    };
    let mut refs = vec![input.plan_id.clone(), source.clone()];
    for (name, policy) in &request.caches {
        let compatibility_id = compatibility(cas, request, policy, &manifest, &source)?;
        let selected = (|| {
            let (artifact_id, manifest_id) = if let Some(key) = &policy.dependency_source {
                let id = binding
                    .dependency_sources
                    .get(key)
                    .ok_or(DevelopmentCacheMissV1::NoInput)?;
                (id.clone(), id.clone())
            } else {
                let port = policy
                    .input
                    .as_ref()
                    .and_then(|port| input.inputs.get(port))
                    .ok_or(DevelopmentCacheMissV1::NoInput)?;
                if port.artifact_ids.len() != 1
                    || port.artifact_type != af_contracts::contract::BUILD_CACHE_V2
                {
                    return Err(DevelopmentCacheMissV1::Corrupt);
                }
                let id = &port.artifact_ids[0];
                let envelope = cas.get_artifact(id).map_err(miss)?;
                if envelope.artifact_type != af_contracts::contract::BUILD_CACHE_V2
                    || envelope.subject_snapshot_id != port.snapshot_id
                {
                    return Err(DevelopmentCacheMissV1::Corrupt);
                }
                let cache: BuildCacheV2 = serde_json::from_value(envelope.payload)
                    .map_err(|_| DevelopmentCacheMissV1::Corrupt)?;
                cache
                    .validate()
                    .map_err(|_| DevelopmentCacheMissV1::Corrupt)?;
                let provenance = cache
                    .worker
                    .as_ref()
                    .ok_or(DevelopmentCacheMissV1::WrongLineage)?;
                if provenance.run_id != run_id
                    || provenance.slot != slot
                    || provenance.name != *name
                    || provenance.role != policy.role
                    || envelope.producer
                        != (af_contracts::Producer::Attempt {
                            run_id: run_id.into(),
                            node_id: cache.gate_node.clone(),
                            attempt_id: cache.gate_attempt_id.clone().unwrap_or_default(),
                        })
                {
                    return Err(DevelopmentCacheMissV1::WrongLineage);
                }
                if policy.selection == DevelopmentCacheSelectionV1::ExactSource
                    && provenance.output_tree_digest != manifest.content_digest()
                {
                    return Err(DevelopmentCacheMissV1::Incompatible);
                }
                if provenance.compatibility_id != compatibility_id
                    || policy.selection == DevelopmentCacheSelectionV1::ExactSource
                        && envelope.subject_snapshot_id.as_deref() != Some(source.as_str())
                {
                    return Err(DevelopmentCacheMissV1::Incompatible);
                }
                let manifest = read_manifest(cas, &cache.manifest_id, &policy.limits)?;
                if manifest.content_digest() != cache.content_digest
                    || manifest.entries.len() as u64 != cache.entries
                    || manifest.entries.iter().map(|entry| entry.size).sum::<u64>() != cache.bytes
                {
                    return Err(DevelopmentCacheMissV1::Corrupt);
                }
                (id.clone(), cache.manifest_id)
            };
            if policy.dependency_source.is_some() {
                read_manifest(cas, &manifest_id, &policy.limits)?;
            }
            Ok((artifact_id, manifest_id))
        })();
        let record = match selected {
            Ok((artifact_id, manifest_id)) => {
                refs.extend([
                    artifact_id.clone(),
                    manifest_id.clone(),
                    compatibility_id.clone(),
                ]);
                DevelopmentCacheSelectionRecordV1::Selected {
                    artifact_id,
                    manifest_id,
                    compatibility_id,
                }
            }
            Err(reason) => DevelopmentCacheSelectionRecordV1::Omitted {
                reason,
                compatibility_id,
            },
        };
        set.development.insert(name.clone(), record);
    }
    set.validate()?;
    let (id, _) = cas
        .put_artifact(
            WARM_SET_V3,
            af_contracts::Producer::KernelOperation {
                run_id: run_id.into(),
                node_id: Some(input.node.clone()),
                operation_id: "worker-development-warm@1".into(),
            },
            refs,
            Some(source.clone()),
            serde_json::to_value(&set).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    input.inputs.insert(
        DEVELOPMENT_WARM_PORT.into(),
        ArtifactInputV1 {
            artifact_ids: vec![id],
            artifact_type: WARM_SET_V3.into(),
            cardinality: af_contracts::PortCardinality::One,
            snapshot_id: Some(source),
        },
    );
    Ok(())
}

pub(super) fn selection(
    cas: &Cas,
    input: &TaskInvocationV1,
    request: &WorkerDevelopmentV3,
) -> Result<Option<WarmSetV3>, String> {
    if request.caches.is_empty() {
        return Ok(None);
    }
    let port = input
        .inputs
        .get(DEVELOPMENT_WARM_PORT)
        .ok_or("Development cache selection was not recorded before reservation")?;
    if port.artifact_ids.len() != 1 || port.artifact_type != WARM_SET_V3 {
        return Err("Invalid development Warm Set".into());
    }
    let envelope = cas
        .get_artifact(&port.artifact_ids[0])
        .map_err(|error| error.to_string())?;
    if envelope.artifact_type != WARM_SET_V3 {
        return Err("Unsupported development Warm Set version".into());
    }
    let set: WarmSetV3 =
        serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
    set.validate()?;
    if set.node != input.node || !set.development.keys().eq(request.caches.keys()) {
        return Err("Warm Set differs from declared cache policies".into());
    }
    for layer in set.development.values() {
        cas.get_bounded(layer.compatibility_id(), 16 * 1024 * 1024)
            .map_err(|error| {
                format!(
                    "Recorded cache compatibility provenance unavailable: {:?}",
                    miss(error)
                )
            })?;
    }
    Ok(Some(set))
}

/// Revalidate the exact recorded selection. A selected artifact that disappeared after
/// reservation is corruption, not permission to select another cache or run silently cold.
pub(super) fn restore(
    cas: &Cas,
    set: Option<&WarmSetV3>,
    request: &WorkerDevelopmentV3,
    root: DevelopmentRootV1,
    destination: &Path,
    deadline: Option<Instant>,
    cancellation: Option<&AtomicBool>,
) -> Result<bool, String> {
    for (name, policy) in &request.caches {
        if policy.destination != root {
            continue;
        }
        if let Some(DevelopmentCacheSelectionRecordV1::Selected {
            artifact_id,
            manifest_id,
            compatibility_id,
        }) = set.and_then(|set| set.development.get(name))
        {
            cas.get_bounded(compatibility_id, 16 * 1024 * 1024)
                .map_err(|error| {
                    format!(
                        "Recorded cache compatibility provenance unavailable: {:?}",
                        miss(error)
                    )
                })?;
            if policy.role != DevelopmentCacheRoleV1::Dependency {
                let envelope = cas.get_artifact(artifact_id).map_err(|error| {
                    format!(
                        "Recorded development cache {name} unavailable: {:?}",
                        miss(error)
                    )
                })?;
                if envelope.artifact_type != af_contracts::contract::BUILD_CACHE_V2 {
                    return Err("Recorded cache has an unsupported type".into());
                }
                let cache: BuildCacheV2 =
                    serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
                cache.validate()?;
                if cache.manifest_id != *manifest_id
                    || cache
                        .worker
                        .as_ref()
                        .is_none_or(|worker| worker.compatibility_id != *compatibility_id)
                {
                    return Err("Recorded cache differs from its Warm Set".into());
                }
            }
            let manifest =
                manifest_metadata(cas, manifest_id, &policy.limits).map_err(|reason| {
                    format!("Recorded development cache {name} unavailable: {reason:?}")
                })?;
            let target = destination.join(&policy.path);
            if !policy.path.is_empty() {
                std::fs::create_dir_all(target.parent().ok_or("Missing private cache parent")?)
                    .map_err(|error| error.to_string())?;
            }
            af_sandbox::clone_cache_directory_before(
                &manifest,
                &target,
                &policy.limits,
                cas,
                deadline,
                cancellation,
            )
            .map_err(|error| error.to_string())?;
            return Ok(true);
        }
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn capture(
    cas: &Cas,
    input: &TaskInvocationV1,
    attempt_id: &str,
    lineage: (&str, &str),
    request: &WorkerDevelopmentV3,
    runtime: &Path,
    cleanup_confirmed: bool,
    command_succeeded: bool,
    trees: Option<(&str, &Manifest)>,
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<String>, String> {
    let Some(_) = selection(cas, input, request)? else {
        return Ok(vec![]);
    };
    let source = input.inputs["source"]
        .snapshot_id
        .as_deref()
        .ok_or("Missing source identity")?;
    let mut retained = Vec::new();
    for (name, policy) in &request.caches {
        if policy.output.is_none() {
            continue;
        }
        let omission = if !cleanup_confirmed {
            Some(WorkerCacheCaptureOmissionV1::CleanupUnconfirmed)
        } else if super::super::control::check(cancellation).is_err() {
            Some(WorkerCacheCaptureOmissionV1::Cancelled)
        } else if Instant::now() >= deadline {
            Some(WorkerCacheCaptureOmissionV1::Deadline)
        } else if !command_succeeded {
            Some(WorkerCacheCaptureOmissionV1::CommandFailed)
        } else if trees.is_none() {
            Some(WorkerCacheCaptureOmissionV1::CaptureRefused)
        } else {
            None
        };
        let result = if omission.is_none() {
            af_sandbox::capture_cache_directory_before(
                af_contracts::BuildCacheKindV1::Worker,
                &runtime
                    .join(format!("development-{}", policy.destination.name()))
                    .join(&policy.path),
                &policy.limits,
                cas,
                Some(deadline),
                cancellation,
            )
            .ok()
        } else {
            None
        };
        let artifact_id = if let Some(captured) = result {
            // Recheck after capture: deadline/cancellation cannot publish a continuation.
            if Instant::now() >= deadline || super::super::control::check(cancellation).is_err() {
                None
            } else {
                let cache = BuildCacheV2 {
                    kind: BuildCacheKindV1::Worker,
                    trust: af_contracts::BuildCacheTrustV1::CandidateBuilt,
                    gate_node: input.node.clone(),
                    gate_attempt_id: Some(attempt_id.into()),
                    head_snapshot_id: source.into(),
                    manifest_id: captured.manifest_id.clone(),
                    content_digest: captured.content_digest,
                    entries: captured.entries,
                    bytes: captured.bytes,
                    limits: policy.limits,
                    worker: Some(DevelopmentCacheProvenanceV1 {
                        run_id: lineage.0.into(),
                        slot: lineage.1.into(),
                        name: name.clone(),
                        role: policy.role,
                        compatibility_id: compatibility(
                            cas,
                            request,
                            policy,
                            trees.ok_or("Missing producer tree")?.1,
                            source,
                        )?,
                        input_tree_digest: trees.ok_or("Missing input tree")?.0.into(),
                        output_tree_digest: trees
                            .ok_or("Missing producer tree")?
                            .1
                            .content_digest(),
                    }),
                };
                cache.validate()?;
                Some(
                    cas.put_artifact(
                        af_contracts::contract::BUILD_CACHE_V2,
                        af_contracts::Producer::Attempt {
                            run_id: lineage.0.into(),
                            node_id: input.node.clone(),
                            attempt_id: attempt_id.into(),
                        },
                        vec![input.plan_id.clone(), captured.manifest_id, source.into()],
                        None,
                        serde_json::to_value(&cache).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?
                    .0,
                )
            }
        } else {
            None
        };
        let receipt = WorkerCacheCaptureV1 {
            schema: "af.worker-cache-capture/1".into(),
            attempt_id: attempt_id.into(),
            name: name.clone(),
            omission: artifact_id.is_none().then_some(omission.unwrap_or_else(|| {
                if super::super::control::check(cancellation).is_err() {
                    WorkerCacheCaptureOmissionV1::Cancelled
                } else if Instant::now() >= deadline {
                    WorkerCacheCaptureOmissionV1::Deadline
                } else {
                    WorkerCacheCaptureOmissionV1::CaptureRefused
                }
            })),
            artifact_id: artifact_id.clone(),
        };
        receipt.validate()?;
        retained.push(
            cas.put_json(&serde_json::to_value(receipt).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
        );
        retained.extend(artifact_id);
    }
    Ok(retained)
}

pub(crate) fn attach_outputs(
    cas: &Cas,
    input: &TaskInvocationV1,
    attempt_id: &str,
    request: &WorkerDevelopmentV3,
    raw: &[String],
    outputs: &mut BTreeMap<String, ArtifactInputV1>,
) -> Result<(), String> {
    for id in raw {
        let Some(envelope) = cas
            .get_optional_artifact(id)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        if envelope.artifact_type != af_contracts::contract::BUILD_CACHE_V2 {
            continue;
        }
        let mut cache: BuildCacheV2 =
            serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
        cache.validate()?;
        let provenance = cache
            .worker
            .as_ref()
            .ok_or("Missing Worker cache provenance")?;
        if cache.gate_node != input.node || cache.gate_attempt_id.as_deref() != Some(attempt_id) {
            return Err("Captured cache belongs to another Attempt".into());
        }
        let policy = request
            .caches
            .get(&provenance.name)
            .ok_or("Undeclared cache output")?;
        let port = policy.output.as_ref().ok_or("Undeclared cache output")?;
        let (id, snapshot_id) = if policy.selection == DevelopmentCacheSelectionV1::ExactSource {
            // Only the environment's sealed candidate (or unchanged input) may bind a
            // continuation. The command's final scan is evidence, not source authority.
            let source = admitted_source(cas, input, outputs, &envelope.producer)?;
            let (snapshot, _, manifest) = source_snapshot(cas, &source)?;
            if provenance.output_tree_digest != manifest.content_digest() {
                // Reviewer experiments and filtered scratch cannot authorize a continuation
                // against a different admitted tree.
                continue;
            }
            let compatibility_id = compatibility(cas, request, policy, &manifest, &snapshot)?;
            cache
                .worker
                .as_mut()
                .ok_or("Missing Worker cache provenance")?
                .compatibility_id = compatibility_id.clone();
            let mut refs = vec![
                id.clone(),
                cache.head_snapshot_id.clone(),
                cache.manifest_id.clone(),
                compatibility_id,
            ];
            refs.extend(source.artifact_ids);
            let id = cas
                .put_artifact(
                    af_contracts::contract::BUILD_CACHE_V2,
                    envelope.producer,
                    refs,
                    Some(snapshot.clone()),
                    serde_json::to_value(&cache).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?
                .0;
            (id, Some(snapshot))
        } else {
            (id.clone(), None)
        };
        outputs.insert(
            port.clone(),
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: af_contracts::contract::BUILD_CACHE_V2.into(),
                cardinality: af_contracts::PortCardinality::One,
                snapshot_id,
            },
        );
    }
    Ok(())
}

/// Resolve exactly the source the typed seal operation will publish. The Snapshot identity
/// is independent of the publishing operation; candidate bytes and parent are host-sealed.
fn admitted_source(
    cas: &Cas,
    input: &TaskInvocationV1,
    outputs: &BTreeMap<String, ArtifactInputV1>,
    producer: &af_contracts::Producer,
) -> Result<ArtifactInputV1, String> {
    let source = input.inputs.get("source").ok_or("Missing source")?;
    let Some(candidate) = outputs.get("candidate") else {
        return Ok(source.clone());
    };
    if candidate.artifact_type != af_source_git::task::CANDIDATE_TREE_V1
        || candidate.artifact_ids.len() != 1
        || candidate.cardinality != af_contracts::PortCardinality::One
    {
        return Err("Worker cache requires one typed sealed candidate".into());
    }
    let envelope = cas
        .get_artifact(&candidate.artifact_ids[0])
        .map_err(|error| error.to_string())?;
    let tree: af_source_git::task::CandidateTree =
        serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
    if envelope.artifact_type != af_source_git::task::CANDIDATE_TREE_V1
        || &envelope.producer != producer
        || Some(&tree.parent_snapshot_id) != source.snapshot_id.as_ref()
        || Some(&tree.parent_snapshot_id) != candidate.snapshot_id.as_ref()
    {
        return Err("Worker cache candidate/source provenance mismatch".into());
    }
    af_source_git::task::derive_source_tree(
        cas,
        producer.clone(),
        &tree.manifest_id,
        &tree.parent_snapshot_id,
        candidate.artifact_ids.clone(),
    )
}

pub(crate) fn validate_output(
    cas: &Cas,
    input: &TaskInvocationV1,
    port: &str,
    value: &ArtifactInputV1,
    outputs: &BTreeMap<String, ArtifactInputV1>,
    request: &WorkerDevelopmentV3,
    run_id: &str,
) -> Result<(), String> {
    let (name, policy) = request
        .caches
        .iter()
        .find(|(_, policy)| policy.output.as_deref() == Some(port))
        .ok_or("Undeclared cache output")?;
    if value.artifact_type != af_contracts::contract::BUILD_CACHE_V2
        || value.artifact_ids.len() != 1
        || policy.selection != DevelopmentCacheSelectionV1::ExactSource
            && value.snapshot_id.is_some()
    {
        return Err("Invalid kernel cache output".into());
    }
    let envelope = cas
        .get_artifact(&value.artifact_ids[0])
        .map_err(|error| error.to_string())?;
    if envelope.artifact_type != af_contracts::contract::BUILD_CACHE_V2
        || envelope.subject_snapshot_id != value.snapshot_id
    {
        return Err("Unsupported cache version or mismatched output Snapshot".into());
    }
    let cache: BuildCacheV2 =
        serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
    cache.validate()?;
    let provenance = cache
        .worker
        .as_ref()
        .ok_or("Gate cache cannot masquerade as Worker output")?;
    if cache.gate_node != input.node
        || cache.head_snapshot_id
            != input.inputs["source"]
                .snapshot_id
                .clone()
                .unwrap_or_default()
        || provenance.run_id != run_id
        || provenance.name != *name
        || provenance.role != policy.role
        || envelope.producer
            != (af_contracts::Producer::Attempt {
                run_id: run_id.into(),
                node_id: input.node.clone(),
                attempt_id: cache.gate_attempt_id.clone().unwrap_or_default(),
            })
    {
        return Err("Worker cache producer/source provenance mismatch".into());
    }
    if policy.selection == DevelopmentCacheSelectionV1::ExactSource {
        let source = admitted_source(cas, input, outputs, &envelope.producer)?;
        let (snapshot, _, manifest) = source_snapshot(cas, &source)?;
        if envelope.subject_snapshot_id.as_deref() != Some(snapshot.as_str())
            || provenance.compatibility_id
                != compatibility(cas, request, policy, &manifest, &snapshot)?
            || provenance.output_tree_digest != manifest.content_digest()
        {
            return Err("Worker cache admitted output source mismatch".into());
        }
    }
    let manifest = manifest_metadata(cas, &cache.manifest_id, &policy.limits)
        .map_err(|reason| format!("Worker cache output unavailable: {reason:?}"))?;
    if manifest.content_digest() != cache.content_digest
        || manifest.entries.len() as u64 != cache.entries
        || manifest.entries.iter().map(|entry| entry.size).sum::<u64>() != cache.bytes
    {
        return Err("Worker cache manifest identity or accounting mismatch".into());
    }
    Ok(())
}
