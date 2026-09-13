//! Software starters are assembled from the same typed contracts used by admission.
use super::*;
use review_core::task::{repair::*, review::*, verification::*};
use review_source_git::task::{CANDIDATE_TREE_V1, SOURCE_TREE_V1};
use serde_json::Value;

const GOAL: &str = "Implement offset/limit pagination with nonnegative integer bounds.";
type Ports = BTreeMap<String, PipelinePortV1>;
type Inputs = BTreeMap<String, ValueRefV1>;

fn same(ty: &str) -> PipelinePortV1 {
    let mut value = port(ty);
    value.affinity = PortAffinityV1::SameAs {
        input: "source".into(),
    };
    value
}
fn derived(ty: &str) -> PipelinePortV1 {
    let mut value = port(ty);
    value.affinity = PortAffinityV1::DerivedFrom {
        input: "source".into(),
    };
    value
}
fn optional_requirements() -> PipelinePortV1 {
    let mut value = port("af/Requirements@1");
    value.optional = true;
    value
}
fn history() -> PipelinePortV1 {
    let mut value = port(REVIEW_HISTORY_V1);
    value.root_default = Some(RootDefaultV1::EmptyReviewHistory);
    value
}
fn ports(values: &[(&str, PipelinePortV1)]) -> Ports {
    values
        .iter()
        .map(|(name, port)| ((*name).into(), port.clone()))
        .collect()
}
fn inputs(values: &[(&str, ValueRefV1)]) -> Inputs {
    values
        .iter()
        .map(|(name, value)| ((*name).into(), value.clone()))
        .collect()
}
fn node(id: &str, operator: TaskOperatorV1, bound: Inputs) -> TaskNodeV1 {
    TaskNodeV1 {
        id: id.into(),
        operator,
        inputs: bound,
        when: None,
    }
}
fn when(mut node: TaskNodeV1, condition: &str, outcome: ReceiptOutcomeV1) -> TaskNodeV1 {
    node.when = Some(NodeConditionV1 {
        node: condition.into(),
        outcome,
    });
    node
}
fn definition(
    name: &str,
    kind: &str,
    incoming: Ports,
    outgoing: Ports,
    attempts: u32,
) -> PipelineDefinitionV1 {
    PipelineDefinitionV1 {
        schema: PipelineSchemaV1::V1,
        name: format!("builtin/{name}"),
        version: "1.0.0".into(),
        contract: PipelineContractV1 {
            inputs: incoming,
            outputs: outgoing,
        },
        accepts: PipelineApplicabilityV1 {
            kinds: BTreeSet::from([kind.into()]),
            required_facts: BTreeMap::new(),
        },
        slots: BTreeMap::new(),
        nodes: vec![],
        outputs: BTreeMap::new(),
        coverage: BTreeMap::new(),
        max_attempts: attempts,
        max_parallel: 2,
    }
}
fn cover(pipeline: &mut PipelineDefinitionV1, obligation: &str, port: &str) {
    pipeline
        .contract
        .outputs
        .get_mut(port)
        .expect("declared evidence")
        .covers
        .insert(obligation.into());
    pipeline
        .coverage
        .insert(obligation.into(), pipeline.outputs[port].clone());
}
fn slot(
    worker: &TaskWorkerManifest,
    role: &str,
    independent: &[&str],
    maximum: u32,
) -> WorkerSlotV1 {
    WorkerSlotV1 {
        worker: worker.name.clone(),
        role: role.into(),
        input_type: worker
            .signature
            .worker_input_type
            .clone()
            .expect("Worker input type"),
        output_type: worker
            .signature
            .worker_output_type
            .clone()
            .expect("Worker output type"),
        min_attempts: 1,
        max_attempts: maximum,
        allow_local_replacement: true,
        independent_from: independent.iter().map(|name| (*name).into()).collect(),
    }
}
fn worker_definition(
    name: &str,
    role: &str,
    incoming: Ports,
    outgoing: Ports,
    input_type: &str,
    output_type: &str,
    writes: bool,
) -> TaskWorkerManifest {
    TaskWorkerManifest {
        schema: "af.worker/1".into(),
        name: format!("builtin/{name}"),
        version: "1.0.0".into(),
        signature: OperatorSignature {
            retains: outgoing
                .keys()
                .map(|p| (p.clone(), incoming.keys().cloned().collect()))
                .collect(),
            contract: PipelineContractV1 {
                inputs: incoming,
                outputs: outgoing,
            },
            effects: if role == "plan" {
                BTreeSet::new()
            } else {
                BTreeSet::from([if writes {
                    "write-source"
                } else {
                    "read-source"
                }
                .into()])
            },
            evidence: BTreeMap::new(),
            roles: BTreeSet::from([role.into()]),
            worker_input_type: Some(input_type.into()),
            worker_output_type: Some(output_type.into()),
            outcome_port: None,
            attempt: Some(OperatorAttemptCost {
                tokens: 0,
                wall_ms: 5000,
            }),
        },
        runner: TaskWorkerRunner::Command {
            command: review_config::CommandSpec {
                program: "python3".into(),
                args: ["-B", "@package/worker.py"]
                    .into_iter()
                    .map(|value| review_config::ArgSpec {
                        value: value.into(),
                        provenance: review_config::ProvenanceSpec::Literal,
                    })
                    .collect(),
            },
        },
    }
}

fn verification(evaluator: &TaskWorkerManifest) -> PipelineDefinitionV1 {
    let mut p = definition(
        "verification",
        "verification",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("requirements", port("af/Requirements@1")),
        ]),
        ports(&[
            ("snapshot", same(SOURCE_TREE_V1)),
            ("verification", same(VERIFICATION_RESULT_V1)),
        ]),
        2,
    );
    p.slots
        .insert("evaluator".into(), slot(evaluator, "evaluate", &[], 1));
    p.nodes = vec![
        node(
            "checks",
            TaskOperatorV1::Check {
                checks: BTreeSet::from(["pagination".into()]),
            },
            inputs(&[("source", input("source"))]),
        ),
        when(
            node(
                "evaluate",
                TaskOperatorV1::Verify {
                    slot: "evaluator".into(),
                },
                inputs(&[
                    ("source", input("source")),
                    ("requirements", input("requirements")),
                    ("checks", output("checks", "result")),
                ]),
            ),
            "checks",
            ReceiptOutcomeV1::Passed,
        ),
        node(
            "accept",
            TaskOperatorV1::Accept {},
            inputs(&[
                ("source", input("source")),
                ("checks", output("checks", "result")),
                ("evaluation", output("evaluate", "result")),
            ]),
        ),
    ];
    p.outputs = inputs(&[
        ("snapshot", output("accept", "snapshot")),
        ("verification", output("accept", "result")),
    ]);
    cover(&mut p, "verified", "verification");
    p
}

fn implementation(
    implementer: &TaskWorkerManifest,
    evaluator: &TaskWorkerManifest,
    heavy: bool,
) -> PipelineDefinitionV1 {
    let maximum = if heavy { 2 } else { 1 };
    let mut p = definition(
        if heavy {
            "implementation-heavy"
        } else {
            "implementation-small"
        },
        "implement",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("requirements", port("af/Requirements@1")),
        ]),
        ports(&[
            ("snapshot", derived(SOURCE_TREE_V1)),
            ("verification", derived(VERIFICATION_RESULT_V1)),
        ]),
        maximum + 2,
    );
    p.accepts
        .required_facts
        .insert("standard".into(), TaskFactV1::Boolean(true));
    p.slots = BTreeMap::from([
        (
            "implementer".into(),
            slot(implementer, "implement", &[], maximum),
        ),
        (
            "evaluator".into(),
            slot(evaluator, "evaluate", &["implementer"], 1),
        ),
    ]);
    p.nodes = vec![
        node(
            "implement",
            TaskOperatorV1::Worker {
                slot: "implementer".into(),
            },
            inputs(&[
                ("source", input("source")),
                ("requirements", input("requirements")),
            ]),
        ),
        node(
            "seal",
            TaskOperatorV1::Seal {},
            inputs(&[("candidate", output("implement", "candidate"))]),
        ),
        node(
            "verification",
            TaskOperatorV1::Call {
                pipeline: "builtin/verification".into(),
                bindings: BTreeMap::from([("evaluator".into(), "evaluator".into())]),
            },
            inputs(&[
                ("source", output("seal", "snapshot")),
                ("requirements", input("requirements")),
            ]),
        ),
    ];
    p.outputs = inputs(&[
        ("snapshot", output("verification", "snapshot")),
        ("verification", output("verification", "verification")),
    ]);
    cover(&mut p, "verified", "verification");
    p
}

fn review(correctness: &TaskWorkerManifest, bugs: &TaskWorkerManifest) -> PipelineDefinitionV1 {
    let mut base = port(SOURCE_TREE_V1);
    base.optional = true;
    let mut continuation = same(TASK_REVIEW_CONTINUATION_V1);
    continuation.optional = true;
    let mut p = definition(
        "review-light",
        "review",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("base", base),
            ("history", history()),
            ("continuation", continuation),
            ("requirements", optional_requirements()),
        ]),
        ports(&[
            ("review", same(TASK_REVIEW_ROUND_V1)),
            ("checks", same(TASK_CHECK_RECEIPT_V1)),
            ("history", port(REVIEW_HISTORY_V1)),
            ("findings", same(TASK_REVIEW_CLAIMS_V1)),
        ]),
        3,
    );
    p.nodes = vec![
        node(
            "bind",
            TaskOperatorV1::ReviewBind {},
            inputs(&[
                ("source", input("source")),
                ("base", input("base")),
                ("history", input("history")),
                ("continuation", input("continuation")),
            ]),
        ),
        node(
            "checks",
            TaskOperatorV1::Check {
                checks: BTreeSet::from(["pagination".into()]),
            },
            inputs(&[("source", input("source"))]),
        ),
    ];
    let mut reduction = inputs(&[
        ("source", input("source")),
        ("history", input("history")),
        ("subject", output("bind", "subject")),
        ("checks", output("checks", "result")),
    ]);
    for (name, worker) in [("correctness", correctness), ("bugs", bugs)] {
        p.slots.insert(name.into(), slot(worker, "review", &[], 1));
        p.nodes.push(when(
            node(name, TaskOperatorV1::Verify { slot: name.into() }, {
                let mut incoming = reduction.clone();
                incoming.insert("requirements".into(), input("requirements"));
                incoming.insert("assignment".into(), output("bind", name));
                incoming
            }),
            "checks",
            ReceiptOutcomeV1::Passed,
        ));
    }
    for name in ["correctness", "bugs"] {
        reduction.insert(name.into(), output(name, "result"));
    }
    p.nodes
        .push(node("reduce", TaskOperatorV1::ReviewReduce {}, reduction));
    p.outputs = inputs(&[
        ("review", output("reduce", "result")),
        ("checks", output("checks", "result")),
        ("history", output("reduce", "history")),
        ("findings", output("reduce", "findings")),
    ]);
    cover(&mut p, "reviewed", "review");
    p
}

fn review_heavy(light: &PipelineDefinitionV1) -> PipelineDefinitionV1 {
    let mut p = light.clone();
    p.name = "builtin/review-heavy".into();
    p.max_attempts = 6;
    p.nodes.clear();
    let bindings = p
        .slots
        .keys()
        .map(|k| (k.clone(), k.clone()))
        .collect::<BTreeMap<_, _>>();
    for (name, prior) in [("first", None), ("second", Some("first"))] {
        let mut incoming: Inputs = p
            .contract
            .inputs
            .keys()
            .map(|k| (k.clone(), input(k)))
            .collect();
        if let Some(prior) = prior {
            incoming.insert("history".into(), output(prior, "history"));
            incoming.remove("continuation");
        }
        p.nodes.push(node(
            name,
            TaskOperatorV1::Call {
                pipeline: light.name.clone(),
                bindings: bindings.clone(),
            },
            incoming,
        ));
    }
    p.outputs = p
        .contract
        .outputs
        .keys()
        .map(|k| (k.clone(), output("second", k)))
        .collect();
    p.coverage = BTreeMap::from([("reviewed".into(), output("second", "review"))]);
    p
}

fn reviewed(
    implementer: &TaskWorkerManifest,
    correctness: &TaskWorkerManifest,
    bugs: &TaskWorkerManifest,
) -> PipelineDefinitionV1 {
    let mut p = definition(
        "implementation-reviewed",
        "implement",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("requirements", port("af/Requirements@1")),
            ("history", history()),
        ]),
        ports(&[
            ("snapshot", derived(SOURCE_TREE_V1)),
            ("verification", derived(REVIEWED_IMPLEMENTATION_V1)),
        ]),
        4,
    );
    p.accepts
        .required_facts
        .insert("standard".into(), TaskFactV1::Boolean(true));
    p.slots
        .insert("implementer".into(), slot(implementer, "implement", &[], 1));
    for (name, w) in [("correctness", correctness), ("bugs", bugs)] {
        p.slots
            .insert(name.into(), slot(w, "review", &["implementer"], 1));
    }
    p.nodes = vec![
        node(
            "implement",
            TaskOperatorV1::Worker {
                slot: "implementer".into(),
            },
            inputs(&[
                ("source", input("source")),
                ("requirements", input("requirements")),
            ]),
        ),
        node(
            "seal",
            TaskOperatorV1::Seal {},
            inputs(&[("candidate", output("implement", "candidate"))]),
        ),
        node(
            "review",
            TaskOperatorV1::Call {
                pipeline: "builtin/review-light".into(),
                bindings: BTreeMap::from([
                    ("correctness".into(), "correctness".into()),
                    ("bugs".into(), "bugs".into()),
                ]),
            },
            inputs(&[
                ("source", output("seal", "snapshot")),
                ("base", input("source")),
                ("history", input("history")),
                ("requirements", input("requirements")),
            ]),
        ),
        node(
            "accept",
            TaskOperatorV1::ReviewAccept {},
            inputs(&[
                ("source", output("seal", "snapshot")),
                ("review", output("review", "review")),
                ("checks", output("review", "checks")),
            ]),
        ),
    ];
    p.outputs = inputs(&[
        ("snapshot", output("accept", "snapshot")),
        ("verification", output("accept", "result")),
    ]);
    cover(&mut p, "verified", "verification");
    p
}

fn repair(
    repairer: &TaskWorkerManifest,
    verifier: &TaskWorkerManifest,
    heavy: bool,
) -> PipelineDefinitionV1 {
    let mut p = definition(
        if heavy {
            "repair-heavy"
        } else {
            "repair-targeted"
        },
        "repair",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("requirements", port("af/Requirements@1")),
            ("history", port(REVIEW_HISTORY_V1)),
            ("review", same(TASK_REVIEW_ROUND_V1)),
            ("findings", same(TASK_REVIEW_CLAIMS_V1)),
        ]),
        ports(&[("snapshot", derived(SOURCE_TREE_V1))]),
        3,
    );
    p.slots = BTreeMap::from([
        ("repairer".into(), slot(repairer, "repair", &[], 1)),
        (
            "fix-verifier".into(),
            slot(verifier, "fix-verify", &["repairer"], 1),
        ),
    ]);
    p.nodes = vec![
        node(
            "repair",
            TaskOperatorV1::Worker {
                slot: "repairer".into(),
            },
            inputs(&[
                ("source", input("source")),
                ("requirements", input("requirements")),
                ("review", input("findings")),
            ]),
        ),
        node(
            "seal",
            TaskOperatorV1::Seal {},
            inputs(&[("candidate", output("repair", "candidate"))]),
        ),
        node(
            "attest",
            TaskOperatorV1::AttestFixes {},
            inputs(&[
                ("source", output("seal", "snapshot")),
                ("previous", input("source")),
                ("history", input("history")),
                ("review", input("review")),
            ]),
        ),
        node(
            "checks",
            TaskOperatorV1::Check {
                checks: BTreeSet::from(["pagination".into()]),
            },
            inputs(&[("source", output("seal", "snapshot"))]),
        ),
        when(
            node(
                "verify",
                TaskOperatorV1::FixVerify {
                    slot: "fix-verifier".into(),
                },
                inputs(&[
                    ("source", output("seal", "snapshot")),
                    ("repair", output("attest", "repair")),
                    ("checks", output("checks", "result")),
                ]),
            ),
            "checks",
            ReceiptOutcomeV1::Passed,
        ),
        node(
            "finish",
            if heavy {
                TaskOperatorV1::ReviewContinue {}
            } else {
                TaskOperatorV1::RepairAccept {}
            },
            inputs(&[
                ("source", output("seal", "snapshot")),
                ("repair", output("attest", "repair")),
                ("checks", output("checks", "result")),
                ("verification", output("verify", "result")),
            ]),
        ),
    ];
    if heavy {
        p.contract
            .outputs
            .insert("continuation".into(), derived(TASK_REVIEW_CONTINUATION_V1));
        p.outputs = inputs(&[
            ("snapshot", output("seal", "snapshot")),
            ("continuation", output("finish", "continuation")),
        ]);
    } else {
        p.contract
            .outputs
            .insert("result".into(), derived(REPAIR_ALLOWED_IMPLEMENTATION_V1));
        p.outputs = inputs(&[
            ("snapshot", output("finish", "snapshot")),
            ("result", output("finish", "result")),
        ]);
        cover(&mut p, "repaired", "result");
    }
    p.contract
        .outputs
        .insert("checks".into(), derived(TASK_CHECK_RECEIPT_V1));
    p.outputs
        .insert("checks".into(), output("checks", "result"));
    p
}

fn reviewed_repair(
    base: &PipelineDefinitionV1,
    repairer: &TaskWorkerManifest,
    verifier: &TaskWorkerManifest,
    heavy: bool,
) -> PipelineDefinitionV1 {
    let mut p = base.clone();
    p.name = format!(
        "builtin/implementation-repair-{}",
        if heavy { "heavy" } else { "targeted" }
    );
    p.max_attempts = if heavy { 10 } else { 7 };
    p.slots
        .insert("repairer".into(), slot(repairer, "repair", &[], 1));
    p.slots.insert(
        "fix-verifier".into(),
        slot(verifier, "fix-verify", &["implementer", "repairer"], 1),
    );
    for name in ["correctness", "bugs"] {
        p.slots
            .get_mut(name)
            .unwrap()
            .independent_from
            .insert("repairer".into());
    }
    p.nodes.push(when(
        node(
            "repair",
            TaskOperatorV1::Call {
                pipeline: format!(
                    "builtin/repair-{}",
                    if heavy { "heavy" } else { "targeted" }
                ),
                bindings: BTreeMap::from([
                    ("repairer".into(), "repairer".into()),
                    ("fix-verifier".into(), "fix-verifier".into()),
                ]),
            },
            inputs(&[
                ("source", output("seal", "snapshot")),
                ("requirements", input("requirements")),
                ("history", output("review", "history")),
                ("review", output("review", "review")),
                ("findings", output("review", "findings")),
            ]),
        ),
        "accept",
        ReceiptOutcomeV1::Failed,
    ));
    let (repair_node, success_port) = if heavy {
        let mut next = p.nodes.iter().find(|n| n.id == "review").unwrap().clone();
        next.id = "second_review".into();
        next.inputs
            .insert("source".into(), output("repair", "snapshot"));
        next.inputs
            .insert("history".into(), output("review", "history"));
        next.inputs
            .insert("continuation".into(), output("repair", "continuation"));
        p.nodes.push(when(next, "accept", ReceiptOutcomeV1::Failed));
        p.nodes.push(when(
            node(
                "second_accept",
                TaskOperatorV1::ReviewAccept {},
                inputs(&[
                    ("source", output("repair", "snapshot")),
                    ("review", output("second_review", "review")),
                    ("checks", output("second_review", "checks")),
                ]),
            ),
            "accept",
            ReceiptOutcomeV1::Failed,
        ));
        ("second_accept", "result")
    } else {
        p.contract
            .outputs
            .get_mut("verification")
            .unwrap()
            .artifact_type = REPAIR_ALLOWED_IMPLEMENTATION_V1.into();
        ("repair", "repair_result")
    };
    for (name, original, repaired) in [
        ("final_snapshot", "snapshot", "snapshot"),
        ("final_verification", success_port, "result"),
    ] {
        p.nodes.push(node(
            name,
            TaskOperatorV1::Select {},
            inputs(&[
                ("condition", output("accept", "result")),
                ("passed", output("accept", original)),
                ("failed", output(repair_node, repaired)),
                ("inconclusive", output("accept", original)),
            ]),
        ));
    }
    p.outputs = inputs(&[
        ("snapshot", output("final_snapshot", "output")),
        ("verification", output("final_verification", "output")),
    ]);
    p.coverage = BTreeMap::from([("verified".into(), output("final_verification", "output"))]);
    p.nodes.push(node(
        "final_checks",
        TaskOperatorV1::Select {},
        inputs(&[
            ("condition", output("accept", "result")),
            ("passed", output("review", "checks")),
            (
                "failed",
                output(if heavy { "second_review" } else { "repair" }, "checks"),
            ),
            ("inconclusive", output("review", "checks")),
        ]),
    ));
    p
}

// Review and goal acceptance refer to the selected final Snapshot. Keep Review's original
// Snapshot artifact and reuse its exact checks; the evaluator has one protected Attempt.
fn with_goal_acceptance(
    mut p: PipelineDefinitionV1,
    evaluator: &TaskWorkerManifest,
) -> PipelineDefinitionV1 {
    let authors: Vec<_> = ["implementer", "repairer"]
        .into_iter()
        .filter(|s| p.slots.contains_key(*s))
        .collect();
    p.slots
        .insert("evaluator".into(), slot(evaluator, "evaluate", &authors, 1));
    let snapshot = p.outputs["snapshot"].clone();
    let (checks, condition) = if p.nodes.iter().any(|n| n.id == "final_checks") {
        (output("final_checks", "output"), "final_checks")
    } else {
        p.nodes.push(node(
            "final_checks",
            TaskOperatorV1::Select {},
            inputs(&[
                ("condition", output("accept", "result")),
                ("passed", output("review", "checks")),
                ("failed", output("review", "checks")),
                ("inconclusive", output("review", "checks")),
            ]),
        ));
        (output("final_checks", "output"), "final_checks")
    };
    p.nodes.push(when(
        node(
            "evaluate_goal",
            TaskOperatorV1::Verify {
                slot: "evaluator".into(),
            },
            inputs(&[
                ("source", snapshot.clone()),
                ("requirements", input("requirements")),
                ("checks", checks.clone()),
            ]),
        ),
        condition,
        ReceiptOutcomeV1::Passed,
    ));
    p.nodes.push(node(
        "accept_goal",
        TaskOperatorV1::Accept {},
        inputs(&[
            ("source", snapshot),
            ("checks", checks),
            ("evaluation", output("evaluate_goal", "result")),
        ]),
    ));
    p.contract
        .outputs
        .insert("evaluation".into(), derived(VERIFICATION_RESULT_V1));
    p.outputs
        .insert("evaluation".into(), output("accept_goal", "result"));
    cover(&mut p, "goal", "evaluation");
    p.max_attempts += 1;
    p
}

fn payload_schema(bytes: &[u8]) -> Result<Value, String> {
    let mut value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or("Invalid public payload schema")?;
    object.remove("$id");
    object.remove("$schema");
    let contracts: Value = serde_json::from_slice(include_bytes!(
        "../../../../../../schemas/task-contracts-v1.json"
    ))
    .map_err(|e| e.to_string())?;
    fn localize(value: &mut Value, digest: &Value) -> Result<(), String> {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    if reference == "urn:af:schema:task-contracts:1#/$defs/digest" {
                        *value = digest.clone();
                        return Ok(());
                    }
                    if reference == "urn:review-kernel:schema:reviewer-result:1#/$defs/legacyReport"
                    {
                        let original: Value = serde_json::from_slice(include_bytes!(
                            "../../../../../../schemas/reviewer-result-v1.json"
                        ))
                        .map_err(|e| e.to_string())?;
                        *value = original["$defs"]["legacyReport"].clone();
                        return Ok(());
                    }
                    if !reference.starts_with('#') {
                        return Err("Unsupported external Worker schema reference".into());
                    }
                }
                for value in object.values_mut() {
                    localize(value, digest)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    localize(value, digest)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    localize(&mut value, &contracts["$defs"]["digest"])?;
    Ok(value)
}

fn worker_input_schema(ports: &Ports) -> Value {
    let mut schema = inputs_schema(ports);
    for (name, port) in ports {
        if port.artifact_type == SOURCE_TREE_V1
            || matches!(port.affinity, PortAffinityV1::SameAs { .. })
        {
            let item = &mut schema["properties"][name]["items"];
            item["properties"]["snapshot_id"] =
                json!({"type":"string","pattern":"^sha256:[0-9a-f]{64}$"});
            item["required"]
                .as_array_mut()
                .unwrap()
                .push(json!("snapshot_id"));
        }
    }
    schema
}

pub(super) fn files(developer_key: Option<String>) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let policy:CodeTaskPolicy=serde_json::from_value(json!({"schema":"af.code-task-policy/1","check_wall_ms":5000,"require_container":false,
        "checks":{"pagination":{"name":"pagination","required":true,"command":{"program":"python3","args":[{"value":"-B","provenance":"literal"},{"value":"-c","provenance":"literal"},{"value":"import pagination; assert pagination.paginate(list(range(7)), 2, 3) == [2, 3, 4]","provenance":"literal"}]}}}})).map_err(|e|e.to_string())?;
    policy.validate()?;
    let policy_id = review_store::canonical::content_id(
        &serde_json::to_value(&policy).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let author_inputs = ports(&[
        ("source", port(SOURCE_TREE_V1)),
        ("requirements", port("af/Requirements@1")),
    ]);
    let author_outputs = ports(&[
        ("candidate", same(CANDIDATE_TREE_V1)),
        ("report", same("af/ImplementationReport@1")),
    ]);
    let author = worker_definition(
        "implementer",
        "implement",
        author_inputs.clone(),
        author_outputs.clone(),
        "af/ImplementationInput@1",
        "af/ImplementationReport@1",
        true,
    );
    let mut evaluator_inputs = author_inputs.clone();
    evaluator_inputs.insert("checks".into(), same(TASK_CHECK_RECEIPT_V1));
    let mut evaluator = worker_definition(
        "evaluator",
        "evaluate",
        evaluator_inputs,
        ports(&[("result", same(TASK_EVALUATION_V1))]),
        "af/EvaluationInput@1",
        TASK_EVALUATION_V1,
        false,
    );
    evaluator.signature.outcome_port = Some("result".into());
    evaluator
        .signature
        .evidence
        .insert("result".into(), BTreeSet::from([policy_id]));
    let reviewer_inputs = ports(&[
        ("requirements", optional_requirements()),
        ("source", port(SOURCE_TREE_V1)),
        ("history", port(REVIEW_HISTORY_V1)),
        ("subject", same(TASK_REVIEW_SUBJECT_V2)),
        ("assignment", same(TASK_REVIEW_ASSIGNMENT_V1)),
        ("checks", same(TASK_CHECK_RECEIPT_V1)),
    ]);
    let reviewer = |name| {
        worker_definition(
            name,
            "review",
            reviewer_inputs.clone(),
            ports(&[("result", same(review_core::contract::REVIEWER_RESULT_V2))]),
            "af/ReviewInput@1",
            review_core::contract::REVIEWER_RESULT_V2,
            false,
        )
    };
    let correctness = reviewer("correctness");
    let bugs = reviewer("bugs");
    let mut repair_inputs = author_inputs;
    repair_inputs.insert("review".into(), same(TASK_REVIEW_CLAIMS_V1));
    let repairer = worker_definition(
        "repairer",
        "repair",
        repair_inputs,
        author_outputs,
        "af/ImplementationInput@1",
        "af/ImplementationReport@1",
        true,
    );
    let fix_verifier = worker_definition(
        "fix-verifier",
        "fix-verify",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("checks", same(TASK_CHECK_RECEIPT_V1)),
            ("repair", same(TASK_REPAIR_CONTEXT_V1)),
        ]),
        ports(&[("result", same(TASK_FIX_VERIFICATION_V1))]),
        "af/FixVerificationInput@1",
        TASK_FIX_VERIFICATION_V1,
        false,
    );
    let planner = worker_definition(
        "planner",
        "plan",
        ports(&[("request", port("af/PlanningRequest@1"))]),
        ports(&[("proposal", port("af/PipelineProposal@1"))]),
        "af/PlannerInput@1",
        "af/PipelineProposal@1",
        false,
    );
    let light = review(&correctness, &bugs);
    let implementation_reviewed = reviewed(&author, &correctness, &bugs);
    let mut generated = with_goal_acceptance(implementation_reviewed.clone(), &evaluator);
    generated.name = "generated/implementation".into();
    generated.accepts.required_facts.clear();
    let proposal = json!({"schema":"af.pipeline-proposal/1","root":generated.name,"definitions":{generated.name.clone():String::from_utf8(toml_bytes(&generated)?).map_err(|e|e.to_string())?}});
    let pipelines = [
        verification(&evaluator),
        implementation(&author, &evaluator, false),
        implementation(&author, &evaluator, true),
        review_heavy(&light),
        light,
        repair(&repairer, &fix_verifier, false),
        repair(&repairer, &fix_verifier, true),
        with_goal_acceptance(
            reviewed_repair(&implementation_reviewed, &repairer, &fix_verifier, false),
            &evaluator,
        ),
        with_goal_acceptance(
            reviewed_repair(&implementation_reviewed, &repairer, &fix_verifier, true),
            &evaluator,
        ),
        with_goal_acceptance(implementation_reviewed, &evaluator),
    ];
    let mut files = BTreeMap::from([
        (".af/code-policy.toml".into(), toml_bytes(&policy)?),
        (
            "pagination.py".into(),
            b"def paginate(items):\n    return list(items)\n".to_vec(),
        ),
    ]);
    let mut packages = BTreeMap::new();
    let mut contracts = CatalogContractFixtures {
        schema: "af.catalog-contract-fixtures/1".into(),
        pipelines: BTreeMap::new(),
        workers: BTreeMap::new(),
        kinds: BTreeMap::new(),
    };
    let report_schema = json!({"type":"object","additionalProperties":false,"required":["summary"],"properties":{"summary":{"type":"string","minLength":1,"maxLength":65536}}});
    let evaluation_schema = payload_schema(include_bytes!(
        "../../../../../../schemas/task-evaluation-v1.json"
    ))?;
    let review_schema = payload_schema(include_bytes!(
        "../../../../../../schemas/reviewer-result-v2.json"
    ))?;
    let fix_schema = payload_schema(include_bytes!(
        "../../../../../../schemas/task-fix-verification-v1.json"
    ))?;
    let proposal_schema = payload_schema(include_bytes!(
        "../../../../../../schemas/pipeline-proposal-v1.json"
    ))?;
    for (worker, script, output_name, schema) in [
        (
            &author,
            include_str!("implement.py"),
            "report",
            &report_schema,
        ),
        (
            &evaluator,
            include_str!("evaluate.py"),
            "result",
            &evaluation_schema,
        ),
        (
            &correctness,
            include_str!("correctness.py"),
            "result",
            &review_schema,
        ),
        (&bugs, include_str!("bugs.py"), "result", &review_schema),
        (
            &repairer,
            include_str!("repair.py"),
            "report",
            &report_schema,
        ),
        (
            &fix_verifier,
            include_str!("fix_verify.py"),
            "result",
            &fix_schema,
        ),
        (
            &planner,
            include_str!("planner.py"),
            "proposal",
            &proposal_schema,
        ),
    ] {
        let mut content = BTreeMap::from([
            ("worker.toml".into(), toml_bytes(worker)?),
            ("worker.py".into(), script.as_bytes().to_vec()),
            (
                "input.schema.json".into(),
                json_bytes(&worker_input_schema(&worker.signature.contract.inputs))?,
            ),
            (
                format!("outputs/{output_name}.schema.json"),
                json_bytes(schema)?,
            ),
        ]);
        if worker.name == planner.name {
            content.insert("proposal.json".into(), json_bytes(&proposal)?);
        }
        package(&mut files, &mut packages, &worker.name, content);
        contracts
            .workers
            .insert(worker.name.clone(), worker.signature.clone());
    }
    for pipeline in &pipelines {
        pipeline.validate()?;
        package(
            &mut files,
            &mut packages,
            &pipeline.name,
            BTreeMap::from([("pipeline.toml".into(), toml_bytes(pipeline)?)]),
        );
        contracts
            .pipelines
            .insert(pipeline.name.clone(), pipeline.into());
    }
    let developers = developer_key.map(|key| developer::DeveloperPolicy {
        schema: "af.task-developers/1".into(),
        keys: BTreeMap::from([("owner".into(), key)]),
    });
    if let Some(developers) = &developers {
        developers.validate()?;
    }
    let catalog = TaskCatalog {
        schema: "af.task-catalog/1".into(),
        provider_admission: None,
        code_policy: Some(".af/code-policy.toml".into()),
        document_policy: None,
        selection: BTreeMap::new(),
        no_match: review_config::task::selection::NoMatchPolicy::Refuse,
        planner: developers.as_ref().map(|_| {
            review_config::task::catalog::planning::PlannerSettings {
                worker: planner.name.clone(),
                max_attempts: 2,
            }
        }),
        developers,
        review: Some(ReviewSettings {
            generation: Some(2),
            reviewers: BTreeMap::from([
                (
                    "correctness".into(),
                    review_core::DemandRequirement::Required,
                ),
                ("bugs".into(), review_core::DemandRequirement::Required),
            ]),
            gate: review_core::Severity::Major,
            clean_rounds: 1,
            max_rounds: 2,
            allow_targeted_repairs: true,
        }),
        packages: packages.clone(),
        kinds: BTreeMap::new(),
        imports: BTreeSet::new(),
        independence: IndependencePolicyV1::default(),
        providers: BTreeMap::new(),
    };
    let shared = SharedTaskCatalog {
        schema: "af.shared-task-catalog/1".into(),
        packages,
        path_base: CatalogPathBase::Repository,
        imports: BTreeSet::new(),
    };
    for (name, kind, verification, attempts, reserved) in [
        ("implementation-small", "implement", None, 3, 2),
        ("implementation-heavy", "implement", None, 4, 2),
        (
            "implementation-reviewed",
            "implement",
            Some(FileVerification::Review),
            5,
            4,
        ),
        (
            "implementation-repair-targeted",
            "implement",
            Some(FileVerification::ReviewOrTargetedFixes),
            8,
            6,
        ),
        (
            "implementation-repair-heavy",
            "implement",
            Some(FileVerification::Review),
            11,
            9,
        ),
        ("review-light", "review", None, 3, 3),
        ("review-heavy", "review", None, 6, 6),
    ] {
        let task = TaskFile {
        issue: None,
            schema: "af.task-file/1".into(),
            task_id: name.into(),
            kind: kind.into(),
            goal: if kind == "review" {
                "Review the current pagination implementation.".into()
            } else {
                GOAL.into()
            },
            requirements: Some(json!({
                "schema": "tutorial.pagination/1", "module": "pagination.py", "function": "paginate",
                "offset_default": 0, "limit_default": 2, "bounds": "nonnegative_integers",
                "preserve_input": true
            }).as_object().unwrap().clone()),
            document_sources: None,
            pipeline: Some(PipelineChoiceV1 {
                name: format!("builtin/{name}"),
                fallback: PipelineFallbackV1::Refuse,
            }),
            strategy: if name.contains("heavy") {
                "heavy".into()
            } else {
                "fast".into()
            },
            verification,
            facts: BTreeMap::from([
                ("standard".into(), TaskFactV1::Boolean(true)),
                ("capability".into(), TaskFactV1::Text("tutorial.pagination/1".into())),
            ]),
            limits: FileLimits {
                tokens: 0,
                max_attempts: attempts,
                wall_ms: 180000,
                verification: VerificationReserveV1 {
                    tokens: 0,
                    attempts: reserved,
                    wall_ms: u64::from(reserved) * 5000,
                },
            },
        };
        files.insert(format!("{name}.json"), json_bytes(&task)?);
    }
    let mut task: TaskFile = serde_json::from_slice(&files["implementation-reviewed.json"])
        .map_err(|e| e.to_string())?;
    task.task_id = "generated-pagination".into();
    task.pipeline.as_mut().unwrap().fallback = PipelineFallbackV1::Generate;
    task.facts
        .insert("standard".into(), TaskFactV1::Boolean(false));
    task.limits.max_attempts = 7;
    task.limits.wall_ms = 900000;
    files.insert("planning.json".into(), json_bytes(&task)?);
    files.insert(".af/task-catalog.toml".into(), toml_bytes(&catalog)?);
    files.insert("catalog.toml".into(), toml_bytes(&shared)?);
    files.insert("contracts.json".into(), json_bytes(&contracts)?);
    files.insert("README.md".into(), include_bytes!("README.md").to_vec());
    Ok(files)
}
