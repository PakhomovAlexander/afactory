//! Concrete AF Task and machine Store adapters for the Scenario domain.
use af_scenario::Host;
use af_scenario::adapter::Settlement;
use af_scenario::contract::*;
use af_scenario::performance::{ProofRequest, Scope};
use af_scenario::template::Template;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub(crate) struct AfHost;
impl Host for AfHost {
    fn run_task(
        &self,
        target: &TaskTarget,
        timeout_ms: u64,
        cancellation: &AtomicBool,
    ) -> Result<Settlement> {
        fn checked(target: &TaskTarget) -> Result<crate::task_commands::OpenedTask> {
            let task = crate::task_commands::open_task(
                &target.task_id,
                &target.repo,
                Some(&target.state),
            )?;
            require(
                task.task.revision_id == target.revision_id
                    && task.task.plan_id.as_deref() == Some(&target.plan_id)
                    && task.task.revision.limits.tokens <= target.max_tokens,
                "Pinned Task revision, plan or budget changed",
            )?;
            Ok(task)
        }
        let task = checked(target)?;
        if !matches!(
            task.task.phase,
            af_contracts::task::TaskPhaseV1::Finished { .. }
        ) {
            drop(task);
            let mut command =
                std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
            command
                .args([
                    "task",
                    "run",
                    &target.task_id,
                    "--confirm-plan",
                    &target.plan_id,
                    "--quiet",
                    "--json",
                    "--repo",
                ])
                .arg(&target.repo)
                .arg("--state")
                .arg(&target.state);
            let capture = af_process::run_supervised_captured_bounded(
                &mut command,
                None,
                Duration::from_millis(timeout_ms),
                af_process::ExitPolicy::KillDescendants,
                Some(cancellation),
                1_048_576,
            );
            // The Task Store, never exit status or stdout, supplies completion and acceptance.
            // A lost child is resumed under the same Task revision and its own recovery rules.
            let _ = capture;
        }
        let task = checked(target)?;
        let (result, _) = task.view()?;
        let Some(result) = result else {
            return Ok(Settlement::Waiting(format!(
                "Task {} has no accepted result yet",
                target.task_id
            )));
        };
        let charged_tokens = task.task.execution.as_ref().map_or(Ok(0), |e| {
            u64::try_from(e.budget.committed_tokens()).map_err(|_| "Task charge exceeds u64")
        })?;
        let unknown_usage = task.task.execution.as_ref().is_some_and(|e| {
            e.attempt_accounting()
                .iter()
                .any(|a| a.unknown_usage.is_some())
        });
        if result.acceptance != af_contracts::task::TaskAcceptanceV1::Satisfied
            || result.execution != af_contracts::task::TaskExecutionV1::Completed
        {
            return Ok(Settlement::TaskFailed {
                diagnostic: format!(
                    "Task {} did not satisfy acceptance: {:?}",
                    target.task_id, result.acceptance
                ),
                charged_tokens,
                unknown_usage,
            });
        }
        let af_contracts::task::TaskPhaseV1::Finished { result_id } = &task.task.phase else {
            return Err("Task result lacks durable completion".into());
        };
        let receipt = TaskReceipt {
            task_id: target.task_id.clone(),
            plan_id: target.plan_id.clone(),
            revision_id: target.revision_id.clone(),
            result_id: result_id.clone(),
            charged_tokens,
            result,
        };
        Ok(Settlement::TaskDone {
            outputs: BTreeMap::from([("result".into(), Datum::Task(receipt))]),
            unknown_usage,
        })
    }

    fn active_task_run(&self, target: &TaskTarget) -> Option<u64> {
        let opened =
            crate::task_commands::open_task(&target.task_id, &target.repo, Some(&target.state))
                .ok()?;
        if opened.task.plan_id.as_deref() != Some(&target.plan_id)
            || opened.task.revision_id != target.revision_id
            || opened.task.lease_until_unix_ms() <= af_scenario::runtime::now().ok()?
        {
            return None;
        }
        crate::task_commands::runs::records(&opened.state, &target.task_id)
            .into_iter()
            .rev()
            .find(|r| r.writer == opened.task.lease_writer())
            .map(|r| r.run)
    }
    fn task_charge(&self, target: &TaskTarget) -> Result<Option<(u64, bool)>> {
        let Ok(opened) =
            crate::task_commands::open_task(&target.task_id, &target.repo, Some(&target.state))
        else {
            return Ok(None);
        };
        if opened.task.revision_id != target.revision_id
            || opened.task.plan_id.as_ref() != Some(&target.plan_id)
        {
            return Ok(None);
        }
        if opened.view().is_ok_and(|(result, _)| result.is_some()) {
            let actual = opened.task.execution.as_ref().map_or(Ok(0), |e| {
                u64::try_from(e.budget.committed_tokens()).map_err(|_| "Native charge overflow")
            })?;
            let unknown = opened.task.execution.as_ref().is_some_and(|e| {
                e.attempt_accounting()
                    .iter()
                    .any(|a| a.unknown_usage.is_some())
            });
            return Ok(Some((actual, unknown)));
        }
        Ok(None)
    }
    fn cancel_task_run(&self, target: &TaskTarget, expected: u64) -> Result<()> {
        crate::task_commands::runs::cancel_recorded_run(
            &target.task_id,
            &target.repo,
            &target.state,
            expected,
        )
        .map(|_| ())
    }
    fn validate_template(
        &self,
        document: &serde_json::Value,
        bindings: Option<&serde_json::Value>,
        maximum: u64,
    ) -> Result<()> {
        crate::task_commands::scenario_template::validate_document(document, bindings, maximum)
    }
    fn check_predecessor(
        &self,
        template: &Template,
        receipt: &TaskReceipt,
        input: &str,
        port: &str,
        artifact_type: &str,
    ) -> Result<()> {
        crate::task_commands::scenario_template::check_predecessor(
            template,
            receipt,
            input,
            port,
            artifact_type,
        )
    }
    fn check_experiment_source(
        &self,
        template: &Template,
        source: &TaskReceipt,
        port: &str,
        revision: &str,
    ) -> Result<()> {
        crate::task_commands::scenario_template::check_experiment_source(
            template, source, port, revision,
        )
    }
    fn plan_template(
        &self,
        template: &Template,
        document: serde_json::Value,
        performance: bool,
        predecessors: &BTreeMap<String, (TaskReceipt, String)>,
    ) -> Result<TaskTarget> {
        crate::task_commands::scenario_template::plan(template, document, performance, predecessors)
    }
    fn validate_performance_proof(&self, request: &ProofRequest, scope: &Scope) -> Result<()> {
        crate::task_commands::candidate::performance::proof(request, &scope.repo, &scope.task_state)
            .map(|_| ())
    }
    fn default_store_path(&self) -> Result<PathBuf> {
        Ok(crate::xdg_state_root()?.join("af/scenarios"))
    }
    fn resolve_path(&self, path: &Path) -> Result<PathBuf> {
        crate::resolve_filesystem_path(path)
    }
    fn prepare_store(&self, path: &Path) -> Result<()> {
        crate::storage::owned::prepare(path, true)
    }
    fn private_dir(&self, path: &Path) -> Result<()> {
        crate::storage::owned::private_dir(path)
    }
    fn register_store(&self, path: &Path) {
        crate::storage::registry::record(path, crate::storage::registry::StoreKind::Task);
    }
    fn hold_store(&self, path: &Path) -> Result<Box<dyn Send>> {
        crate::storage::hold_running(path).map(|guard| Box::new(guard) as Box<dyn Send>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_template_host_keeps_the_task_file_parser_and_binding_refusals() {
        let document = json!({"schema":"af.task-file/1","task_id":"placeholder","kind":"implement","goal":"Complete the supplied optimization goal","pipeline":{"name":"fixture/implementation","fallback":"refuse"},"strategy":"small","verification":"evaluation","limits":{"tokens":20,"max_attempts":1,"wall_ms":60000,"verification":{"tokens":5,"attempts":1,"wall_ms":10000}}});
        AfHost.validate_template(&document, None, 20).unwrap();
        // Preserve the native half of the moved fixture tests at the application boundary.
        for bytes in [
            include_str!("../../../../fixtures/scenarios/fresh-task-loop.json"),
            include_str!("../../../../fixtures/scenarios/performance-candidate.json"),
            include_str!("../../../../fixtures/scenarios/performance-no-worthwhile.json"),
        ] {
            let bundle: Bundle = serde_json::from_str(bytes).unwrap();
            bundle.validate(&AfHost).unwrap();
        }

        let mut invalid = document.clone();
        invalid["unknown_native_field"] = true.into();
        assert!(
            AfHost
                .validate_template(&invalid, None, 20)
                .unwrap_err()
                .contains("unknown_native_field")
        );
        assert!(
            AfHost
                .validate_template(&document, None, 21)
                .unwrap_err()
                .contains("exact token limit")
        );
        let bindings =
            json!({"schema":"af.task-bindings/0","packages":{},"slots":{},"providers":{}});
        assert!(
            AfHost
                .validate_template(&document, Some(&bindings), 20)
                .unwrap_err()
                .contains("af.task-bindings/1")
        );
    }
}
