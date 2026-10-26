//! A startup authority callback is a freshness boundary even while the Store mutex is held.
use super::*;
use review_store::store::task::{DeveloperGrant, TaskAuthority, task_run_id};
use std::sync::atomic::{AtomicBool, Ordering};

/// Validate through the real captured compiler, then deterministically simulate an external
/// CAS mutation before returning. TaskAuthority has no immutable borrow of the CAS files.
struct ChangeAfterValidation<'a> {
    compiler: &'a TaskPlanCompiler,
    file: std::path::PathBuf,
    remove: bool,
    changed: AtomicBool,
}

impl TaskAuthority for ChangeAfterValidation<'_> {
    fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        self.compiler.validate_plan(cas, task, plan)?;
        if !self.changed.swap(true, Ordering::SeqCst) {
            if self.remove {
                std::fs::remove_file(&self.file).unwrap();
            } else {
                std::fs::write(&self.file, b"changed after captured plan validation").unwrap();
            }
        }
        Ok(plan.generated_origins.clone())
    }

    fn authorize_decision(
        &self,
        _: &TaskRevisionV1,
        _: &str,
        _: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        Err("this fixture has no generated plan".into())
    }

    fn authorization_current(&self, _: &PlanDecisionV1) -> Result<(), String> {
        Err("this fixture has no developer decision".into())
    }

    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("startup cannot validate a result".into())
    }
}

#[test]
fn startup_rechecks_revision_integrity_after_the_authority_callback() {
    for remove in [false, true] {
        let mut f = Fixture::new(SUCCESS);
        let host = CapturedTaskHost::capture_with_models(
            &f.cas,
            &f.compiler,
            &f.task,
            &f.plan,
            f.graph.clone(),
            &EmptyTaskEnvironment,
            &DocumentDomain,
            &BTreeMap::new(),
        )
        .unwrap();
        let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
        let lease = f
            .store
            .open_task(&f.cas, &f.revision_id, "test-writer", 60_000)
            .unwrap();
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
            .unwrap();
        f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
        let hex = f.revision_id.strip_prefix("sha256:").unwrap();
        let file = f
            ._directory
            .path()
            .join("cas/objects")
            .join(&hex[..2])
            .join(&hex[2..]);
        let original = std::fs::read(&file).unwrap();
        let changing = ChangeAfterValidation {
            compiler: &f.compiler,
            file: file.clone(),
            remove,
            changed: AtomicBool::new(false),
        };
        let run_id = task_run_id(lease.task_id()).unwrap();
        let before = f.store.replay(&run_id).unwrap();
        let error = match TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &changing, &host) {
            Ok(_) => {
                panic!("startup accepted authority changed during validation; remove={remove}")
            }
            Err(error) => error,
        };
        assert!(changing.changed.load(Ordering::SeqCst));
        assert!(error.contains(&f.revision_id), "{error}");
        let after = f.store.replay(&run_id).unwrap();
        assert_eq!(&after[..before.len()], before.as_slice());
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(
            after.last().unwrap().event_type.as_str(),
            "TaskExecutionStarted@1",
            "only the pre-check execution marker was recorded"
        );
        std::fs::write(&file, original).unwrap();
        // Recovery under the same lease remains valid and never repeats its execution marker.
        drop(TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap());
        assert_eq!(f.store.replay(&run_id).unwrap(), after);
        let reopened = EventStore::open(f._directory.path().join("events.sqlite")).unwrap();
        assert!(
            reopened
                .task_projection(&f.cas, &f.task.task_id)
                .unwrap()
                .unwrap()
                .execution
                .is_none()
        );
    }
}
