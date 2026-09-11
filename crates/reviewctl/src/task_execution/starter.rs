//! Token-free starters are emitted from the supported typed definitions and actual byte pins.
//! Command substitutes implement a deliberately bounded tutorial, with independent acceptance.
use super::*;
use review_config::task::catalog::export::CatalogContractFixtures;
use review_config::task::kind::TaskKindManifest;
use review_config::task::shared::{CatalogPathBase, SharedTaskCatalog, package_directory};
use review_core::task::document::*;
use review_core::task::pipeline::*;
use review_graph::task::{OperatorAttemptCost, OperatorSignature};

const GOAL: &str = "Publish release notes from the captured changes.";
const AUTHOR: &str = r#"import json, sys
r=json.load(sys.stdin)
assert set(r['inputs']) == {'requirements','sources'}
s=r['inputs']['sources'][0]['payload']['sources']
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'draft':[{
 'schema':'af.document-draft/1','title':'Release notes',
 'sections':[{'heading':'Summary','body':'\n\n'.join(s[k]['text'] for k in sorted(s))}],
 'citations':sorted(s)}]}}))
"#;
const VERIFIER: &str = r#"import json, sys, string
r=json.load(sys.stdin)
assert set(r['inputs']) == {'requirements','sources','document','checks'}
i={k:v[0] for k,v in r['inputs'].items()}
assert i['checks']['payload']['outcome'] == 'passed'
plain=i['document']['payload']['text']
for c in string.punctuation: plain=plain.replace('\\'+c,c)
sources=i['sources']['payload']['sources']
accepted=(i['requirements']['payload']['text']=='Publish release notes from the captured changes.'
 and '# Release notes' in plain and '## Summary' in plain
 and all(s['text'] in plain and s['title'] in plain and s['revision'] in plain for s in sources.values()))
print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{
 'document_id':i['document']['artifact_id'],'sources_id':i['sources']['artifact_id'],
 'requirements_id':i['requirements']['artifact_id'],'check_receipt_id':i['checks']['artifact_id'],
 'outcome':'passed' if accepted else 'failed',
 'summary':'Captured changes, source references and requested release-note format checked.'}]}}))
"#;

fn port(ty: &str) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        optional: false,
        affinity: PortAffinityV1::Unbound {},
        root_default: None,
        covers: BTreeSet::new(),
    }
}
fn input(name: &str) -> ValueRefV1 {
    ValueRefV1::Input { port: name.into() }
}
fn output(node: &str, port: &str) -> ValueRefV1 {
    ValueRefV1::Node {
        node: node.into(),
        port: port.into(),
    }
}
fn inputs_schema(ports: &BTreeMap<String, PipelinePortV1>) -> serde_json::Value {
    let properties: BTreeMap<_,_> = ports.iter().map(|(name,port)| (name.clone(),json!({
        "type":"array","minItems":1,"maxItems":1,"items":{
            "type":"object","additionalProperties":false,"required":["artifact_id","artifact_type","payload"],
            "properties":{"artifact_id":{"type":"string"},"artifact_type":{"const":port.artifact_type},"payload":{"type":"object"}}
        }}))).collect();
    json!({"type":"object","additionalProperties":false,"required":ports.keys().collect::<Vec<_>>(),"properties":properties})
}
fn worker(
    name: &str,
    role: &str,
    inputs: BTreeMap<String, PipelinePortV1>,
    output_name: &str,
    output_type: &str,
    policy_id: &str,
) -> TaskWorkerManifest {
    let mut inputs = inputs;
    let mut produced = port(output_type);
    if role == "verify" {
        inputs
            .get_mut("checks")
            .expect("verifier check input")
            .affinity = PortAffinityV1::SameAs {
            input: "document".into(),
        };
        produced.affinity = PortAffinityV1::SameAs {
            input: "document".into(),
        };
    }
    TaskWorkerManifest {
        schema: "af.worker/1".into(),
        name: name.into(),
        version: "1.0.0".into(),
        signature: OperatorSignature {
            retains: BTreeMap::from([(output_name.into(), inputs.keys().cloned().collect())]),
            contract: PipelineContractV1 {
                inputs,
                outputs: BTreeMap::from([(output_name.into(), produced)]),
            },
            effects: BTreeSet::new(),
            evidence: if role == "verify" {
                BTreeMap::from([(output_name.into(), BTreeSet::from([policy_id.into()]))])
            } else {
                BTreeMap::new()
            },
            roles: BTreeSet::from([role.into()]),
            worker_input_type: Some(
                if role == "verify" {
                    "af/DocumentVerificationInput@1"
                } else {
                    "af/DocumentInput@1"
                }
                .into(),
            ),
            worker_output_type: Some(output_type.into()),
            outcome_port: if role == "verify" {
                Some(output_name.into())
            } else {
                None
            },
            attempt: Some(OperatorAttemptCost {
                tokens: 0,
                wall_ms: 5000,
            }),
        },
        runner: TaskWorkerRunner::Command {
            command: review_config::CommandSpec {
                program: "python3".into(),
                args: vec![
                    review_config::ArgSpec {
                        value: "-B".into(),
                        provenance: review_config::ProvenanceSpec::Literal,
                    },
                    review_config::ArgSpec {
                        value: "@package/worker.py".into(),
                        provenance: review_config::ProvenanceSpec::Literal,
                    },
                ],
            },
        },
    }
}
fn package(
    files: &mut BTreeMap<String, Vec<u8>>,
    packages: &mut BTreeMap<String, TaskPackagePin>,
    name: &str,
    content: BTreeMap<String, Vec<u8>>,
) {
    let path = format!(".af/{}", package_directory(name));
    packages.insert(
        name.into(),
        TaskPackagePin {
            version: "1.0.0".into(),
            digest: review_config::lock::package_digest_from_files(&content),
            path: path.clone(),
        },
    );
    files.extend(
        content
            .into_iter()
            .map(|(file, bytes)| (format!("{path}/{file}"), bytes)),
    );
}
fn toml_bytes(value: &impl Serialize) -> Result<Vec<u8>, String> {
    toml::to_string(value)
        .map(String::into_bytes)
        .map_err(|e| e.to_string())
}
fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(value).map_err(|e| e.to_string())
}

pub(super) fn document_files() -> Result<BTreeMap<String, Vec<u8>>, String> {
    let policy = DocumentTaskPolicy {
        schema: "af.document-task-policy/1".into(),
        max_document_bytes: 65536,
        required_sections: BTreeSet::from(["Summary".into()]),
        require_citations: true,
        check_wall_ms: 5000,
        require_container: false,
    };
    policy.validate()?;
    let policy_id = review_store::canonical::content_id(
        &serde_json::to_value(&policy).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let author = worker(
        "builtin/document-author",
        "author",
        BTreeMap::from([
            ("requirements".into(), port("af/Requirements@1")),
            ("sources".into(), port(DOCUMENT_SOURCES_V1)),
        ]),
        "draft",
        DOCUMENT_DRAFT_V1,
        &policy_id,
    );
    let verifier = worker(
        "builtin/document-verifier",
        "verify",
        BTreeMap::from([
            ("requirements".into(), port("af/Requirements@1")),
            ("sources".into(), port(DOCUMENT_SOURCES_V1)),
            ("document".into(), port(DOCUMENT_V1)),
            ("checks".into(), port(DOCUMENT_CHECK_RECEIPT_V1)),
        ]),
        "result",
        DOCUMENT_EVALUATION_V1,
        &policy_id,
    );
    let slot = |worker: &TaskWorkerManifest, role: &str| WorkerSlotV1 {
        worker: worker.name.clone(),
        role: role.into(),
        input_type: worker.signature.worker_input_type.clone().unwrap(),
        output_type: worker.signature.worker_output_type.clone().unwrap(),
        min_attempts: u32::from(role == "verify"),
        max_attempts: 1,
        allow_local_replacement: true,
        independent_from: if role == "verify" {
            BTreeSet::from(["author".into()])
        } else {
            BTreeSet::new()
        },
    };
    let node = |id: &str, operator, inputs, when| TaskNodeV1 {
        id: id.into(),
        operator,
        inputs,
        when,
    };
    let mut verification = port(DOCUMENT_VERIFICATION_V1);
    verification.covers.insert("verified".into());
    let pipeline = PipelineDefinitionV1 {
        schema: PipelineSchemaV1::V1,
        name: "builtin/release-notes".into(),
        version: "1.0.0".into(),
        contract: PipelineContractV1 {
            inputs: author.signature.contract.inputs.clone(),
            outputs: BTreeMap::from([
                ("document".into(), port(DOCUMENT_V1)),
                ("verification".into(), verification),
            ]),
        },
        accepts: PipelineApplicabilityV1 {
            kinds: BTreeSet::from(["release-note".into()]),
            required_facts: BTreeMap::new(),
        },
        slots: BTreeMap::from([
            ("author".into(), slot(&author, "author")),
            ("verifier".into(), slot(&verifier, "verify")),
        ]),
        nodes: vec![
            node(
                "author",
                TaskOperatorV1::Worker {
                    slot: "author".into(),
                },
                BTreeMap::from([
                    ("requirements".into(), input("requirements")),
                    ("sources".into(), input("sources")),
                ]),
                None,
            ),
            node(
                "seal",
                TaskOperatorV1::DocumentSeal {},
                BTreeMap::from([
                    ("draft".into(), output("author", "draft")),
                    ("sources".into(), input("sources")),
                ]),
                None,
            ),
            node(
                "checks",
                TaskOperatorV1::DocumentCheck {},
                BTreeMap::from([
                    ("document".into(), output("seal", "document")),
                    ("sources".into(), input("sources")),
                ]),
                None,
            ),
            node(
                "verify",
                TaskOperatorV1::Verify {
                    slot: "verifier".into(),
                },
                BTreeMap::from([
                    ("document".into(), output("seal", "document")),
                    ("checks".into(), output("checks", "result")),
                    ("requirements".into(), input("requirements")),
                    ("sources".into(), input("sources")),
                ]),
                Some(NodeConditionV1 {
                    node: "checks".into(),
                    outcome: ReceiptOutcomeV1::Passed,
                }),
            ),
            node(
                "accept",
                TaskOperatorV1::DocumentAccept {},
                BTreeMap::from([
                    ("document".into(), output("seal", "document")),
                    ("checks".into(), output("checks", "result")),
                    ("evaluation".into(), output("verify", "result")),
                ]),
                None,
            ),
        ],
        outputs: BTreeMap::from([
            ("document".into(), output("accept", "document")),
            ("verification".into(), output("accept", "result")),
        ]),
        coverage: BTreeMap::from([("verified".into(), output("accept", "result"))]),
        max_attempts: 3,
        max_parallel: 1,
    };
    pipeline.validate()?;
    let kind = TaskKindManifest {
        schema: "af.task-kind/1".into(),
        name: "builtin/release-note-kind".into(),
        version: "1.0.0".into(),
        kind: "release-note".into(),
        profile: TaskKindProfile::Document,
    };
    let mut files = BTreeMap::from([(".af/document-policy.toml".into(), toml_bytes(&policy)?)]);
    let mut packages = BTreeMap::new();
    for (worker, script, output_name, schema) in [
        (
            &author,
            AUTHOR,
            "draft",
            include_bytes!("../../../../schemas/document-draft-v1.json").as_slice(),
        ),
        (
            &verifier,
            VERIFIER,
            "result",
            include_bytes!("../../../../schemas/document-evaluation-v1.json").as_slice(),
        ),
    ] {
        let mut payload_schema: serde_json::Value =
            serde_json::from_slice(schema).map_err(|e| e.to_string())?;
        // The Worker validator fixes Draft 2020-12 and forbids external schema identities.
        // Preserve the payload constraints while localizing the public schema resource.
        let schema_object = payload_schema
            .as_object_mut()
            .ok_or("Payload schema is not an object")?;
        schema_object.remove("$schema");
        schema_object.remove("$id");
        package(
            &mut files,
            &mut packages,
            &worker.name,
            BTreeMap::from([
                ("worker.toml".into(), toml_bytes(worker)?),
                ("worker.py".into(), script.as_bytes().to_vec()),
                (
                    "input.schema.json".into(),
                    json_bytes(&inputs_schema(&worker.signature.contract.inputs))?,
                ),
                (
                    format!("outputs/{output_name}.schema.json"),
                    json_bytes(&payload_schema)?,
                ),
            ]),
        );
    }
    package(
        &mut files,
        &mut packages,
        &pipeline.name,
        BTreeMap::from([("pipeline.toml".into(), toml_bytes(&pipeline)?)]),
    );
    package(
        &mut files,
        &mut packages,
        &kind.name,
        BTreeMap::from([("kind.toml".into(), toml_bytes(&kind)?)]),
    );
    let catalog = TaskCatalog {
        schema: "af.task-catalog/1".into(),
        code_policy: None,
        document_policy: Some(".af/document-policy.toml".into()),
        selection: BTreeMap::new(),
        no_match: review_config::task::selection::NoMatchPolicy::Refuse,
        developers: None,
        planner: None,
        review: None,
        packages: packages.clone(),
        kinds: BTreeMap::from([(kind.kind.clone(), kind.name.clone())]),
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
    let contracts = CatalogContractFixtures {
        schema: "af.catalog-contract-fixtures/1".into(),
        pipelines: BTreeMap::from([(pipeline.name.clone(), (&pipeline).into())]),
        workers: BTreeMap::from([
            (author.name, author.signature),
            (verifier.name, verifier.signature),
        ]),
        kinds: BTreeMap::from([(kind.name.clone(), kind)]),
    };
    let task = TaskFile {
        schema: "af.task-file/1".into(),
        task_id: "release-notes".into(),
        kind: "release-note".into(),
        goal: GOAL.into(),
        document_sources: Some("sources.json".into()),
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
            wall_ms: 90000,
            verification: VerificationReserveV1 {
                tokens: 0,
                attempts: 2,
                wall_ms: 10000,
            },
        },
    };
    let sources = DocumentSourcesV1 {
        schema: "af.document-sources/1".into(),
        sources: BTreeMap::from([(
            "pagination".into(),
            DocumentSourceV1 {
                title: "Pagination ticket".into(),
                uri: "https://example.invalid/AF-42".into(),
                revision: "AF-42@1".into(),
                text: "Pagination now accepts an offset and a limit.".into(),
            },
        )]),
    };
    files.extend(BTreeMap::from([(".af/task-catalog.toml".into(),toml_bytes(&catalog)?),("catalog.toml".into(),toml_bytes(&shared)?),
        ("contracts.json".into(),json_bytes(&contracts)?),("document.json".into(),json_bytes(&task)?),("sources.json".into(),json_bytes(&sources)?),
        ("README.md".into(),b"# Document Task tutorial\n\nReview these files, initialize this directory as a Git repository, and commit them.\nRun `af catalog test --source . --json`, then `af task start --file document.json --json`.\nThe Python command substitutes support exactly the supplied release-note goal, preserve captured changes,\nand independently verify their inclusion and source revisions. Other goals fail acceptance.\nNo network link check or model inference runs. Replace the captured Workers for broader authoring.\n".to_vec()),
    ]));
    Ok(files)
}

pub(crate) fn init(repo: &Path, destination: &str, json: bool) -> Result<(), String> {
    let target = catalog::absent_destination(repo, destination)?;
    let files = document_files()?;
    catalog::publish_absent(&target, &files)?;
    if json {
        println!(
            "{}",
            json!({"schema":"af.catalog-init/1","profile":"document","destination":target,"task":"document.json","attempts":0,"prerequisites":["git","python3"],"authority":"review_and_commit_required"})
        );
    } else {
        println!(
            "Created document starter in {}. Review and commit its definitions before running document.json.",
            target.display()
        );
    }
    Ok(())
}
