//! The trusted CI Pipeline exception (ADR-0141): the one way a Remote Check may send a candidate
//! whose `.github/` differs from the Task's source.
//!
//! The exception is never a flag. It is a [`TrustedCiPipeline`], which only [`capture`] builds,
//! from records the Store already holds: the admitted Execution Plan, the Task revision it
//! compiles, that revision's captured run authority, the compiled graph's selected root and the
//! root's captured package bytes. The root must be the package the run authority pins — never a
//! generated, embedded or called Pipeline — and its own `pipeline.toml` must carry the exact tag
//! `ci`. Nothing a candidate, a Task file, an environment variable, a Pipeline name or a job
//! label says enters the decision, and the coordinator captures it again on every resume.
//!
//! [`capture`]: TrustedCiPipeline::capture

use std::collections::BTreeMap;

use review_config::task::catalog::{COMPILED_TASK_V1, TaskPlanCompiler};
use review_core::task::pipeline::PIPELINE_TAG_CI;
use review_core::task::plan::{ExecutionPlanV1, IndependencePolicyV1};
use review_core::task::remote_check::RemoteTrustedCiV1;
use review_core::task::{EXECUTION_PLAN_V1, TASK_REVISION_V1, TaskRevisionV1};
use review_graph::task::CompiledTask;
use review_store::Cas;
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// The schema of the run authority the coordinator captures for every Task. A Task whose
/// authority is anything else has no catalog pin, so no exception.
pub const RUN_AUTHORITY_SCHEMA: &str = "af.task-run-authority/2";

/// One pinned package of a captured run authority.
#[derive(Deserialize)]
struct PinnedPackage {
    digest: String,
    artifact_id: String,
}

/// The fields of a captured run authority this capture reads. The coordinator owns the full
/// document and its strict reader; this is a read-only view of it, so unknown fields are the
/// coordinator's and are ignored here.
#[derive(Deserialize)]
struct PinnedAuthority {
    schema: String,
    #[serde(default)]
    code_policy_id: Option<String>,
    #[serde(default)]
    packages: BTreeMap<String, PinnedPackage>,
}

/// A validated capture of the trusted CI Pipeline exception for one admitted plan. Its fields
/// are private and it has no other constructor, so a direct caller of the code domain or the
/// executor that did not capture it from the Store cannot claim it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedCiPipeline {
    record: RemoteTrustedCiV1,
    code_policy_id: String,
    graph: CompiledTask,
}

impl TrustedCiPipeline {
    /// Capture the exception for the admitted plan `plan_id`, or `None` when that plan does
    /// not grant it. `Err` only for records that do not read back as what they claim to be.
    ///
    /// The plan grants it when all of these hold:
    /// - it is not a Planner bootstrap and compiled no generated Pipeline;
    /// - its Task revision's captured authority is a run authority with a code policy;
    /// - the compiled graph's root call names a Pipeline that authority pins, at exactly the
    ///   package artifact and digest the plan's root and dependency name;
    /// - that package's own `pipeline.toml`, re-read and re-verified against its pinned digest,
    ///   carries the tag `ci`, spelled exactly so.
    pub fn capture(cas: &Cas, plan_id: &str) -> Result<Option<Self>, String> {
        let plan: ExecutionPlanV1 = artifact(cas, plan_id, EXECUTION_PLAN_V1)?;
        plan.validate()?;
        if plan.preparation.is_some() || !plan.generated_origins.is_empty() {
            return Ok(None);
        }
        let revision: TaskRevisionV1 = artifact(cas, &plan.task_revision_id, TASK_REVISION_V1)?;
        if revision.authority != plan.authority {
            return Err("Execution Plan names another Task authority than its revision".into());
        }
        let graph: CompiledTask = artifact(cas, &plan.compiled_graph_id, COMPILED_TASK_V1)?;
        let root = graph
            .calls
            .get("root")
            .map(|call| call.pipeline.clone())
            .ok_or("Execution Plan has no root Pipeline")?;
        let authority_id = &plan.authority.policy_id;
        // A Task whose authority is not a captured run authority has no catalog pins.
        let Ok(authority) = serde_json::from_value::<PinnedAuthority>(
            cas.get_json(authority_id).map_err(|e| e.to_string())?,
        ) else {
            return Ok(None);
        };
        if authority.schema != RUN_AUTHORITY_SCHEMA {
            return Ok(None);
        }
        let Some(code_policy_id) = authority.code_policy_id else {
            return Ok(None);
        };
        let Some(pinned) = authority.packages.get(&root) else {
            return Ok(None);
        };
        if pinned.artifact_id != plan.pipeline_id
            || plan
                .dependencies
                .get(&root)
                .is_none_or(|dependency| dependency.artifact_id != plan.pipeline_id)
        {
            return Ok(None);
        }
        // The pinned bytes, re-verified against their digest and parsed by the one package
        // reader the compiler uses.
        let mut compiler = TaskPlanCompiler::new(
            plan.engine_id.clone(),
            code_policy_id.clone(),
            BTreeMap::new(),
            BTreeMap::new(),
            IndependencePolicyV1::default(),
        )?;
        compiler.restore_package(cas, &root, &pinned.digest, &pinned.artifact_id)?;
        let Some(definition) = compiler.pipelines().get(&root) else {
            return Ok(None);
        };
        if !definition.tags.contains(PIPELINE_TAG_CI) {
            return Ok(None);
        }
        let record = RemoteTrustedCiV1 {
            tag: PIPELINE_TAG_CI.into(),
            authority_id: authority_id.clone(),
            plan_id: plan_id.into(),
            pipeline: root,
            pipeline_id: plan.pipeline_id.clone(),
        };
        record.validate()?;
        Ok(Some(Self {
            record,
            code_policy_id,
            graph,
        }))
    }

    /// What evidence records about the exception.
    pub fn record(&self) -> &RemoteTrustedCiV1 {
        &self.record
    }

    /// The code policy the granting run authority captured.
    pub fn code_policy_id(&self) -> &str {
        &self.code_policy_id
    }

    /// Whether this capture is the one of the domain that would use it: the same code policy
    /// and the plan's exact compiled graph.
    pub(crate) fn belongs_to(&self, policy_id: &str, graph: &CompiledTask) -> bool {
        self.code_policy_id == policy_id && self.graph == *graph
    }
}

fn artifact<T: DeserializeOwned>(cas: &Cas, id: &str, kind: &str) -> Result<T, String> {
    let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
    if envelope.artifact_id != id || envelope.artifact_type != kind {
        return Err(format!("Expected exact {kind} artifact"));
    }
    serde_json::from_value(envelope.payload).map_err(|e| e.to_string())
}
