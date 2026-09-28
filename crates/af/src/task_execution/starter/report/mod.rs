//! The report starter (ADR-0126), assembled from the same typed contracts as admission: a
//! report policy, a command author that reads the committed tree in a clone that seals nothing
//! back, and an independent command verifier that reads the same Snapshot. The kernel renders
//! the draft, resolves its repository citations against the exact Manifest and accepts the
//! report only on the verifier's positive verdict.
use super::software::{Ports, cover, definition, inputs, node, ports, same, slot, when};
use super::*;
use review_core::task::measurement::{MEASUREMENT_COMPARISON_V1, MEASUREMENT_V1};
use review_core::task::report_task::*;
use review_pipeline::task::report_task::{REPORT_TASK_POLICY_SCHEMA, ReportTaskPolicy};
use review_source_git::task::SOURCE_TREE_V1;
use serde_json::Value;

const GOAL: &str = "Report the starter layout from its committed files.";

fn optional(mut port: PipelinePortV1) -> PipelinePortV1 {
    port.optional = true;
    port
}

fn many(ty: &str) -> PipelinePortV1 {
    let mut port = optional(port(ty));
    port.cardinality = PortCardinality::Many;
    port
}

/// The root inputs every report Worker may read: the kernel's measurements are optional and
/// reach a Worker only when a Task binds them.
fn root_inputs() -> Ports {
    ports(&[
        ("requirements", port("af/Requirements@1")),
        ("source", port(SOURCE_TREE_V1)),
        ("sources", optional(port(REPORT_SOURCES_V1))),
        ("comparison", optional(port(MEASUREMENT_COMPARISON_V1))),
        ("measurements", many(MEASUREMENT_V1)),
    ])
}

/// A command Worker's input schema: one item per `one` port, up to the reply bound per `many`
/// port, a Snapshot ID on every Snapshot-bound value, and only the required ports required. A
/// bound Measurement or Comparison names the Snapshot it measured even on an unbound port, and
/// a port bound from several Measurements shows each one's own (ADR-0127), so those values may
/// carry the member; it is never required of them.
fn input_schema(ports: &Ports) -> Value {
    let properties: BTreeMap<_, _> = ports
        .iter()
        .map(|(name, port)| {
            let mut item = json!({
                "type": "object", "additionalProperties": false,
                "required": ["artifact_id", "artifact_type", "payload"],
                "properties": {
                    "artifact_id": {"type": "string"},
                    "artifact_type": {"const": port.artifact_type},
                    "payload": {"type": "object"}
                }
            });
            if port.artifact_type == SOURCE_TREE_V1
                || matches!(port.affinity, PortAffinityV1::SameAs { .. })
            {
                item["properties"]["snapshot_id"] =
                    json!({"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"});
                item["required"]
                    .as_array_mut()
                    .expect("required list")
                    .push(json!("snapshot_id"));
            } else if [MEASUREMENT_V1, MEASUREMENT_COMPARISON_V1]
                .contains(&port.artifact_type.as_str())
            {
                item["properties"]["snapshot_id"] =
                    json!({"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"});
            }
            let most = if port.cardinality == PortCardinality::Many {
                1024
            } else {
                1
            };
            (
                name.clone(),
                json!({"type": "array", "minItems": 1, "maxItems": most, "items": item}),
            )
        })
        .collect();
    let required: Vec<_> = ports
        .iter()
        .filter(|(_, port)| !port.optional)
        .map(|(name, _)| name)
        .collect();
    json!({"type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

fn worker(
    name: &str,
    role: &str,
    incoming: Ports,
    outgoing: Ports,
    input_type: &str,
    effects: &[&str],
) -> TaskWorkerManifest {
    let output_type = outgoing
        .values()
        .next()
        .expect("one Worker output")
        .artifact_type
        .clone();
    TaskWorkerManifest {
        schema: "af.worker/1".into(),
        name: format!("builtin/{name}"),
        version: "1.0.0".into(),
        signature: OperatorSignature {
            retains: outgoing
                .keys()
                .map(|port| (port.clone(), incoming.keys().cloned().collect()))
                .collect(),
            contract: PipelineContractV1 {
                inputs: incoming,
                outputs: outgoing,
            },
            effects: effects.iter().map(|effect| (*effect).into()).collect(),
            evidence: BTreeMap::new(),
            roles: BTreeSet::from([role.into()]),
            worker_input_type: Some(input_type.into()),
            worker_output_type: Some(output_type),
            outcome_port: None,
            attempt: Some(OperatorAttemptCost {
                tokens: 0,
                wall_ms: 20000,
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

fn pipeline(author: &TaskWorkerManifest, verifier: &TaskWorkerManifest) -> PipelineDefinitionV1 {
    let mut p = definition(
        "report",
        "report",
        root_inputs(),
        ports(&[
            ("report", same(DOCUMENT_V1)),
            ("verification", same(REPORT_VERIFICATION_V1)),
        ]),
        3,
    );
    p.max_parallel = 1;
    p.slots
        .insert("author".into(), slot(author, "author", &[], 1));
    p.slots
        .insert("verifier".into(), slot(verifier, "verify", &["author"], 1));
    fn root(names: &[&'static str]) -> Vec<(&'static str, ValueRefV1)> {
        names.iter().map(|name| (*name, input(name))).collect()
    }
    let mut verify = root(&[
        "requirements",
        "source",
        "sources",
        "comparison",
        "measurements",
    ]);
    verify.extend([
        ("document", output("seal", "document")),
        ("checks", output("checks", "result")),
    ]);
    p.nodes = vec![
        node(
            "author",
            TaskOperatorV1::Worker {
                slot: "author".into(),
            },
            inputs(&root(&[
                "requirements",
                "source",
                "sources",
                "comparison",
                "measurements",
            ])),
        ),
        node(
            "seal",
            TaskOperatorV1::ReportSeal {},
            inputs(&[
                ("draft", output("author", "draft")),
                ("sources", input("sources")),
                ("source", input("source")),
            ]),
        ),
        node(
            "checks",
            TaskOperatorV1::ReportCheck {},
            inputs(&[
                ("document", output("seal", "document")),
                ("sources", input("sources")),
                ("source", input("source")),
            ]),
        ),
        when(
            node(
                "verify",
                TaskOperatorV1::Verify {
                    slot: "verifier".into(),
                },
                inputs(&verify),
            ),
            "checks",
            ReceiptOutcomeV1::Passed,
        ),
        node(
            "accept",
            TaskOperatorV1::ReportAccept {},
            inputs(&[
                ("document", output("seal", "document")),
                ("checks", output("checks", "result")),
                ("evaluation", output("verify", "result")),
                ("source", input("source")),
            ]),
        ),
    ];
    p.outputs = inputs(&[
        ("report", output("accept", "document")),
        ("verification", output("accept", "result")),
    ]);
    cover(&mut p, "verified", "verification");
    p
}

pub(super) fn files() -> Result<BTreeMap<String, Vec<u8>>, String> {
    let policy = ReportTaskPolicy {
        schema: REPORT_TASK_POLICY_SCHEMA.into(),
        max_document_bytes: 65536,
        required_sections: BTreeSet::from(["Findings".into()]),
        require_citations: false,
        require_repository_citations: true,
        check_wall_ms: 10000,
        require_container: false,
    };
    policy.validate()?;
    let policy_id = review_store::canonical::content_id(
        &serde_json::to_value(&policy).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // The author reads source and runs what it needs in a clone that seals nothing back.
    let author = worker(
        "report-author",
        "author",
        root_inputs(),
        ports(&[("draft", same(DOCUMENT_DRAFT_V2))]),
        "af/ReportInput@1",
        &["read-source", "execute-checks"],
    );
    let mut verifier_inputs = root_inputs();
    verifier_inputs.insert("document".into(), same(DOCUMENT_V1));
    verifier_inputs.insert("checks".into(), same(REPORT_CHECK_RECEIPT_V1));
    let mut verifier = worker(
        "report-verifier",
        "verify",
        verifier_inputs,
        ports(&[("result", same(REPORT_EVALUATION_V1))]),
        "af/ReportVerificationInput@1",
        &["read-source"],
    );
    verifier.signature.outcome_port = Some("result".into());
    verifier
        .signature
        .evidence
        .insert("result".into(), BTreeSet::from([policy_id]));
    let pipeline = pipeline(&author, &verifier);
    pipeline.validate()?;
    let mut files = BTreeMap::from([(".af/report-policy.toml".into(), toml_bytes(&policy)?)]);
    let mut packages = BTreeMap::new();
    let mut contracts = CatalogContractFixtures {
        schema: "af.catalog-contract-fixtures/1".into(),
        pipelines: BTreeMap::from([(pipeline.name.clone(), (&pipeline).into())]),
        workers: BTreeMap::new(),
        kinds: BTreeMap::new(),
    };
    for (worker, script, output_name, schema) in [
        (
            &author,
            include_str!("author.py"),
            "draft",
            include_bytes!("../../../../../../schemas/document-draft-v2.json").as_slice(),
        ),
        (
            &verifier,
            include_str!("verifier.py"),
            "result",
            include_bytes!("../../../../../../schemas/report-evaluation-v1.json").as_slice(),
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
                    json_bytes(&input_schema(&worker.signature.contract.inputs))?,
                ),
                (
                    format!("outputs/{output_name}.schema.json"),
                    json_bytes(&super::software::payload_schema(schema)?)?,
                ),
            ]),
        );
        contracts
            .workers
            .insert(worker.name.clone(), worker.signature.clone());
    }
    package(
        &mut files,
        &mut packages,
        &pipeline.name,
        BTreeMap::from([("pipeline.toml".into(), toml_bytes(&pipeline)?)]),
    );
    let catalog = TaskCatalog {
        schema: "af.task-catalog/2".into(),
        provider_admission: None,
        code_policy: None,
        document_policy: None,
        report_policy: Some(".af/report-policy.toml".into()),
        selection: BTreeMap::new(),
        no_match: review_config::task::selection::NoMatchPolicy::Refuse,
        developers: None,
        planner: None,
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
        requirements: None,
        inputs: None,
        schema: "af.task-file/1".into(),
        task_id: "starter-report".into(),
        kind: "report".into(),
        goal: GOAL.into(),
        document_sources: None,
        report_sources: Some("sources.json".into()),
        optimization_history: None,
        pipeline: Some(PipelineChoiceV1 {
            name: pipeline.name,
            fallback: PipelineFallbackV1::Refuse,
        }),
        strategy: "fast".into(),
        verification: None,
        facts: BTreeMap::new(),
        limits: FileLimits {
            tokens: 0,
            max_attempts: 3,
            wall_ms: 180000,
            verification: VerificationReserveV1 {
                tokens: 0,
                attempts: 2,
                wall_ms: 30000,
            },
        },
    };
    let sources = ReportSourcesV1 {
        schema: "af.document-sources/1".into(),
        sources: BTreeMap::from([(
            "note".into(),
            review_core::task::document::DocumentSourceV1 {
                title: "Starter note".into(),
                uri: "repo:README.md".into(),
                revision: "starter@1".into(),
                text: "A report cites the committed files it read.".into(),
            },
        )]),
    };
    sources.validate()?;
    files.extend(BTreeMap::from([
        (".af/task-catalog.toml".into(), toml_bytes(&catalog)?),
        ("catalog.toml".into(), toml_bytes(&shared)?),
        ("contracts.json".into(), json_bytes(&contracts)?),
        ("report.json".into(), json_bytes(&task)?),
        ("sources.json".into(), json_bytes(&sources)?),
        ("README.md".into(), include_bytes!("README.md").to_vec()),
    ]));
    Ok(files)
}
