//! Compile package-defined data contracts through the ordinary Task graph.
use super::catalog::TaskPlanCompiler;
use review_core::PortCardinality;
use review_core::task::pipeline::*;
use review_core::task::structured::*;
use review_graph::task::{CompiledOperator, CompiledTask, OperatorSignature};
use review_store::Cas;
use std::collections::{BTreeMap, BTreeSet};

pub fn port(ty: &str) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        optional: false,
        affinity: PortAffinityV1::Unbound {},
        root_default: None,
        brief: None,
        covers: BTreeSet::new(),
    }
}
pub fn root_contract(contract: &StructuredContractV2) -> PipelineContractV1 {
    let mut inputs = contract
        .inputs
        .iter()
        .map(|(n, p)| (n.clone(), port(&p.artifact_type)))
        .collect::<BTreeMap<_, _>>();
    inputs.insert("requirements".into(), port("af/Requirements@1"));
    PipelineContractV1 {
        inputs,
        outputs: contract
            .outputs
            .iter()
            .map(|(n, p)| (n.clone(), port(&p.artifact_type)))
            .collect(),
    }
}
pub fn signature(id: &str, contract: &StructuredContractV2) -> Result<OperatorSignature, String> {
    validate_schemas(contract)?;
    if !review_core::is_digest(id) {
        return Err(
            "Structured acceptance requires an exact captured kind package identity".into(),
        );
    }
    let root = root_contract(contract);
    let mut inputs = root.inputs;
    inputs.extend(
        root.outputs
            .iter()
            .map(|(n, p)| (format!("result_{n}"), p.clone())),
    );
    let mut outputs = root.outputs;
    outputs.insert("validation".into(), port(STRUCTURED_VALIDATION_V1));
    let retains = outputs
        .keys()
        .map(|n| {
            (
                n.clone(),
                if n == "validation" {
                    inputs.keys().cloned().collect()
                } else {
                    BTreeSet::from([format!("result_{n}")])
                },
            )
        })
        .collect();
    Ok(OperatorSignature {
        contract: PipelineContractV1 { inputs, outputs },
        effects: BTreeSet::new(),
        evidence: BTreeMap::from([("validation".into(), BTreeSet::from([id.into()]))]),
        retains,
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        attempt: None,
        outcome_port: None,
        unbound_allowed: BTreeSet::new(),
    })
}
pub fn parse_contract(bytes: &[u8]) -> Result<StructuredContractV2, String> {
    if bytes.len() > 262144 {
        return Err("Structured contract file exceeds 256 KiB".into());
    }
    let contract: StructuredContractV2 =
        serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    validate_schemas(&contract)?;
    Ok(contract)
}
pub fn validate_schemas(contract: &StructuredContractV2) -> Result<(), String> {
    contract.validate()?;
    for p in contract.inputs.values().chain(contract.outputs.values()) {
        review_runner::task::schema_validator(&p.schema)?;
    }
    Ok(())
}
impl TaskPlanCompiler {
    /// Captured schemas belong to the kind package, never to a Task's untrusted input.
    pub fn structured_contract(&self, name: &str) -> Result<StructuredContractV2, String> {
        let kind = self.task_kind(name).ok_or("Missing structured kind")?;
        if kind.profile != super::kind::TaskKindProfile::Structured {
            return Err("Kind is not structured".into());
        }
        let files = self
            .package_files(name)
            .ok_or("Missing structured package")?;
        parse_contract(
            files
                .get("structured.json")
                .ok_or("Structured kind requires structured.json")?,
        )
    }
    pub(super) fn validate_structured(
        &self,
        cas: &Cas,
        task: &review_core::task::TaskRevisionV1,
        graph: &CompiledTask,
    ) -> Result<(), String> {
        let Some(name) = &self.active_kind else {
            return Ok(());
        };
        if self.kinds[name].profile != super::kind::TaskKindProfile::Structured {
            return Ok(());
        }
        let contract = self.structured_contract(name)?;
        let root = root_contract(&contract);
        if !task.authority.allowed_effects.is_empty()
            || graph.nodes.len() > 4
            || graph.slots.len() != 1
            || task.inputs.keys().ne(root.inputs.keys())
        {
            return Err("Structured Tasks require exactly one data-only Worker and one installed acceptance, with every declared root input".into());
        }
        let mut workers = 0;
        let mut accepts = 0;
        for node in graph.nodes.values() {
            match &node.operator {
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::Worker { slot },
                    ..
                } => {
                    workers += 1;
                    let worker = &self.workers[&graph.slots[slot].worker];
                    if node.contract != root
                        || !worker.signature.effects.is_empty()
                        || !worker.signature.evidence.is_empty()
                        || worker.signature.roles
                            != BTreeSet::from([graph.slots[slot].role.clone()])
                        || worker.signature.retains
                            != root
                                .outputs
                                .keys()
                                .map(|n| (n.clone(), root.inputs.keys().cloned().collect()))
                                .collect()
                    {
                        return Err("Structured Worker cannot grant effects, receipts, roles or source trust".into());
                    }
                    // The normal adapter applies these exact package output schemas too.
                    for (port, declaration) in &contract.outputs {
                        if self
                            .worker_schema(&worker.name, &format!("outputs/{port}.schema.json"))?
                            != declaration.schema
                        {
                            return Err(
                                "Structured Worker output schema differs from kind contract".into(),
                            );
                        }
                    }
                    self.worker_contract(cas, &worker.name, &BTreeSet::new())?;
                }
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::StructuredAccept {},
                    ..
                } => {
                    accepts += 1;
                }
                CompiledOperator::RootInputs | CompiledOperator::ProviderAdmission { .. } => {}
                _ => return Err(
                    "Structured Tasks permit only their Worker and installed structural acceptance"
                        .into(),
                ),
            }
        }
        if workers != 1 || accepts != 1 {
            return Err("Structured Task needs one Worker and one acceptance".into());
        }
        for (name, declaration) in &contract.inputs {
            let value = task
                .inputs
                .get(name)
                .ok_or("Missing structured root input")?;
            validate_value(cas, value, declaration)?;
        }
        Ok(())
    }
}
pub fn validate_value(
    cas: &Cas,
    value: &review_core::task::ArtifactInputV1,
    declaration: &StructuredPortV1,
) -> Result<serde_json::Value, String> {
    value.validate()?;
    if value.artifact_type != declaration.artifact_type
        || value.cardinality != PortCardinality::One
        || value.artifact_ids.len() != 1
        || value.snapshot_id.is_some()
    {
        return Err("Structured port changed type, cardinality or source affinity".into());
    }
    let envelope = cas
        .get_artifact(&value.artifact_ids[0])
        .map_err(|e| e.to_string())?;
    if envelope.artifact_type != declaration.artifact_type || envelope.subject_snapshot_id.is_some()
    {
        return Err("Structured artifact changed its typed source contract".into());
    }
    validate_payload(&declaration.schema, &envelope.payload)?;
    Ok(envelope.payload)
}
pub fn validate_payload(
    schema: &serde_json::Value,
    payload: &serde_json::Value,
) -> Result<(), String> {
    if serde_json::to_vec(payload)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_STRUCTURED_BYTES
    {
        return Err("Structured payload exceeds 64 KiB".into());
    }
    review_runner::task::schema_validator(schema)?
        .validate(payload)
        .map_err(|e| format!("Structured payload: {e}"))
}
