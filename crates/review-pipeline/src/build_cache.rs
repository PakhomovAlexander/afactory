//! Worker warm layers, package P2: Build Cache carry from the Gate to Worker sandboxes.
//!
//! The Gate has already built the head before any reviewer is dispatched. After its checks
//! pass, a trusted-local Gate captures each declared cache directory as an explicitly unsafe
//! `review.kernel/BuildCache@1`, records `BuildCacheCaptured@1`, and the Round's Warm Set
//! selection carries the artifact to every reviewer node that declares the same kind. Each
//! Worker sandbox receives a clone from the CAS below the reserved cache root, one environment
//! variable points the build tool at it, and the bytes are removed before seal so they never
//! enter a candidate tree, a Proposal or a delivered worktree.
//!
//! The handoff is admitted only under the trusted-local policy. Config validation refuses a
//! safe pipeline at load; the checks here refuse again at Warm Set selection, before any
//! Attempt is reserved or dispatched, and at every capture and clone, so a hand-edited or
//! replayed binding cannot widen the boundary at runtime.

use review_core::task::runtime::{
    TaskCacheLayerV1, TaskCacheObservationV1, TaskCacheResultV1, TaskRuntimeSpanKindV1,
    TaskRuntimeSpanV1,
};
use review_core::{
    BuildCacheCapturedPayloadV1, BuildCacheDropReasonV1, BuildCacheKindV1,
    BuildCacheRefusalReasonV1, BuildCacheTrustV1, BuildCacheV1, EventType, Producer, RunEvent,
};
use review_sandbox::{CacheEnvironment, CacheErrorKind, Sandbox};
use review_source_git::Manifest;
use review_store::{Cas, NewEvent};

use super::RoundAuthority;
use super::review_domain::ReviewDomainState;
use super::warm::WarmSetRecord;

fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn refusal_reason(kind: CacheErrorKind) -> BuildCacheRefusalReasonV1 {
    match kind {
        CacheErrorKind::UnsafeContent => BuildCacheRefusalReasonV1::UnsafeContent,
        CacheErrorKind::LimitExceeded | CacheErrorKind::CopyLimitExceeded => {
            BuildCacheRefusalReasonV1::LimitExceeded
        }
        CacheErrorKind::SourceUnavailable | CacheErrorKind::PolicyUnavailable => {
            BuildCacheRefusalReasonV1::SourceUnavailable
        }
        CacheErrorKind::ConcurrentChange | CacheErrorKind::MaterializationFailed => {
            BuildCacheRefusalReasonV1::CaptureFailed
        }
    }
}

impl ReviewDomainState<'_> {
    /// Build cache kinds the pinned Gate Execution Binding declares.
    pub(crate) fn gate_build_cache_kinds(&self) -> Vec<BuildCacheKindV1> {
        self.gate_execution
            .as_ref()
            .map(|binding| {
                binding
                    .build_caches
                    .iter()
                    .map(|kind| kind.kind())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The build cache kind a reviewer node's pinned warm policy declares, if any.
    pub(crate) fn node_build_cache_kind(&self, node_id: &str) -> Option<BuildCacheKindV1> {
        self.warm_policy(node_id)
            .and_then(|policy| policy.build_cache.kinds().first().map(|kind| kind.kind()))
    }

    /// The one policy under which a candidate-built cache may travel: a `trusted_local` Gate
    /// binding requiring no isolation. Everything else is the safe policy and is refused.
    pub(crate) fn build_cache_policy_admits(&self) -> Result<(), String> {
        let admitted = self.gate_execution.as_ref().is_some_and(|binding| {
            binding.provider == review_config::SandboxProviderSpec::TrustedLocal
                && binding.required_isolation == review_config::IsolationSpec::None
        });
        if admitted {
            Ok(())
        } else {
            Err(
                "Build Cache handoff refused under the safe policy: a candidate-built cache carries no administrator approval and is admitted only from a `trusted_local` Gate Execution Binding"
                    .into(),
            )
        }
    }

    /// Prepare the empty private directories a trusted-local Gate builds into and the
    /// environment that points its checks at them. Refused before any check runs when the
    /// Gate is not trusted-local.
    pub(crate) fn prepare_gate_build_caches(
        &self,
        node_id: &str,
        sandbox: &Sandbox,
        container_bound: bool,
    ) -> Result<Vec<CacheEnvironment>, String> {
        let kinds = self.gate_build_cache_kinds();
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        if container_bound {
            return Err(
                "Build Cache capture refused: a container Gate cannot declare a candidate-built cache"
                    .into(),
            );
        }
        self.build_cache_policy_admits()?;
        let mut environments = Vec::with_capacity(kinds.len());
        for kind in kinds {
            review_sandbox::prepare_build_cache_root(kind, sandbox).map_err(|error| {
                eprintln!(
                    "build cache diagnostic for Gate `{node_id}`: {}",
                    error.operator_detail()
                );
                error.to_string()
            })?;
            environments.push(review_sandbox::build_cache_environment(
                kind,
                sandbox.root(),
            ));
        }
        Ok(environments)
    }

    /// Capture every declared kind from the Gate sandbox after its checks passed, and record
    /// one `BuildCacheCaptured@1` per kind: the artifact, or the reason the closed layout
    /// refused it. A refusal never changes the Gate verdict.
    pub(crate) fn capture_gate_build_caches(
        &self,
        node_id: &str,
        gate_attempt: Option<&str>,
        sandbox: &Sandbox,
    ) -> Result<(), String> {
        let Some(binding) = self.gate_execution.as_ref() else {
            return Ok(());
        };
        let kinds = self.gate_build_cache_kinds();
        if kinds.is_empty() {
            return Ok(());
        }
        self.build_cache_policy_admits()?;
        let limits = binding.build_cache_limits();
        limits.validate()?;
        for kind in kinds {
            let mut refs = Vec::new();
            let payload =
                match review_sandbox::capture_build_cache(kind, sandbox, &limits, self.cas) {
                    Ok(captured) => {
                        let cache = BuildCacheV1 {
                            kind,
                            trust: BuildCacheTrustV1::CandidateBuilt,
                            gate_node: node_id.to_string(),
                            gate_attempt_id: gate_attempt.map(str::to_owned),
                            head_snapshot_id: self.authority.head_snapshot_id.clone(),
                            manifest_id: captured.manifest_id.clone(),
                            content_digest: captured.content_digest.clone(),
                            entries: captured.entries,
                            bytes: captured.bytes,
                            limits,
                        };
                        cache.validate()?;
                        let producer = Producer::KernelOperation {
                            run_id: self.run_id.clone(),
                            node_id: Some(node_id.to_string()),
                            operation_id: format!(
                                "build-cache:{kind}:{}:{}",
                                self.authority.head_snapshot_id, self.authority.epoch
                            ),
                        };
                        let (artifact_id, _) = self
                            .cas
                            .put_artifact(
                                review_core::contract::BUILD_CACHE_V1,
                                producer,
                                vec![
                                    captured.manifest_id.clone(),
                                    self.authority.head_snapshot_id.clone(),
                                ],
                                Some(self.authority.head_snapshot_id.clone()),
                                encode(&cache)?,
                            )
                            .map_err(|error| error.to_string())?;
                        self.record_build_cache_evidence(
                            node_id,
                            kind,
                            captured.started_unix_ms,
                            captured.capture_ms,
                            0,
                            &captured.manifest_id,
                            captured.bytes,
                        )?;
                        refs.push(artifact_id.clone());
                        refs.push(captured.manifest_id);
                        BuildCacheCapturedPayloadV1 {
                            gate_node: node_id.to_string(),
                            gate_attempt_id: gate_attempt.map(str::to_owned),
                            head_snapshot_id: self.authority.head_snapshot_id.clone(),
                            kind,
                            limits,
                            build_cache_artifact_id: Some(artifact_id),
                            refused: None,
                            entries: captured.entries,
                            bytes: captured.bytes,
                        }
                    }
                    Err(error) => {
                        // The reason is durable and path-free; the machine-local detail is
                        // operator-only stderr, exactly as Cache Snapshot failures are reported.
                        eprintln!(
                            "build cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        BuildCacheCapturedPayloadV1 {
                            gate_node: node_id.to_string(),
                            gate_attempt_id: gate_attempt.map(str::to_owned),
                            head_snapshot_id: self.authority.head_snapshot_id.clone(),
                            kind,
                            limits,
                            build_cache_artifact_id: None,
                            refused: Some(refusal_reason(error.kind())),
                            entries: 0,
                            bytes: 0,
                        }
                    }
                };
            payload.validate()?;
            self.append(
                NewEvent::new(EventType::BuildCacheCapturedV1, encode(&payload)?)
                    .node(node_id)
                    .referencing(refs),
            )?;
        }
        Ok(())
    }

    /// Clone the Warm Set's Build Cache into one Worker sandbox and return the sandbox-local
    /// environment that points the build tool at it. Nothing happens for a node without a
    /// carried build cache; a node whose pipeline is not trusted-local is refused.
    pub(crate) fn materialize_build_cache(
        &self,
        node_id: &str,
        record: Option<&WarmSetRecord>,
        sandbox: &Sandbox,
    ) -> Result<Vec<(String, String)>, String> {
        let Some(artifact_id) =
            record.and_then(|record| record.set.build_cache_artifact_id.as_ref())
        else {
            return Ok(Vec::new());
        };
        self.build_cache_policy_admits()?;
        let kind = self.node_build_cache_kind(node_id).ok_or_else(|| {
            format!("node `{node_id}` carries a Build Cache without declaring a build cache kind")
        })?;
        let envelope = self
            .cas
            .get_artifact(artifact_id)
            .map_err(|error| error.to_string())?;
        if envelope.artifact_type != review_core::contract::BUILD_CACHE_V1 {
            return Err(format!("artifact {artifact_id} is not BuildCache@1"));
        }
        let cache: BuildCacheV1 =
            serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
        cache.validate()?;
        if cache.kind != kind
            || cache.trust != BuildCacheTrustV1::CandidateBuilt
            || cache.head_snapshot_id != self.authority.head_snapshot_id
        {
            return Err(format!(
                "Build Cache {artifact_id} belongs to another kind, trust or head Snapshot"
            ));
        }
        let manifest: Manifest = serde_json::from_value(
            self.cas
                .get_json(&cache.manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if manifest.content_digest() != cache.content_digest {
            return Err(format!(
                "Build Cache {artifact_id} manifest contradicts its content digest"
            ));
        }
        let materialized = review_sandbox::materialize_build_cache(
            kind,
            &manifest,
            sandbox,
            &cache.limits,
            self.cas,
        )
        .map_err(|error| {
            eprintln!(
                "build cache diagnostic for reviewer `{node_id}`: {}",
                error.operator_detail()
            );
            error.to_string()
        })?;
        self.record_build_cache_evidence(
            node_id,
            kind,
            materialized.started_unix_ms,
            0,
            materialized.materialization_ms,
            &cache.manifest_id,
            materialized.bytes,
        )?;
        Ok(review_sandbox::build_cache_environment(kind, sandbox.root()).local)
    }

    /// Measured host evidence beside the node, in the same `TaskRuntimeEvidence@1` shapes the
    /// safe cache uses. It is dependency-preparation evidence, never a compiler cache-hit claim.
    #[allow(clippy::too_many_arguments)] // one exact measurement; grouping would hide which clock each field is
    fn record_build_cache_evidence(
        &self,
        node_id: &str,
        kind: BuildCacheKindV1,
        started_unix_ms: u64,
        lookup_ms: u64,
        materialization_ms: u64,
        manifest_id: &str,
        bytes: u64,
    ) -> Result<(), String> {
        let span_id = self
            .cas
            .put_json(&serde_json::json!([
                self.run_id,
                node_id,
                "dependency_preparation",
                kind.as_str(),
                started_unix_ms,
                lookup_ms.saturating_add(materialization_ms)
            ]))
            .map_err(|error| error.to_string())?;
        let observation_id = self
            .cas
            .put_json(&serde_json::json!([
                self.run_id,
                node_id,
                "dependency_preparation",
                kind.as_str(),
                manifest_id,
                started_unix_ms,
                lookup_ms,
                materialization_ms
            ]))
            .map_err(|error| error.to_string())?;
        self.runtime_spans
            .lock()
            .expect("runtime spans")
            .entry(node_id.to_string())
            .or_default()
            .push(TaskRuntimeSpanV1 {
                span_id,
                kind: TaskRuntimeSpanKindV1::DependencyPreparation,
                label: kind.as_str().into(),
                started_unix_ms,
                elapsed_ms: lookup_ms.saturating_add(materialization_ms),
            });
        self.runtime_caches
            .lock()
            .expect("runtime caches")
            .entry(node_id.to_string())
            .or_default()
            .push(TaskCacheObservationV1 {
                observation_id,
                layer: TaskCacheLayerV1::DependencyPreparation,
                kind: kind.as_str().into(),
                eligible: true,
                result: TaskCacheResultV1::Prepared,
                source_digest: manifest_id.to_string(),
                toolchain_id: None,
                bytes_available: bytes,
                lookup_ms,
                materialization_ms,
            });
        Ok(())
    }
}

/// The Build Cache this Round's Gate captured for `kind`, read from the durable log so a
/// resumed Round selects the same artifact. Exactly one Gate may capture a kind in a Round.
pub(crate) fn select_build_cache(
    cas: &Cas,
    events: &[RunEvent],
    authority: &RoundAuthority,
    kind: BuildCacheKindV1,
) -> Result<(Option<String>, Option<BuildCacheDropReasonV1>), String> {
    let mut captured: Option<String> = None;
    let mut refused = false;
    for event in events.iter().filter(|event| {
        event.event_type == EventType::BuildCacheCapturedV1
            && event.causation_id.as_deref() == Some(authority.round_event_id.as_str())
    }) {
        let payload: BuildCacheCapturedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        payload.validate()?;
        if payload.kind != kind {
            continue;
        }
        if payload.head_snapshot_id != authority.head_snapshot_id {
            return Err(format!(
                "Gate `{}` captured a {kind} build cache for another head Snapshot",
                payload.gate_node
            ));
        }
        match payload.build_cache_artifact_id {
            Some(artifact_id) => {
                if captured.is_some() {
                    return Err(format!(
                        "more than one Gate captured a {kind} build cache in this Round"
                    ));
                }
                captured = Some(artifact_id);
            }
            None => refused = true,
        }
    }
    match captured {
        Some(artifact_id) => {
            let envelope = cas
                .get_artifact(&artifact_id)
                .map_err(|error| error.to_string())?;
            if envelope.artifact_type != review_core::contract::BUILD_CACHE_V1 {
                return Err(format!("artifact {artifact_id} is not BuildCache@1"));
            }
            let cache: BuildCacheV1 =
                serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
            cache.validate()?;
            if cache.kind != kind || cache.head_snapshot_id != authority.head_snapshot_id {
                return Err(format!(
                    "Build Cache {artifact_id} contradicts its capture record"
                ));
            }
            Ok((Some(artifact_id), None))
        }
        None if refused => Ok((None, Some(BuildCacheDropReasonV1::Refused))),
        None => Ok((None, Some(BuildCacheDropReasonV1::NotCaptured))),
    }
}
