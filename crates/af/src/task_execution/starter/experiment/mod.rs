//! The experiment starter (ADR-0124), assembled from the same typed contracts as admission: a
//! code policy with one measure and one objective, a command implementer that proposes a
//! candidate, and a command evaluator whose contract reads the kernel's comparison. The kernel
//! measures the source, measures the sealed candidate after its checks pass, compares the two,
//! and dispatches the evaluator only for a passed comparison.
use super::software::{
    Ports, cover, definition, derived, inputs, node, ports, same, slot, when, worker_definition,
    worker_input_schema,
};
use super::*;
use review_core::task::measurement::{MEASUREMENT_COMPARISON_V1, MEASUREMENT_V1};
use review_core::task::verification::*;
use review_source_git::task::{CANDIDATE_TREE_V1, SOURCE_TREE_V1};

const GOAL: &str = "Make the measured command write fewer bytes.";

fn optional(mut port: PipelinePortV1) -> PipelinePortV1 {
    port.optional = true;
    port
}

/// The candidate half: measure the sealed source, compare it with the baseline under the
/// `smaller` objective, and evaluate it only when the comparison passed. The parent calls it
/// only after passed checks, so the evaluator is gated on both.
fn trial(evaluator: &TaskWorkerManifest) -> PipelineDefinitionV1 {
    let mut p = definition(
        "experiment-trial",
        "implement",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("requirements", port("af/Requirements@1")),
            ("checks", same(TASK_CHECK_RECEIPT_V1)),
            ("baseline", port(MEASUREMENT_V1)),
        ]),
        ports(&[
            ("candidate", same(MEASUREMENT_V1)),
            ("comparison", same(MEASUREMENT_COMPARISON_V1)),
            ("evaluation", optional(same(TASK_EVALUATION_V1))),
        ]),
        3,
    );
    p.max_parallel = 1;
    p.slots
        .insert("evaluator".into(), slot(evaluator, "evaluate", &[], 1));
    p.nodes = vec![
        node(
            "measure",
            TaskOperatorV1::Measure {
                measures: BTreeSet::from(["write".into()]),
            },
            inputs(&[("source", input("source"))]),
        ),
        node(
            "compare",
            TaskOperatorV1::Compare {
                objective: "smaller".into(),
            },
            inputs(&[
                ("baseline", input("baseline")),
                ("candidate", output("measure", "write")),
            ]),
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
                    ("checks", input("checks")),
                    ("comparison", output("compare", "result")),
                ]),
            ),
            "compare",
            ReceiptOutcomeV1::Passed,
        ),
    ];
    p.outputs = inputs(&[
        ("candidate", output("measure", "write")),
        ("comparison", output("compare", "result")),
        ("evaluation", output("evaluate", "result")),
    ]);
    p
}

fn experiment(author: &TaskWorkerManifest, evaluator: &TaskWorkerManifest) -> PipelineDefinitionV1 {
    let mut p = definition(
        "experiment",
        "implement",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("requirements", port("af/Requirements@1")),
        ]),
        ports(&[
            ("snapshot", derived(SOURCE_TREE_V1)),
            ("verification", derived(VERIFICATION_RESULT_V1)),
            ("baseline", same(MEASUREMENT_V1)),
            ("candidate", optional(derived(MEASUREMENT_V1))),
            ("comparison", optional(derived(MEASUREMENT_COMPARISON_V1))),
        ]),
        6,
    );
    // One node at a time: no measurement shares the machine with other work of this Task.
    p.max_parallel = 1;
    p.slots
        .insert("implementer".into(), slot(author, "implement", &[], 1));
    p.slots.insert(
        "evaluator".into(),
        slot(evaluator, "evaluate", &["implementer"], 1),
    );
    p.nodes = vec![
        node(
            "baseline",
            TaskOperatorV1::Measure {
                measures: BTreeSet::from(["write".into()]),
            },
            inputs(&[("source", input("source"))]),
        ),
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
            "checks",
            TaskOperatorV1::Check {
                checks: BTreeSet::from(["sanity".into()]),
            },
            inputs(&[("source", output("seal", "snapshot"))]),
        ),
        when(
            node(
                "trial",
                TaskOperatorV1::Call {
                    pipeline: "builtin/experiment-trial".into(),
                    bindings: BTreeMap::from([("evaluator".into(), "evaluator".into())]),
                },
                inputs(&[
                    ("source", output("seal", "snapshot")),
                    ("requirements", input("requirements")),
                    ("checks", output("checks", "result")),
                    ("baseline", output("baseline", "write")),
                ]),
            ),
            "checks",
            ReceiptOutcomeV1::Passed,
        ),
        node(
            "accept",
            TaskOperatorV1::Accept {},
            inputs(&[
                ("source", output("seal", "snapshot")),
                ("checks", output("checks", "result")),
                ("evaluation", output("trial", "evaluation")),
            ]),
        ),
    ];
    p.outputs = inputs(&[
        ("snapshot", output("accept", "snapshot")),
        ("verification", output("accept", "result")),
        ("baseline", output("baseline", "write")),
        ("candidate", output("trial", "candidate")),
        ("comparison", output("trial", "comparison")),
    ]);
    cover(&mut p, "verified", "verification");
    p
}

pub(super) fn files() -> Result<BTreeMap<String, Vec<u8>>, String> {
    let literal = |value: &str| json!({"value": value, "provenance": "literal"});
    let policy: CodeTaskPolicy = serde_json::from_value(json!({
        "schema": "af.code-task-policy/1",
        "check_wall_ms": 60000,
        "require_container": false,
        "checks": {"sanity": {"name": "sanity", "required": true, "command": {
            "program": "python3",
            "args": [literal("-B"), literal("-c"), literal("assert int(open('size.txt').read()) >= 0")]
        }}},
        "measures": {"write": {
            "command": {"program": "python3", "args": [literal("-B"), literal("measure.py")]},
            "repetitions": 3,
            "warm": false,
            "wall_ms": 20000,
            "metrics": [{"key": "bytes_written", "unit": "bytes"}]
        }},
        "objectives": {"smaller": {
            "measure": "write",
            "metric": "bytes_written",
            "direction": "lower",
            "min_improvement_ratio": "0.1",
            "min_repetitions": 3
        }}
    }))
    .map_err(|e| e.to_string())?;
    policy.validate()?;
    let mut author = worker_definition(
        "experiment-implementer",
        "implement",
        ports(&[
            ("source", port(SOURCE_TREE_V1)),
            ("requirements", port("af/Requirements@1")),
        ]),
        ports(&[
            ("candidate", same(CANDIDATE_TREE_V1)),
            ("report", same("af/ImplementationReport@1")),
        ]),
        "af/ImplementationInput@1",
        "af/ImplementationReport@1",
        true,
    );
    // The implementer may build and time its candidate in its sandbox (ADR-0120); only the
    // kernel's measurements count.
    author.signature.effects.insert("execute-checks".into());
    let evaluator_inputs: Ports = ports(&[
        ("source", port(SOURCE_TREE_V1)),
        ("requirements", port("af/Requirements@1")),
        ("checks", same(TASK_CHECK_RECEIPT_V1)),
        ("comparison", same(MEASUREMENT_COMPARISON_V1)),
    ]);
    let mut evaluator = worker_definition(
        "experiment-evaluator",
        "evaluate",
        evaluator_inputs,
        ports(&[("result", same(TASK_EVALUATION_V1))]),
        "af/EvaluationInput@1",
        TASK_EVALUATION_V1,
        false,
    );
    evaluator.signature.outcome_port = Some("result".into());
    let pipelines = [experiment(&author, &evaluator), trial(&evaluator)];
    let mut files = BTreeMap::from([
        (".af/code-policy.toml".into(), toml_bytes(&policy)?),
        ("measure.py".into(), include_bytes!("measure.py").to_vec()),
        ("size.txt".into(), b"100\n".to_vec()),
    ]);
    let mut packages = BTreeMap::new();
    let mut contracts = CatalogContractFixtures {
        schema: "af.catalog-contract-fixtures/1".into(),
        pipelines: BTreeMap::new(),
        workers: BTreeMap::new(),
        kinds: BTreeMap::new(),
    };
    let report_schema = json!({"type":"object","additionalProperties":false,"required":["summary"],"properties":{"summary":{"type":"string","minLength":1,"maxLength":65536}}});
    let evaluation_schema = super::software::payload_schema(include_bytes!(
        "../../../../../../schemas/task-evaluation-v1.json"
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
    ] {
        package(
            &mut files,
            &mut packages,
            &worker.name,
            BTreeMap::from([
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
            ]),
        );
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
    let catalog = TaskCatalog {
        schema: "af.task-catalog/2".into(),
        provider_admission: None,
        code_policy: Some(".af/code-policy.toml".into()),
        document_policy: None,
        selection: BTreeMap::new(),
        no_match: review_config::task::selection::NoMatchPolicy::Refuse,
        planner: None,
        developers: None,
        review: None,
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
    let task = TaskFile {
        issue: None,
        inputs: None,
        schema: "af.task-file/1".into(),
        task_id: "experiment".into(),
        kind: "implement".into(),
        goal: GOAL.into(),
        requirements: Some(
            json!({"schema": "tutorial.experiment/1", "size": 80})
                .as_object()
                .expect("an object")
                .clone(),
        ),
        document_sources: None,
        optimization_history: None,
        pipeline: Some(PipelineChoiceV1 {
            name: "builtin/experiment".into(),
            fallback: PipelineFallbackV1::Refuse,
        }),
        strategy: "fast".into(),
        verification: Some(FileVerification::Evaluation),
        facts: BTreeMap::new(),
        limits: FileLimits {
            tokens: 0,
            max_attempts: 8,
            wall_ms: 600000,
            verification: VerificationReserveV1 {
                tokens: 0,
                attempts: 3,
                wall_ms: 180000,
            },
        },
    };
    files.insert("experiment.json".into(), json_bytes(&task)?);
    files.insert(".af/task-catalog.toml".into(), toml_bytes(&catalog)?);
    files.insert("catalog.toml".into(), toml_bytes(&shared)?);
    files.insert("contracts.json".into(), json_bytes(&contracts)?);
    files.insert("README.md".into(), include_bytes!("README.md").to_vec());
    Ok(files)
}
