//! Explicit application services required by the Scenario domain.
use crate::adapter::{Settlement, TaskCancellation};
use crate::contract::{Result, TaskReceipt, TaskTarget};
use crate::performance::{ProofRequest, Scope};
use crate::template::Template;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// The outcome of reading the exact binding, independent of adapter completion.
pub enum TaskCancellationObservation {
    Observed(Option<TaskCancellation>),
    Missing(String),
    Unavailable(String),
}

pub type FencedTaskCancellation = (TaskCancellationObservation, Option<Box<dyn Send>>);

/// Failure to materialize a prepared FreshTask intent.
#[derive(Debug)]
pub enum TemplatePlanError {
    StorageCapacity(String),
    Refused(String),
}

impl From<String> for TemplatePlanError {
    fn from(diagnostic: String) -> Self {
        Self::Refused(diagnostic)
    }
}

impl From<&str> for TemplatePlanError {
    fn from(diagnostic: &str) -> Self {
        diagnostic.to_owned().into()
    }
}

/// Native Task and machine Store services. Implementations must read exact native receipts,
/// preserve cancellation's recorded-run lease, and keep the Store collection hold alive until
/// its returned guard is dropped. None of these methods grants publication authority.
///
/// Template document validation is called during admission *and* journal replay. The domain
/// owns Scenario structure; the application owns its native Task-file grammar and diagnostics.
pub trait Host: Send + Sync {
    /// Trusted live authorization clock; inspection clocks cannot supply this value.
    fn authorization_time(&self) -> Result<u64> {
        crate::runtime::now()
    }
    fn preflight_implementation_template(
        &self,
        _: &Template,
        _: &BTreeMap<String, crate::implementation::ModelBinding>,
    ) -> Result<()> {
        Err("Host does not support implementation startup admission".into())
    }
    fn preflight_implementation_adapter(
        &self,
        _: &crate::contract::Adapter,
        _: &crate::implementation::Scope,
    ) -> crate::implementation::PreflightResult {
        Err("Host does not support implementation adapter preflight".into())
    }

    fn validate_template(
        &self,
        document: &serde_json::Value,
        bindings: Option<&serde_json::Value>,
        maximum: u64,
    ) -> Result<()>;
    fn check_predecessor(
        &self,
        template: &Template,
        receipt: &TaskReceipt,
        input: &str,
        port: &str,
        artifact_type: &str,
    ) -> Result<()>;
    fn check_experiment_source(
        &self,
        template: &Template,
        source: &TaskReceipt,
        port: &str,
        revision: &str,
    ) -> Result<()>;
    fn plan_template(
        &self,
        template: &Template,
        document: serde_json::Value,
        started_unix_ms: u64,
        performance: bool,
        predecessors: &BTreeMap<String, (TaskReceipt, String)>,
    ) -> std::result::Result<TaskTarget, TemplatePlanError>;
    fn check_repair(&self, _: &Template, _: &crate::task_outcome::TaskOutcome) -> Result<()> {
        Err("Host does not support checked native repair".into())
    }
    fn run_task(
        &self,
        target: &TaskTarget,
        intent_id: &str,
        timeout_ms: u64,
        cancellation: &AtomicBool,
    ) -> Result<Settlement>;
    fn check_run_dispatch(&self, _: &Path, _: &crate::state::Run) -> Result<()> {
        Ok(())
    }
    fn run_backlog_action(
        &self,
        _: &crate::state::Run,
        _: &crate::state::Dispatch,
        _: &crate::contract::Grant,
        _: &AtomicBool,
    ) -> Result<Option<String>> {
        Err("Host does not support local backlog actions".into())
    }
    /// A bounded native Task settlement, including deterministic refused-output observations.
    /// This never supplies a refused result through an accepted Task port.
    fn run_ci_diagnosis(
        &self,
        _target: &TaskTarget,
        _intent_id: &str,
        _timeout_ms: u64,
        _cancellation: &AtomicBool,
    ) -> Result<Settlement> {
        Err("Native CI diagnosis settlement unsupported by this Host".into())
    }
    /// Read one output from the exact accepted native result, validating its CI source evidence.
    /// Unsupported hosts refuse; payload text or an adapter response is never authority.
    fn task_output(
        &self,
        _target: &TaskTarget,
        _receipt: &TaskReceipt,
        _port: &str,
    ) -> Result<af_contracts::ArtifactEnvelope> {
        Err("Native Task output retrieval unsupported by this Host".into())
    }
    fn active_task_run(&self, target: &TaskTarget) -> Option<u64>;
    fn cancel_task_run(&self, target: &TaskTarget, expected: u64) -> Result<()>;
    /// Observe an operator stop of a writer bound to this native intent.
    fn task_cancellation(
        &self,
        _target: &TaskTarget,
        _intent_id: &str,
    ) -> Result<TaskCancellationObservation> {
        Ok(TaskCancellationObservation::Observed(None))
    }
    /// Observe cancellation under a guard that fences publication of new requests.
    /// The caller keeps the guard until the Scenario transaction commits.
    fn task_cancellation_for_settlement(
        &self,
        target: &TaskTarget,
        intent_id: &str,
    ) -> Result<FencedTaskCancellation> {
        Ok((self.task_cancellation(target, intent_id)?, None))
    }
    /// Missing, unreadable, running or mismatched Tasks retain the full reservation (`None`).
    fn task_charge(&self, target: &TaskTarget) -> Result<Option<(u64, bool)>>;
    fn validate_implementation_proof(
        &self,
        _: &crate::adapter::Request,
        _: &crate::state::Run,
        _: &crate::implementation::Scope,
    ) -> Result<()> {
        Err("Host does not support scoped native publication".into())
    }
    fn validate_performance_proof(&self, request: &ProofRequest, scope: &Scope) -> Result<()>;

    fn default_store_path(&self) -> Result<PathBuf>;
    fn resolve_path(&self, path: &Path) -> Result<PathBuf>;
    fn prepare_store(&self, path: &Path) -> Result<()>;
    fn private_dir(&self, path: &Path) -> Result<()>;
    fn register_store(&self, path: &Path);
    fn hold_store(&self, path: &Path) -> Result<Box<dyn Send>>;
}
