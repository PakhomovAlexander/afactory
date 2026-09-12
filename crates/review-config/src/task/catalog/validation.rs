//! Reuse structural validation only while the compiler is immutably borrowed. Every use still
//! verifies the bytes of the authority artifacts; live lease, approval and budget checks stay
//! in the Store. This is not a cache of artifact integrity or dispatch authorization.

use std::collections::BTreeSet;
use std::ops::Deref;
use std::sync::Mutex;

use review_core::task::TaskRevisionV1;
use review_core::task::plan::{ExecutionPlanV1, GeneratedOriginV1};
use review_store::Cas;

use super::TaskPlanCompiler;

struct ValidatedPlan {
    task: TaskRevisionV1,
    plan: ExecutionPlanV1,
    artifacts: BTreeSet<String>,
}

/// The immutable borrow prevents package, binding or compiler-policy changes for the whole
/// validator lifetime. A changed plan or Task always passes through full recompilation.
pub struct CapturedTaskPlanValidator<'a> {
    compiler: &'a TaskPlanCompiler,
    validated: Mutex<Option<ValidatedPlan>>,
}

impl<'a> CapturedTaskPlanValidator<'a> {
    pub fn new(compiler: &'a TaskPlanCompiler) -> Self {
        Self {
            compiler,
            validated: Mutex::new(None),
        }
    }

    pub fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        let mut cached = self
            .validated
            .lock()
            .expect("captured Task plan validation");
        if let Some(known) = &*cached
            && &known.task == task
            && &known.plan == plan
        {
            // The original compiler reads exactly these CAS objects. Hashing them again
            // detects removal, replacement and corruption even on the same open Store.
            for id in &known.artifacts {
                cas.verify(id).map_err(|e| e.to_string())?;
            }
            return Ok(plan.generated_origins.clone());
        }
        let origins = self.compiler.validate_plan(cas, task, plan)?;
        let mut artifacts = BTreeSet::from([
            plan.task_revision_id.clone(),
            plan.compiled_graph_id.clone(),
            plan.engine_id.clone(),
            plan.authority.policy_id.clone(),
            plan.pipeline_id.clone(),
        ]);
        artifacts.extend(
            plan.dependencies
                .values()
                .map(|dependency| dependency.artifact_id.clone()),
        );
        artifacts.extend(
            plan.bindings
                .values()
                .map(|binding| binding.invocation_policy_id.clone()),
        );
        *cached = Some(ValidatedPlan {
            task: task.clone(),
            plan: plan.clone(),
            artifacts,
        });
        Ok(origins)
    }
}

impl Deref for CapturedTaskPlanValidator<'_> {
    type Target = TaskPlanCompiler;

    fn deref(&self) -> &Self::Target {
        self.compiler
    }
}
