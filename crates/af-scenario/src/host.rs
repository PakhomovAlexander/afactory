//! Explicit application services required by the Scenario domain.
use crate::adapter::Settlement;
use crate::contract::{Result, TaskReceipt, TaskTarget};
use crate::performance::{ProofRequest, Scope};
use crate::template::Template;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

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
    ) -> Result<()> {
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
        performance: bool,
        predecessors: &BTreeMap<String, (TaskReceipt, String)>,
    ) -> Result<TaskTarget>;
    fn check_repair(&self, _: &Template, _: &crate::task_outcome::TaskOutcome) -> Result<()> {
        Err("Host does not support checked native repair".into())
    }
    fn run_task(
        &self,
        target: &TaskTarget,
        timeout_ms: u64,
        cancellation: &AtomicBool,
    ) -> Result<Settlement>;
    fn active_task_run(&self, target: &TaskTarget) -> Option<u64>;
    fn cancel_task_run(&self, target: &TaskTarget, expected: u64) -> Result<()>;
    /// Missing, unreadable, running or mismatched Tasks retain the full reservation (`None`).
    fn task_charge(&self, target: &TaskTarget) -> Result<Option<(u64, bool)>>;
    fn validate_performance_proof(&self, request: &ProofRequest, scope: &Scope) -> Result<()>;

    fn default_store_path(&self) -> Result<PathBuf>;
    fn resolve_path(&self, path: &Path) -> Result<PathBuf>;
    fn prepare_store(&self, path: &Path) -> Result<()>;
    fn private_dir(&self, path: &Path) -> Result<()>;
    fn register_store(&self, path: &Path);
    fn hold_store(&self, path: &Path) -> Result<Box<dyn Send>>;
}
