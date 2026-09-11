//! The Planner bootstrap is installed by the engine from a captured Worker setting. Project
//! Pipeline text cannot declare itself a preparation plan or relax business acceptance.
use super::*;
use review_core::task::pipeline::*;
use review_core::task::planning::{PIPELINE_PROPOSAL_V1, PLANNING_REQUEST_V1, PipelineProposalV1};
use review_store::store::task::planning::TaskPlanningProof;

pub const PLANNER_PIPELINE: &str = "af-internal/task-planner";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannerSettings {
    pub worker: String,
    pub max_attempts: u32,
}
impl PlannerSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !is_package_name(&self.worker) || !(1..=2).contains(&self.max_attempts) {
            return Err("Planner requires a captured Worker and at most two Attempts".into());
        }
        Ok(())
    }
}

fn port(artifact_type: &str, cardinality: review_core::PortCardinality) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: artifact_type.into(),
        cardinality,
        optional: false,
        affinity: PortAffinityV1::Unbound {},
        root_default: None,
        covers: BTreeSet::new(),
    }
}

pub fn planning_context_signature() -> OperatorSignature {
    OperatorSignature {
        contract: PipelineContractV1 {
            inputs: BTreeMap::new(),
            outputs: BTreeMap::from([(
                "request".into(),
                port(PLANNING_REQUEST_V1, review_core::PortCardinality::One),
            )]),
        },
        effects: BTreeSet::new(),
        evidence: BTreeMap::new(),
        retains: BTreeMap::new(),
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        outcome_port: None,
        attempt: None,
    }
}

impl TaskPlanCompiler {
    pub fn validate_planning_inputs(
        &self,
        cas: &Cas,
        previous: &TaskRevisionV1,
        next: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<(), String> {
        let root = plan
            .dependencies
            .iter()
            .find(|(_, p)| p.artifact_id == plan.pipeline_id)
            .map(|(name, _)| name.as_str())
            .ok_or("Generated root is not captured")?;
        if self.normalize_root_inputs(cas, root, previous.inputs.clone())? != next.inputs {
            return Err("Planning changed inputs beyond admitted root constructors".into());
        }
        Ok(())
    }
    /// Expose only the Task contract and captured public interfaces needed for composition.
    /// Worker instructions, package files, source payloads and Provider credentials are absent.
    pub fn planning_request(&self, task: &TaskRevisionV1) -> Result<serde_json::Value, String> {
        task.validate()?;
        let permits = |signature: &&OperatorSignature| {
            signature.effects.is_subset(&task.authority.allowed_effects)
        };
        let operators: BTreeMap<_, _> = self
            .signatures
            .iter()
            .filter(|(name, signature)| {
                name.starts_with("operator/")
                    && name.as_str() != "operator/planning-context"
                    && permits(signature)
            })
            .collect();
        let workers: BTreeMap<_, _> = self.workers.iter()
            .filter(|(_, worker)| !worker.signature.roles.contains("plan") && worker.signature.effects.is_subset(&task.authority.allowed_effects))
            .map(|(name, worker)| (name, serde_json::json!({"version":worker.version, "digest":self.packages[name].bytes.digest, "signature":worker.signature})))
            .collect();
        let pipelines: BTreeMap<_, _> = self.pipelines.iter()
            .filter(|(name, _)| !self.preparation_roots.contains(*name) && !self.generated.contains_key(*name))
            .map(|(name, pipeline)| (name, serde_json::json!({"version":pipeline.version, "digest":self.packages[name].bytes.digest, "contract":pipeline.contract, "accepts":pipeline.accepts, "slots":pipeline.slots, "max_attempts":pipeline.max_attempts, "max_parallel":pipeline.max_parallel})))
            .collect();
        let request = serde_json::json!({
            "schema":"af.planning-request/1",
            "task":{"kind":task.kind,"goal":task.goal,"inputs":task.inputs,
                "required_outputs":task.required_outputs,"acceptance":task.acceptance,
                "allowed_effects":task.authority.allowed_effects,"limits":task.limits,
                "strategy":task.strategy,"facts":task.facts},
            "operators":operators,"workers":workers,"pipelines":pipelines,
            "bounds":{"max_definitions":16,"max_definition_bytes":262144,"max_total_bytes":1048576,"max_expanded_nodes":64,"max_depth":4},
        });
        if serde_json::to_vec(&request)
            .map_err(|e| e.to_string())?
            .len()
            > 65536
        {
            return Err(
                "Planning request exceeds its 64 KiB interface budget; narrow the captured catalog"
                    .into(),
            );
        }
        Ok(request)
    }

    /// Only the common Store can establish that this exact proposal was selected by the
    /// admitted Planner. Generated origins are derived here for every proposed dependency.
    pub fn install_selected_proposal(
        &mut self,
        cas: &Cas,
        proof: &TaskPlanningProof,
    ) -> Result<String, String> {
        let original = read_envelope(
            cas,
            proof.revision_id(),
            review_core::task::TASK_REVISION_V1,
        )?;
        let task: TaskRevisionV1 =
            serde_json::from_value(original.payload).map_err(|e| e.to_string())?;
        let bootstrap = read_envelope(
            cas,
            proof.bootstrap_plan_id(),
            review_core::task::EXECUTION_PLAN_V1,
        )?;
        let bootstrap: ExecutionPlanV1 =
            serde_json::from_value(bootstrap.payload).map_err(|e| e.to_string())?;
        self.validate_plan(cas, &task, &bootstrap)?;
        if bootstrap.preparation.is_none() {
            return Err("Selected proposal requires the exact installed preparation plan".into());
        }
        let envelope = read_envelope(cas, proof.proposal_id(), PIPELINE_PROPOSAL_V1)?;
        let proposal: PipelineProposalV1 =
            serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
        let mut next = self.clone();
        next.install_proposal_bytes(
            cas,
            &proposal,
            proof.proposal_id(),
            proof.bootstrap_plan_id(),
        )?;
        *self = next;
        Ok(proposal.root)
    }

    fn proposal_structure(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        proposal: &PipelineProposalV1,
    ) -> Result<(Self, TaskRevisionV1, CompiledTask), String> {
        let mut preview = self.clone();
        // These private placeholders are never published as a plan or returned to a caller.
        preview.install_proposal_bytes(cas, proposal, &self.engine_id, &self.engine_id)?;
        let mut normalized = task.clone();
        normalized.inputs =
            preview.normalize_root_inputs(cas, &proposal.root, normalized.inputs)?;
        let graph = preview.compile_graph(&normalized, &proposal.root)?;
        preview.validate_replacement_schemas(&graph)?;
        Ok((preview, normalized, graph))
    }

    /// Pure domain admission used under the Store lock. Host capability reads happen outside
    /// that lock before full proposal validation and again before the final handoff.
    pub fn check_proposal_structure(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        proposal: &PipelineProposalV1,
    ) -> Result<(), String> {
        self.proposal_structure(cas, task, proposal).map(|_| ())
    }

    pub fn required_proposal_workers(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        proposal: &PipelineProposalV1,
    ) -> Result<BTreeSet<String>, String> {
        let (_, _, graph) = self.proposal_structure(cas, task, proposal)?;
        Ok(graph
            .slots
            .values()
            .map(|slot| slot.worker.clone())
            .collect())
    }

    /// Full compiler feedback, including effective binding independence. No executable plan
    /// or compiler capability escapes; installation still requires the selected Store proof.
    pub fn check_pipeline_proposal(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        proposal: &PipelineProposalV1,
    ) -> Result<(), String> {
        let (preview, normalized, _) = self.proposal_structure(cas, task, proposal)?;
        let revision_id = cas
            .put_artifact(
                review_core::task::TASK_REVISION_V1,
                capture_producer(),
                vec![],
                None,
                serde_json::to_value(normalized).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        preview.compile_inner(cas, &revision_id, &proposal.root, None)?;
        Ok(())
    }

    fn install_proposal_bytes(
        &mut self,
        cas: &Cas,
        proposal: &PipelineProposalV1,
        proposal_id: &str,
        bootstrap_plan_id: &str,
    ) -> Result<(), String> {
        proposal.validate()?;
        for (name, source) in &proposal.definitions {
            let definition =
                super::super::parse_task_pipeline(source).map_err(|e| e.to_string())?;
            if definition.name != *name || definition.nodes.iter().any(|node|
                matches!(&node.operator, TaskOperatorV1::PlanningContext {})
                || matches!(&node.operator, TaskOperatorV1::Call { pipeline, .. } if pipeline == PLANNER_PIPELINE)
            ) {
                return Err("Generated Pipeline changed its name or invokes the internal Planner".into());
            }
            if let Some(existing) = self.packages.get(name) {
                if self.generated.get(name).is_some_and(|origin| {
                    origin.proposal_id == proposal_id
                        && origin.bootstrap_plan_id == bootstrap_plan_id
                }) && existing.bytes.files.get("pipeline.toml").map(Vec::as_slice)
                    == Some(source.as_bytes())
                {
                    continue;
                }
                return Err("Generated Pipeline cannot shadow a captured package".into());
            }
            let files = BTreeMap::from([("pipeline.toml".into(), source.as_bytes().to_vec())]);
            let id = self.capture_package(
                cas,
                name,
                &TaskPackagePin {
                    version: definition.version,
                    digest: crate::lock::package_digest_from_files(&files),
                    path: "proposal".into(),
                },
                &BTreeMap::from([("proposal/pipeline.toml".into(), source.as_bytes().to_vec())]),
            )?;
            self.generated.insert(
                name.clone(),
                GeneratedOriginV1 {
                    pipeline_id: id,
                    proposal_id: proposal_id.into(),
                    bootstrap_plan_id: bootstrap_plan_id.into(),
                },
            );
        }
        self.validate_dependency_closure()?;
        Ok(())
    }

    pub fn install_planning_bootstrap(
        &mut self,
        cas: &Cas,
        task: &TaskRevisionV1,
        settings: &PlannerSettings,
    ) -> Result<String, String> {
        settings.validate()?;
        task.validate()?;
        if task.authority.policy_id != self.policy_id {
            return Err("Planner belongs to another Task authority".into());
        }
        let worker = self
            .workers
            .get(&settings.worker)
            .ok_or("Planner Worker is not captured")?;
        let signature = &worker.signature;
        let request = port(PLANNING_REQUEST_V1, review_core::PortCardinality::One);
        let proposal = port(PIPELINE_PROPOSAL_V1, review_core::PortCardinality::One);
        if !signature.roles.contains("plan")
            || !signature.effects.is_empty()
            || !signature.evidence.is_empty()
            || signature.worker_input_type.as_deref() != Some("af/PlannerInput@1")
            || signature.worker_output_type.as_deref() != Some(PIPELINE_PROPOSAL_V1)
            || signature.contract.inputs != BTreeMap::from([("request".into(), request.clone())])
            || signature.contract.outputs != BTreeMap::from([("proposal".into(), proposal.clone())])
            || signature.retains
                != BTreeMap::from([("proposal".into(), BTreeSet::from(["request".into()]))])
        {
            return Err(
                "Planner Worker must implement the fixed data-only request/proposal contract"
                    .into(),
            );
        }
        let provider_attempt = u32::from(matches!(worker.runner, TaskWorkerRunner::Model { .. }));
        let definition = PipelineDefinitionV1 {
            schema: PipelineSchemaV1::V1,
            name: PLANNER_PIPELINE.into(),
            version: "1.0.0".into(),
            contract: PipelineContractV1 {
                inputs: task
                    .inputs
                    .iter()
                    .map(|(name, input)| {
                        (name.clone(), port(&input.artifact_type, input.cardinality))
                    })
                    .collect(),
                outputs: BTreeMap::from([("proposal".into(), proposal)]),
            },
            accepts: PipelineApplicabilityV1 {
                kinds: BTreeSet::from([task.kind.clone()]),
                required_facts: BTreeMap::new(),
            },
            slots: BTreeMap::from([(
                "planner".into(),
                WorkerSlotV1 {
                    worker: settings.worker.clone(),
                    role: "plan".into(),
                    input_type: "af/PlannerInput@1".into(),
                    output_type: PIPELINE_PROPOSAL_V1.into(),
                    min_attempts: 1,
                    max_attempts: settings.max_attempts,
                    allow_local_replacement: false,
                    independent_from: BTreeSet::new(),
                },
            )]),
            nodes: vec![
                TaskNodeV1 {
                    id: "context".into(),
                    operator: TaskOperatorV1::PlanningContext {},
                    inputs: BTreeMap::new(),
                    when: None,
                },
                TaskNodeV1 {
                    id: "plan".into(),
                    operator: TaskOperatorV1::Worker {
                        slot: "planner".into(),
                    },
                    inputs: BTreeMap::from([(
                        "request".into(),
                        ValueRefV1::Node {
                            node: "context".into(),
                            port: "request".into(),
                        },
                    )]),
                    when: None,
                },
            ],
            outputs: BTreeMap::from([(
                "proposal".into(),
                ValueRefV1::Node {
                    node: "plan".into(),
                    port: "proposal".into(),
                },
            )]),
            coverage: BTreeMap::new(),
            max_attempts: settings.max_attempts + provider_attempt,
            max_parallel: 1,
        };
        definition.validate()?;
        if self.packages.contains_key(PLANNER_PIPELINE) {
            if self.preparation_roots.contains(PLANNER_PIPELINE)
                && self.pipelines.get(PLANNER_PIPELINE) == Some(&definition)
            {
                return Ok(PLANNER_PIPELINE.into());
            }
            return Err("Captured package collides with the installed Planner bootstrap".into());
        }
        let bytes = toml::to_string(&definition)
            .map_err(|e| e.to_string())?
            .into_bytes();
        let files = BTreeMap::from([("pipeline.toml".into(), bytes.clone())]);
        let pin = TaskPackagePin {
            version: "1.0.0".into(),
            digest: crate::lock::package_digest_from_files(&files),
            path: "bootstrap".into(),
        };
        self.capture_package(
            cas,
            PLANNER_PIPELINE,
            &pin,
            &BTreeMap::from([("bootstrap/pipeline.toml".into(), bytes)]),
        )?;
        self.signatures.insert(
            "operator/planning-context".into(),
            planning_context_signature(),
        );
        self.preparation_roots.insert(PLANNER_PIPELINE.into());
        Ok(PLANNER_PIPELINE.into())
    }

    pub(super) fn compile_graph(
        &self,
        task: &TaskRevisionV1,
        root: &str,
    ) -> Result<CompiledTask, String> {
        let preparation = self.preparation_roots.contains(root);
        let context = CompileContext {
            pipelines: &self.pipelines,
            signatures: &self.signatures,
            slot_workers: if preparation {
                BTreeMap::new()
            } else {
                self.slot_workers.clone()
            },
            acceptance_outputs: self.acceptance_outputs.clone(),
            max_nodes: 64,
            max_depth: 4,
        };
        if preparation {
            let (graph, resources) =
                review_graph::task::compile_task_preparation(task, root, &context)?;
            if !resources.is_empty() {
                return Err(resources.join("; "));
            }
            graph.budget(task.limits.clone())?;
            Ok(graph)
        } else {
            compile_task(task, root, &context)
        }
    }
}
