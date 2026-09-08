//! The composition layer: the graph driving the real nodes.
//!
//! Everything below this crate was built to be provable in isolation — capture without a
//! scheduler, scheduling without models, checks without a graph. This is where they meet, and
//! the only thing it adds is wiring. That is deliberate: if composing them required new rules,
//! the boundaries underneath would be wrong.
//!
//! One review therefore looks like this, end to end:
//!
//! ```text
//!   capture ── snapshot ──┐
//!                         v
//!            gate (checks in a sandbox) ──decision──┐
//!                                                   v
//!                    architecture ┐  performance ┐  tdd ┐   (each sandboxed, gated)
//!                                 └──────────────┴──────┴──> gather
//!                                                              │
//!                                                              v
//!                                                           ledger ──> convergence
//! ```
//!
//! A blocked gate makes every node after it unreachable, so a review that could not build
//! produces no reviewer artifacts at all — not reviewer artifacts nobody reads.

mod authority;
mod kernel;
pub mod mutations;
pub mod scatter;
pub mod task;
mod verdict;

pub use authority::RoundAuthority;
pub use kernel::{AttemptEvidence, Kernel};
pub use mutations::mutation_summary;
pub use verdict::{RunVerdict, run_suppression_reason, run_verdict};

use std::collections::BTreeMap;
use std::sync::Arc;

use review_broker::{Connector, Credential};
use review_core::{
    PortArtifactsV1, ReviewerResultContract, RunCacheFailureReasonV5, RunCacheKindV5,
    RunCacheMaterializationV5, RunIsolationV4, SnapshotAffinity,
};
use review_graph::{ArtifactMap, Node, PortContract};
use review_sandbox::{CacheErrorKind, CacheKind, CacheMaterialization, Isolation};

/// Machine-local capability material for one brokered reviewer. Neither field is captured in
/// project authority or durable evidence; only bounded symbolic operation policy is.
pub struct BrokerProvider {
    credential: Vec<u8>,
    connector: Arc<dyn Connector>,
}

impl BrokerProvider {
    pub fn new(
        credential: impl Into<Vec<u8>>,
        connector: Arc<dyn Connector>,
    ) -> Result<Self, String> {
        let credential = credential.into();
        Credential::new(credential.clone()).map_err(|error| error.to_string())?;
        Ok(Self {
            credential,
            connector,
        })
    }
}

fn is_generation_prior_findings_output(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::PRIOR_FINDINGS_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "findings"
}

fn is_generation_finding_set_output(port: &PortContract) -> bool {
    port.artifact_type == review_core::contract::FINDING_SET_V1
}

fn is_demand_set_port(port: &PortContract) -> bool {
    port.artifact_type == review_core::contract::DEMAND_SET_V1
}

fn is_reviewer_prior_findings_input(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::PRIOR_FINDINGS_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "prior_findings"
}

fn is_reviewer_finding_set_input(port: &PortContract) -> bool {
    port.artifact_type == review_core::contract::FINDING_SET_V1
}

fn is_reviewer_prior_set_input(port: &PortContract, pipeline_version: u32) -> bool {
    is_reviewer_prior_findings_input(port, pipeline_version) || is_reviewer_finding_set_input(port)
}

fn reviewer_result_contract(node: &Node) -> Result<ReviewerResultContract, String> {
    let [port] = node.outputs.as_slice() else {
        return Err(format!(
            "reviewer `{}` must declare exactly one result output",
            node.id
        ));
    };
    ReviewerResultContract::parse_artifact_type(&port.artifact_type)
        .or_else(|| {
            (port.artifact_type == review_core::contract::OPAQUE_V1)
                .then_some(ReviewerResultContract::V1)
        })
        .ok_or_else(|| {
            format!(
                "reviewer `{}` output `{}` has unsupported result type `{}`",
                node.id, port.name, port.artifact_type
            )
        })
}

fn is_change_set_port(port: &PortContract, pipeline_version: u32) -> bool {
    port.artifact_type == review_core::contract::CHANGE_SET_V1
        || pipeline_version == 1
            && port.artifact_type == review_core::contract::OPAQUE_V1
            && port.name == "change_set"
}

fn run_isolation(isolation: Isolation) -> RunIsolationV4 {
    match isolation {
        Isolation::None => RunIsolationV4::None,
        Isolation::Process => RunIsolationV4::Process,
        Isolation::Container => RunIsolationV4::Container,
    }
}

fn run_cache_kind(kind: CacheKind) -> RunCacheKindV5 {
    match kind {
        CacheKind::Cargo => RunCacheKindV5::Cargo,
    }
}

fn run_cache_materialization(method: CacheMaterialization) -> RunCacheMaterializationV5 {
    match method {
        CacheMaterialization::Reflink => RunCacheMaterializationV5::Reflink,
        CacheMaterialization::Copy => RunCacheMaterializationV5::Copy,
    }
}

fn cache_failure_reason(kind: CacheErrorKind) -> RunCacheFailureReasonV5 {
    match kind {
        CacheErrorKind::PolicyUnavailable => RunCacheFailureReasonV5::PolicyUnavailable,
        CacheErrorKind::SourceUnavailable => RunCacheFailureReasonV5::SourceUnavailable,
        CacheErrorKind::UnsafeContent => RunCacheFailureReasonV5::UnsafeContent,
        CacheErrorKind::LimitExceeded => RunCacheFailureReasonV5::LimitExceeded,
        CacheErrorKind::CopyLimitExceeded => RunCacheFailureReasonV5::CopyLimitExceeded,
        CacheErrorKind::ConcurrentChange => RunCacheFailureReasonV5::ConcurrentChange,
        CacheErrorKind::MaterializationFailed => RunCacheFailureReasonV5::MaterializationFailed,
    }
}

fn gate_provider_admitted(
    provider: review_config::SandboxProviderSpec,
    provided: Isolation,
    required: Isolation,
) -> bool {
    let usable = match provider {
        review_config::SandboxProviderSpec::TrustedLocal => true,
        review_config::SandboxProviderSpec::Container => provided == Isolation::Container,
    };
    usable && provided >= required
}

/// The artifact ids a node's resolved inputs carry, dropping the port labels — for reducers
/// (gather, ledger) that consume artifacts regardless of which port delivered them.
fn artifact_ids(inputs: &ArtifactMap) -> Vec<String> {
    inputs.values().flatten().cloned().collect()
}

fn bind_single_output(node: &Node, artifacts: Vec<String>) -> Result<ArtifactMap, String> {
    let [port] = node.outputs.as_slice() else {
        return Err(format!(
            "node {} has {} output ports, but its built-in dispatcher produces one port",
            node.id,
            node.outputs.len()
        ));
    };
    Ok(BTreeMap::from([(port.name.clone(), artifacts)]))
}

fn port_artifacts(
    contracts: &[PortContract],
    artifacts: &ArtifactMap,
    subject_snapshot_id: &str,
) -> Vec<PortArtifactsV1> {
    contracts
        .iter()
        .map(|port| PortArtifactsV1 {
            port: port.name.clone(),
            artifact_type: port.artifact_type.clone(),
            cardinality: port.cardinality,
            optional: port.optional,
            snapshot_affinity: port.snapshot_affinity,
            artifact_ids: artifacts.get(&port.name).cloned().unwrap_or_default(),
            subject_snapshot_id: (port.snapshot_affinity == SnapshotAffinity::SameSubject)
                .then(|| subject_snapshot_id.to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unusable_container_is_not_admitted_even_when_none_was_required() {
        assert!(!gate_provider_admitted(
            review_config::SandboxProviderSpec::Container,
            Isolation::None,
            Isolation::None,
        ));
        assert!(gate_provider_admitted(
            review_config::SandboxProviderSpec::Container,
            Isolation::Container,
            Isolation::None,
        ));
    }
}
