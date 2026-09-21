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
    TaskCacheObservationV1, TaskRuntimeSpanKindV1, TaskRuntimeSpanV1,
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

    /// The Gate a build-cache reviewer node waits on: the only Gate whose capture it may
    /// receive. Dynamic reviewers resolve through their static base.
    pub(crate) fn build_cache_gate(&self, node_id: &str) -> Result<String, String> {
        let base = self.reviewer_binding_node(node_id);
        self.warm_build_cache_gates
            .get(node_id)
            .or_else(|| self.warm_build_cache_gates.get(&base))
            .cloned()
            .ok_or_else(|| {
                format!("node `{node_id}` declares a build cache kind but waits on no Gate")
            })
    }

    /// Capture every declared kind from the Gate sandbox after its checks passed, and record
    /// one `BuildCacheCaptured@1` per kind: the artifact, or the reason the closed layout
    /// refused it. A refusal never changes the Gate verdict. The record is buffered with the
    /// Gate's check results and published in the same batch as its decision, so no log ever
    /// holds a capture whose Gate decision did not become durable with it.
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
                        let evidence = self.build_cache_evidence(
                            node_id,
                            kind,
                            captured.started_unix_ms,
                            captured.capture_ms,
                            0,
                            &captured.manifest_id,
                            captured.bytes,
                        )?;
                        self.retain_build_cache_evidence(node_id, evidence);
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
            self.buffer_reviewer_event(
                node_id,
                NewEvent::new(EventType::BuildCacheCapturedV1, encode(&payload)?)
                    .node(node_id)
                    .referencing(refs),
            );
        }
        Ok(())
    }

    /// Clone the Warm Set's Build Cache into one Worker sandbox and return the sandbox-local
    /// environment that points the build tool at it, with the measured clone. The caller owns
    /// the measurement: it belongs to the exact Attempt whose sandbox received the clone, so a
    /// Task-hosted Worker retains it with that Attempt rather than beside the node. Nothing
    /// happens for a node without a carried build cache; a node whose pipeline is not
    /// trusted-local is refused.
    pub(crate) fn materialize_build_cache(
        &self,
        node_id: &str,
        record: Option<&WarmSetRecord>,
        sandbox: &Sandbox,
    ) -> Result<BuildCacheHandoff, String> {
        let Some(artifact_id) =
            record.and_then(|record| record.set.build_cache_artifact_id.as_ref())
        else {
            return Ok((Vec::new(), None));
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
        let evidence = self.build_cache_evidence(
            node_id,
            kind,
            materialized.started_unix_ms,
            0,
            materialized.materialization_ms,
            &cache.manifest_id,
            materialized.bytes,
        )?;
        Ok((
            review_sandbox::build_cache_environment(kind, sandbox.root()).local,
            Some(evidence),
        ))
    }

    /// Measured host evidence in the same `TaskRuntimeEvidence@1` shapes the safe cache uses.
    /// It is dependency-preparation evidence, never a compiler cache-hit claim.
    #[allow(clippy::too_many_arguments)] // one exact measurement; grouping would hide which clock each field is
    fn build_cache_evidence(
        &self,
        node_id: &str,
        kind: BuildCacheKindV1,
        started_unix_ms: u64,
        lookup_ms: u64,
        materialization_ms: u64,
        manifest_id: &str,
        bytes: u64,
    ) -> Result<BuildCacheEvidence, String> {
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
        Ok(BuildCacheEvidence {
            span: TaskRuntimeSpanV1 {
                span_id,
                kind: TaskRuntimeSpanKindV1::DependencyPreparation,
                label: kind.as_str().into(),
                started_unix_ms,
                elapsed_ms: lookup_ms.saturating_add(materialization_ms),
            },
            observation: TaskCacheObservationV1 {
                observation_id,
                kind: kind.as_str().into(),
                eligible: true,
                source_digest: manifest_id.to_string(),
                toolchain_id: None,
                bytes_available: bytes,
                lookup_ms,
                materialization_ms,
            },
        })
    }

    /// Keep a measurement beside its node, where a Task-hosted Gate's settlement collects it.
    pub(crate) fn retain_build_cache_evidence(&self, node_id: &str, evidence: BuildCacheEvidence) {
        self.runtime_spans
            .lock()
            .expect("runtime spans")
            .entry(node_id.to_string())
            .or_default()
            .push(evidence.span);
        self.runtime_caches
            .lock()
            .expect("runtime caches")
            .entry(node_id.to_string())
            .or_default()
            .push(evidence.observation);
    }
}

/// What one Worker sandbox received: the sandbox-local environment that points the build tool
/// at the clone, and the measured clone itself for the caller to retain with its Attempt.
pub(crate) type BuildCacheHandoff = (Vec<(String, String)>, Option<BuildCacheEvidence>);

/// One measured capture or clone: a `dependency_preparation` span and its cache observation.
#[derive(Debug, Clone)]
pub(crate) struct BuildCacheEvidence {
    pub(crate) span: TaskRuntimeSpanV1,
    pub(crate) observation: TaskCacheObservationV1,
}

/// What the bound Gate published for one kind in this Round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublishedBuildCache {
    Captured(String),
    Refused,
    NotCaptured,
}

/// The capture record the bound Gate published with its passing decision. Records are read
/// from the durable log so a resumed Round selects the same artifact. Only the capture
/// published on `gate_node` before that Gate's passing decision counts: a record another Gate
/// appended, or one drained later from a Gate Attempt that never decided, is not this Gate's
/// handoff. Among that Gate's records the last one before its decision is the one its batch
/// carried.
pub(crate) fn published_build_cache(
    events: &[RunEvent],
    round_event_id: &str,
    head_snapshot_id: &str,
    gate_node: &str,
    kind: BuildCacheKindV1,
) -> Result<PublishedBuildCache, String> {
    let in_round = |event: &&RunEvent| {
        event.causation_id.as_deref() == Some(round_event_id)
            && event.node_id.as_deref() == Some(gate_node)
    };
    let mut decided: Option<u64> = None;
    for event in events
        .iter()
        .filter(in_round)
        .filter(|event| event.event_type == EventType::GateDecisionV1)
    {
        let decision: review_check::GateDecision =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        if decision.passed() {
            decided = Some(decided.map_or(event.sequence, |seen| seen.max(event.sequence)));
        }
    }
    let Some(decided) = decided else {
        return Err(format!(
            "Gate `{gate_node}` has no published passing decision in this Round; a Build Cache is selected only after the Gate that captured it"
        ));
    };
    let mut selected: Option<(u64, BuildCacheCapturedPayloadV1)> = None;
    for event in events.iter().filter(in_round).filter(|event| {
        event.event_type == EventType::BuildCacheCapturedV1 && event.sequence < decided
    }) {
        let payload: BuildCacheCapturedPayloadV1 =
            serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
        payload.validate()?;
        if payload.kind != kind {
            continue;
        }
        if payload.gate_node != gate_node {
            return Err(format!(
                "a {kind} build cache record on Gate `{gate_node}` names Gate `{}`",
                payload.gate_node
            ));
        }
        if payload.head_snapshot_id != head_snapshot_id {
            return Err(format!(
                "Gate `{gate_node}` captured a {kind} build cache for another head Snapshot"
            ));
        }
        if selected
            .as_ref()
            .is_none_or(|(sequence, _)| event.sequence > *sequence)
        {
            selected = Some((event.sequence, payload));
        }
    }
    Ok(match selected {
        Some((_, payload)) => match payload.build_cache_artifact_id {
            Some(artifact_id) => PublishedBuildCache::Captured(artifact_id),
            None => PublishedBuildCache::Refused,
        },
        None => PublishedBuildCache::NotCaptured,
    })
}

/// The Build Cache the bound Gate handed this Round, checked against the CAS before any
/// Attempt is reserved: the artifact must be a `BuildCache@1` of the same kind and head.
pub(crate) fn select_build_cache(
    cas: &Cas,
    events: &[RunEvent],
    authority: &RoundAuthority,
    gate_node: &str,
    kind: BuildCacheKindV1,
) -> Result<(Option<String>, Option<BuildCacheDropReasonV1>), String> {
    match published_build_cache(
        events,
        &authority.round_event_id,
        &authority.head_snapshot_id,
        gate_node,
        kind,
    )? {
        PublishedBuildCache::Captured(artifact_id) => {
            let envelope = cas
                .get_artifact(&artifact_id)
                .map_err(|error| error.to_string())?;
            if envelope.artifact_type != review_core::contract::BUILD_CACHE_V1 {
                return Err(format!("artifact {artifact_id} is not BuildCache@1"));
            }
            let cache: BuildCacheV1 =
                serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
            cache.validate()?;
            if cache.kind != kind
                || cache.gate_node != gate_node
                || cache.head_snapshot_id != authority.head_snapshot_id
            {
                return Err(format!(
                    "Build Cache {artifact_id} contradicts its capture record"
                ));
            }
            Ok((Some(artifact_id), None))
        }
        PublishedBuildCache::Refused => Ok((None, Some(BuildCacheDropReasonV1::Refused))),
        PublishedBuildCache::NotCaptured => Ok((None, Some(BuildCacheDropReasonV1::NotCaptured))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_check::{GateDecision, GateOutcome};

    const ROUND: &str = "round-1";

    fn head() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    fn event(
        sequence: u64,
        node: &str,
        event_type: EventType,
        payload: serde_json::Value,
    ) -> RunEvent {
        RunEvent {
            event_id: format!("event-{sequence}"),
            run_id: "run".into(),
            sequence,
            event_type,
            occurred_at: "2026-01-01T00:00:00Z".into(),
            node_id: Some(node.into()),
            attempt_id: None,
            causation_id: Some(ROUND.into()),
            correlation_id: None,
            artifact_refs: vec![],
            payload,
        }
    }

    fn decision(sequence: u64, node: &str, outcome: GateOutcome) -> RunEvent {
        let decision = GateDecision {
            outcome,
            blocking: vec![],
            reasons: vec![],
            executed: 1,
            required: 1,
        };
        event(
            sequence,
            node,
            EventType::GateDecisionV1,
            serde_json::to_value(decision).unwrap(),
        )
    }

    fn capture(sequence: u64, node: &str, artifact: Option<&str>) -> RunEvent {
        let payload = BuildCacheCapturedPayloadV1 {
            gate_node: node.into(),
            gate_attempt_id: None,
            head_snapshot_id: head(),
            kind: BuildCacheKindV1::CargoTarget,
            limits: review_core::BuildCacheLimitsV1::default_v1(),
            build_cache_artifact_id: artifact.map(str::to_owned),
            refused: artifact
                .is_none()
                .then_some(BuildCacheRefusalReasonV1::UnsafeContent),
            entries: if artifact.is_some() { 1 } else { 0 },
            bytes: if artifact.is_some() { 1 } else { 0 },
        };
        payload.validate().unwrap();
        event(
            sequence,
            node,
            EventType::BuildCacheCapturedV1,
            serde_json::to_value(payload).unwrap(),
        )
    }

    /// A well-formed artifact id; the label must be a hex digit.
    fn artifact(label: char) -> String {
        format!("sha256:{}", label.to_string().repeat(64))
    }

    fn select(events: &[RunEvent], gate: &str) -> Result<PublishedBuildCache, String> {
        published_build_cache(events, ROUND, &head(), gate, BuildCacheKindV1::CargoTarget)
    }

    #[test]
    fn only_the_bound_gates_capture_is_selected() {
        let other = artifact('b');
        let own = artifact('c');
        let events = [
            capture(1, "other-gate", Some(&other)),
            decision(2, "other-gate", GateOutcome::Passed),
            capture(3, "gate", Some(&own)),
            decision(4, "gate", GateOutcome::Passed),
        ];
        assert_eq!(
            select(&events, "gate").unwrap(),
            PublishedBuildCache::Captured(own)
        );
        assert_eq!(
            select(&events, "other-gate").unwrap(),
            PublishedBuildCache::Captured(other)
        );
    }

    #[test]
    fn a_reviewer_bound_to_a_gate_that_captured_nothing_runs_cold() {
        let events = [
            capture(1, "other-gate", Some(&artifact('b'))),
            decision(2, "other-gate", GateOutcome::Passed),
            decision(3, "gate", GateOutcome::Passed),
        ];
        assert_eq!(
            select(&events, "gate").unwrap(),
            PublishedBuildCache::NotCaptured
        );
        let refused = [
            capture(1, "gate", None),
            decision(2, "gate", GateOutcome::Passed),
        ];
        assert_eq!(
            select(&refused, "gate").unwrap(),
            PublishedBuildCache::Refused
        );
    }

    #[test]
    fn a_capture_without_a_published_passing_decision_is_never_selected() {
        let stale = artifact('d');
        let unpublished = [capture(1, "gate", Some(&stale))];
        assert!(
            select(&unpublished, "gate")
                .unwrap_err()
                .contains("no published passing decision")
        );
        let blocked = [
            capture(1, "gate", Some(&stale)),
            decision(2, "gate", GateOutcome::Blocked),
        ];
        assert!(
            select(&blocked, "gate")
                .unwrap_err()
                .contains("no published passing decision")
        );
        // A record drained after the decision belongs to a Gate Attempt that never decided.
        let drained = [
            capture(1, "gate", Some(&artifact('e'))),
            decision(2, "gate", GateOutcome::Passed),
            capture(3, "gate", Some(&stale)),
        ];
        assert_eq!(
            select(&drained, "gate").unwrap(),
            PublishedBuildCache::Captured(artifact('e'))
        );
    }

    #[test]
    fn the_record_published_with_the_decision_wins_over_an_earlier_one() {
        let events = [
            capture(1, "gate", Some(&artifact('f'))),
            capture(2, "gate", Some(&artifact('7'))),
            decision(3, "gate", GateOutcome::Passed),
        ];
        assert_eq!(
            select(&events, "gate").unwrap(),
            PublishedBuildCache::Captured(artifact('7'))
        );
    }

    #[test]
    fn a_record_that_names_another_gate_or_head_is_refused() {
        let mut foreign = capture(1, "gate", Some(&artifact('8')));
        foreign.payload["gate_node"] = serde_json::json!("other-gate");
        let events = [foreign, decision(2, "gate", GateOutcome::Passed)];
        assert!(
            select(&events, "gate")
                .unwrap_err()
                .contains("names Gate `other-gate`")
        );
        let mut moved = capture(1, "gate", Some(&artifact('8')));
        moved.payload["head_snapshot_id"] = serde_json::json!(format!("sha256:{}", "9".repeat(64)));
        let events = [moved, decision(2, "gate", GateOutcome::Passed)];
        assert!(
            select(&events, "gate")
                .unwrap_err()
                .contains("another head Snapshot")
        );
    }
}
