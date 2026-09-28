use super::*;
use review_source_git::Entry;
use review_source_git::task::{capture_snapshot, source_tree};
use serde_json::json;

fn store() -> (tempfile::TempDir, Cas) {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    (directory, cas)
}

fn manifest(cas: &Cas, entries: &[(&str, EntryKind, &[u8])]) -> Manifest {
    Manifest::new(
        entries
            .iter()
            .map(|(path, kind, bytes)| Entry {
                path: (*path).into(),
                kind: *kind,
                content: cas.put(bytes).unwrap(),
                size: bytes.len() as u64,
            })
            .collect(),
    )
    .unwrap()
}

/// One captured Snapshot of `manifest` and the `af/SourceTree@1` port that names it.
fn snapshot(cas: &Cas, manifest: &Manifest) -> (String, ArtifactInputV1) {
    let origin = cas
        .put_json(
            &json!({"schema":"af.task-source-origin/1","repository_id":"fixture",
            "source_revision":"0".repeat(40),"content_digest":manifest.content_digest()}),
        )
        .unwrap();
    let id = capture_snapshot(cas, manifest, &origin, None).unwrap();
    let port = source_tree(cas, kernel(), &id, vec![origin]).unwrap();
    (id, port)
}

fn kernel() -> Producer {
    Producer::KernelOperation {
        run_id: "report-fixture".into(),
        node_id: None,
        operation_id: "fixture@1".into(),
    }
}

fn attempt(node: &str) -> Producer {
    Producer::Attempt {
        run_id: "report-fixture".into(),
        node_id: node.into(),
        attempt_id: "a".repeat(26),
    }
}

fn one(id: String, ty: &str, snapshot: Option<&str>) -> ArtifactInputV1 {
    ArtifactInputV1 {
        artifact_ids: vec![id],
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        snapshot_id: snapshot.map(str::to_owned),
    }
}

fn policy() -> ReportTaskPolicy {
    ReportTaskPolicy {
        schema: REPORT_TASK_POLICY_SCHEMA.into(),
        max_document_bytes: 65536,
        required_sections: BTreeSet::from(["Findings".into()]),
        require_citations: false,
        require_repository_citations: true,
        check_wall_ms: 5000,
        require_container: false,
    }
}

fn domain(cas: &Cas) -> ReportTaskDomain {
    let policy = policy();
    let policy_id = cas
        .put_json(&serde_json::to_value(&policy).unwrap())
        .unwrap();
    let signatures = report_signatures(&policy_id, &policy).unwrap();
    let mut graph: CompiledTask = serde_json::from_value(json!({
        "schema":"af.compiled-task/1","nodes":{},"order":[],"inputs":{},"outputs":{},
        "coverage":{},"calls":{},"slots":{},"max_parallel":1,"allowances":{}
    }))
    .unwrap();
    for (node, operator, signature) in [
        (
            "root.nodes.seal",
            TaskOperatorV1::ReportSeal {},
            "operator/report-seal",
        ),
        (
            "root.nodes.checks",
            TaskOperatorV1::ReportCheck {},
            "operator/report-check",
        ),
        (
            "root.nodes.verify",
            TaskOperatorV1::Verify {
                slot: "root.slots.verifier".into(),
            },
            "worker/fixture/verifier",
        ),
    ] {
        graph.nodes.insert(
            node.into(),
            review_graph::task::CompiledNode {
                operator: CompiledOperator::Primitive {
                    operator,
                    signature: signature.into(),
                },
                contract: signatures.get(signature).map_or_else(
                    || signatures["operator/report-check"].contract.clone(),
                    |s| s.contract.clone(),
                ),
                inputs: BTreeMap::new(),
                conditions: vec![],
            },
        );
    }
    ReportTaskDomain {
        policy_id,
        policy,
        graph,
    }
}

fn section(heading: &str, body: &str) -> DocumentSectionV1 {
    DocumentSectionV1 {
        heading: heading.into(),
        body: body.into(),
    }
}

fn cite(path: &str, line: Option<u64>) -> RepositoryCitationV1 {
    RepositoryCitationV1 {
        path: path.into(),
        line,
    }
}

fn draft(citations: &[RepositoryCitationV1]) -> DocumentDraftV2 {
    DocumentDraftV2 {
        schema: "af.document-draft/2".into(),
        title: "Where the time goes".into(),
        sections: vec![section("Findings", "Tests dominate: 12 of 14 minutes.")],
        citations: BTreeSet::from(["r1".into()]),
        repository_citations: citations.iter().cloned().collect(),
    }
}

fn sources() -> ReportSourcesV1 {
    ReportSourcesV1 {
        schema: "af.document-sources/1".into(),
        sources: BTreeMap::from([(
            "r1".into(),
            DocumentSourceV1 {
                title: "Task research-r1".into(),
                uri: "https://example.invalid/r1".into(),
                revision: "r1@1".into(),
                text: "{\"checks\":[]}".into(),
            },
        )]),
    }
}

#[test]
fn a_first_version_draft_renders_exactly_as_the_document_renderer_renders_it() {
    let draft = draft(&[]).as_first_version();
    let report = sources();
    let document = DocumentSourcesV1 {
        schema: report.schema.clone(),
        sources: report.sources.clone(),
    };
    assert_eq!(
        render_report(&ReportDraft::V1(draft.clone()), &report).unwrap(),
        super::super::document::render_document(&draft, &document).unwrap()
    );
    // A second-version draft without repository citations renders the same bytes too.
    assert_eq!(
        render_report(&ReportDraft::V2(self::draft(&[])), &report).unwrap(),
        super::super::document::render_document(&draft, &document).unwrap()
    );
}

#[test]
fn repository_citations_render_as_path_or_path_line_in_a_closed_code_span() {
    let rendered = render_report(
        &ReportDraft::V2(draft(&[
            cite("src/lib.rs", None),
            cite("src/lib.rs", Some(2)),
            cite("docs/a`b.md", Some(1)),
        ])),
        &sources(),
    )
    .unwrap();
    assert!(
        rendered.ends_with(
            "\n## Repository citations\n\n- ``docs/a`b.md:1``\n\n- `src/lib.rs`\n\n- `src/lib.rs:2`\n"
        ),
        "{rendered}"
    );
    assert_eq!(code_span("`tick"), "`` `tick ``");
    assert_eq!(code_span("a``b"), "```a``b```");
    // The empty set of sources renders no Sources section and still renders citations.
    let mut bare = draft(&[cite("README.md", None)]);
    bare.citations.clear();
    let rendered = render_report(&ReportDraft::V2(bare), &ReportSourcesV1::empty()).unwrap();
    assert!(!rendered.contains("## Sources"));
    assert!(rendered.contains("- `README.md`"));
}

#[test]
fn citations_resolve_only_to_text_lines_of_the_exact_manifest() {
    use ReportCitationFailureReasonV1::*;
    let (_directory, cas) = store();
    let mut late = vec![b'x'; 8192];
    late.push(0);
    let manifest = manifest(
        &cas,
        &[
            ("src/lib.rs", EntryKind::File, b"a\nb\nc\n"),
            ("src/unterminated.rs", EntryKind::File, b"a\nb"),
            ("bin/run", EntryKind::Executable, b"#!/bin/sh\n"),
            ("link", EntryKind::Symlink, b"src/lib.rs"),
            ("data.bin", EntryKind::File, b"head\0tail\n"),
            ("late.bin", EntryKind::File, &late),
            ("empty.txt", EntryKind::File, b""),
        ],
    );
    for (citation, expected) in [
        (cite("src/lib.rs", None), None),
        (cite("src/lib.rs", Some(1)), None),
        (cite("src/lib.rs", Some(3)), None),
        (cite("src/lib.rs", Some(4)), Some(LineOutOfRange)),
        (cite("src/unterminated.rs", Some(2)), None),
        (cite("src/unterminated.rs", Some(3)), Some(LineOutOfRange)),
        (cite("bin/run", Some(1)), None),
        (cite("empty.txt", None), None),
        (cite("empty.txt", Some(1)), Some(LineOutOfRange)),
        (cite("src", None), Some(Directory)),
        (cite("bin", Some(1)), Some(Directory)),
        (cite("missing.rs", None), Some(Absent)),
        (cite("SRC/lib.rs", None), Some(Absent)),
        (cite("./src/lib.rs", None), Some(Absent)),
        (cite("link", None), Some(Symlink)),
        (cite("data.bin", None), Some(Binary)),
        // Only the first 8 KiB decide: a NUL byte after them is text as far as a citation goes.
        (cite("late.bin", Some(1)), None),
    ] {
        assert_eq!(
            resolve_citation(&cas, &manifest, &citation).unwrap(),
            expected,
            "{}",
            citation.display()
        );
    }
}

#[test]
fn only_closed_author_and_verifier_contracts_are_admitted() {
    let mut author = PipelineContractV1 {
        inputs: BTreeMap::from([
            ("requirements".into(), port("af/Requirements@1", false)),
            ("source".into(), port(SOURCE_TREE_V1, false)),
        ]),
        outputs: BTreeMap::from([("draft".into(), same_source(DOCUMENT_DRAFT_V2, false))]),
    };
    assert!(author_contract(&author));
    author
        .outputs
        .insert("draft".into(), same_source(DOCUMENT_DRAFT_V1, false));
    assert!(author_contract(&author));
    author
        .inputs
        .insert("sources".into(), port(REPORT_SOURCES_V1, true));
    author
        .inputs
        .insert("comparison".into(), port(MEASUREMENT_COMPARISON_V1, true));
    author
        .inputs
        .insert("measurements".into(), many(MEASUREMENT_V1));
    assert!(author_contract(&author));
    // A comparison — or sources — the author cannot run without would make an unbound Task
    // uncompilable: every port beyond requirements and source is optional.
    let mut required = author.clone();
    required
        .inputs
        .insert("comparison".into(), port(MEASUREMENT_COMPARISON_V1, false));
    assert!(!author_contract(&required));
    let mut required_sources = author.clone();
    required_sources
        .inputs
        .insert("sources".into(), port(REPORT_SOURCES_V1, false));
    assert!(!author_contract(&required_sources));
    // A draft not bound to the source Snapshot, or any other output, is refused.
    let mut unbound = author.clone();
    unbound
        .outputs
        .insert("draft".into(), port(DOCUMENT_DRAFT_V2, false));
    assert!(!author_contract(&unbound));
    let mut candidate = author.clone();
    candidate
        .outputs
        .insert("candidate".into(), same_source("af/CandidateTree@1", false));
    assert!(!author_contract(&candidate));
    let mut undeclared = author.clone();
    undeclared
        .inputs
        .insert("history".into(), port("af/ReviewHistory@1", false));
    assert!(!author_contract(&undeclared));

    let mut verifier = PipelineContractV1 {
        inputs: BTreeMap::from([
            ("requirements".into(), port("af/Requirements@1", false)),
            ("source".into(), port(SOURCE_TREE_V1, false)),
            ("document".into(), same_source(DOCUMENT_V1, false)),
            ("checks".into(), same_source(REPORT_CHECK_RECEIPT_V1, false)),
            ("comparison".into(), port(MEASUREMENT_COMPARISON_V1, true)),
            ("measurements".into(), many(MEASUREMENT_V1)),
        ]),
        outputs: BTreeMap::from([("result".into(), same_source(REPORT_EVALUATION_V1, false))]),
    };
    assert!(verifier_contract(&verifier));
    verifier.inputs.remove("source");
    assert!(!verifier_contract(&verifier));
    verifier
        .inputs
        .insert("source".into(), port(SOURCE_TREE_V1, false));
    verifier
        .inputs
        .insert("checks".into(), port(REPORT_CHECK_RECEIPT_V1, false));
    assert!(
        !verifier_contract(&verifier),
        "checks not bound to the source"
    );
}

#[test]
fn a_verifier_bound_to_another_snapshot_is_refused_at_admission() {
    let (_directory, cas) = store();
    let domain = domain(&cas);
    let tree = manifest(&cas, &[("src/lib.rs", EntryKind::File, b"a\nb\n")]);
    let (cited, source) = snapshot(&cas, &tree);
    let other = manifest(&cas, &[("src/lib.rs", EntryKind::File, b"a\nb\nchanged\n")]);
    let (foreign, foreign_source) = snapshot(&cas, &other);
    assert_ne!(cited, foreign);
    let plan_id = cas.put_json(&json!({"plan":"fixture"})).unwrap();
    let invocation = |node: &str, inputs: Vec<(&str, ArtifactInputV1)>| TaskInvocationV1 {
        plan_id: plan_id.clone(),
        node: node.into(),
        inputs: inputs
            .into_iter()
            .map(|(port, value)| (port.to_owned(), value))
            .collect(),
    };
    let draft_id = cas
        .put_artifact(
            DOCUMENT_DRAFT_V2,
            attempt("root.nodes.author"),
            vec![],
            Some(cited.clone()),
            serde_json::to_value(draft(&[cite("src/lib.rs", Some(2))])).unwrap(),
        )
        .unwrap()
        .0;
    let sources_id = cas
        .put_artifact(
            REPORT_SOURCES_V1,
            kernel(),
            vec![],
            None,
            serde_json::to_value(sources()).unwrap(),
        )
        .unwrap()
        .0;
    let sources_port = one(sources_id.clone(), REPORT_SOURCES_V1, None);
    let seal = invocation(
        "root.nodes.seal",
        vec![
            ("draft", one(draft_id, DOCUMENT_DRAFT_V2, Some(&cited))),
            ("sources", sources_port.clone()),
            ("source", source.clone()),
        ],
    );
    let document = domain.sealed(&cas, &seal).unwrap();
    let document_id = cas
        .put_artifact(
            DOCUMENT_V1,
            kernel(),
            vec![],
            Some(cited.clone()),
            serde_json::to_value(&document).unwrap(),
        )
        .unwrap()
        .0;
    let document_port = one(document_id.clone(), DOCUMENT_V1, Some(&cited));
    let check = invocation(
        "root.nodes.checks",
        vec![
            ("document", document_port.clone()),
            ("sources", sources_port.clone()),
            ("source", source.clone()),
        ],
    );
    let receipt = domain.checks(&cas, &check).unwrap();
    assert_eq!(receipt.outcome, ReceiptOutcomeV1::Passed, "{receipt:?}");
    assert_eq!(receipt.source_snapshot_id, cited);
    let (_, recorded, _) = domain.source(&cas, &check).unwrap();
    assert_eq!(receipt.manifest_id, recorded);
    let receipt_id = cas
        .put_artifact(
            REPORT_CHECK_RECEIPT_V1,
            attempt("root.nodes.checks"),
            vec![document_id, sources_id, receipt.manifest_id.clone()],
            Some(cited.clone()),
            serde_json::to_value(&receipt).unwrap(),
        )
        .unwrap()
        .0;
    let requirements = cas
        .put_artifact(
            "af/Requirements@1",
            kernel(),
            vec![],
            None,
            json!({"text":"Report where the time goes."}),
        )
        .unwrap()
        .0;
    let verify = |source: ArtifactInputV1, snapshot: &str| {
        invocation(
            "root.nodes.verify",
            vec![
                (
                    "requirements",
                    one(requirements.clone(), "af/Requirements@1", None),
                ),
                ("document", document_port.clone()),
                (
                    "checks",
                    one(receipt_id.clone(), REPORT_CHECK_RECEIPT_V1, Some(snapshot)),
                ),
                ("sources", sources_port.clone()),
                ("source", source),
            ],
        )
    };
    assert_eq!(
        domain
            .admit_verifier(&cas, &verify(source, &cited))
            .unwrap(),
        receipt
    );
    // The same receipt, handed to a verifier that reads another Snapshot, is refused whether
    // the invocation spells the receipt's Snapshot honestly or claims its own.
    for claimed in [cited.as_str(), foreign.as_str()] {
        let refused = domain
            .admit_verifier(&cas, &verify(foreign_source.clone(), claimed))
            .unwrap_err();
        assert!(refused.contains("Snapshot"), "{refused}");
    }
}

#[test]
fn a_failed_citation_fails_the_check_receipt_and_names_its_reason() {
    let (_directory, cas) = store();
    let domain = domain(&cas);
    let tree = manifest(&cas, &[("src/lib.rs", EntryKind::File, b"a\nb\n")]);
    let (cited, source) = snapshot(&cas, &tree);
    let draft_id = cas
        .put_artifact(
            DOCUMENT_DRAFT_V2,
            attempt("root.nodes.author"),
            vec![],
            Some(cited.clone()),
            serde_json::to_value(draft(&[
                cite("src/lib.rs", Some(3)),
                cite("src/main.rs", None),
            ]))
            .unwrap(),
        )
        .unwrap()
        .0;
    let sources_id = cas
        .put_artifact(
            REPORT_SOURCES_V1,
            kernel(),
            vec![],
            None,
            serde_json::to_value(sources()).unwrap(),
        )
        .unwrap()
        .0;
    let invocation = |node: &str, inputs: Vec<(&str, ArtifactInputV1)>| TaskInvocationV1 {
        plan_id: format!("sha256:{}", "2".repeat(64)),
        node: node.into(),
        inputs: inputs
            .into_iter()
            .map(|(port, value)| (port.to_owned(), value))
            .collect(),
    };
    let sources_port = one(sources_id, REPORT_SOURCES_V1, None);
    let document = domain
        .sealed(
            &cas,
            &invocation(
                "root.nodes.seal",
                vec![
                    ("draft", one(draft_id, DOCUMENT_DRAFT_V2, Some(&cited))),
                    ("sources", sources_port.clone()),
                    ("source", source.clone()),
                ],
            ),
        )
        .unwrap();
    let document_id = cas
        .put_artifact(
            DOCUMENT_V1,
            kernel(),
            vec![],
            Some(cited.clone()),
            serde_json::to_value(&document).unwrap(),
        )
        .unwrap()
        .0;
    let receipt = domain
        .checks(
            &cas,
            &invocation(
                "root.nodes.checks",
                vec![
                    ("document", one(document_id, DOCUMENT_V1, Some(&cited))),
                    ("sources", sources_port),
                    ("source", source),
                ],
            ),
        )
        .unwrap();
    assert_eq!(receipt.outcome, ReceiptOutcomeV1::Failed);
    assert_eq!(
        receipt.checks["repository_citations"],
        ReceiptOutcomeV1::Failed
    );
    assert_eq!(
        receipt.checks["required_sections"],
        ReceiptOutcomeV1::Passed
    );
    assert_eq!(
        receipt
            .citation_failures
            .iter()
            .map(|f| (f.citation.display(), f.reason))
            .collect::<Vec<_>>(),
        [
            (
                "src/lib.rs:3".to_string(),
                ReportCitationFailureReasonV1::LineOutOfRange
            ),
            (
                "src/main.rs".to_string(),
                ReportCitationFailureReasonV1::Absent
            ),
        ]
    );
    receipt.validate().unwrap();
}

#[test]
fn the_seal_binds_exactly_one_draft() {
    let (_directory, cas) = store();
    let tree = manifest(&cas, &[("a", EntryKind::File, b"a\n")]);
    let (cited, source) = snapshot(&cas, &tree);
    let id = format!("sha256:{}", "3".repeat(64));
    let both = TaskInvocationV1 {
        plan_id: id.clone(),
        node: "root.nodes.seal".into(),
        inputs: BTreeMap::from([
            (
                "draft".into(),
                one(id.clone(), DOCUMENT_DRAFT_V2, Some(&cited)),
            ),
            (
                "draft_v1".into(),
                one(id.clone(), DOCUMENT_DRAFT_V1, Some(&cited)),
            ),
            ("source".into(), source),
        ]),
    };
    assert!(
        draft_input(&both, &cited)
            .unwrap_err()
            .contains("exactly one draft")
    );
    let mut none = both.clone();
    none.inputs.remove("draft");
    none.inputs.remove("draft_v1");
    assert!(draft_input(&none, &cited).is_err());
    let mut first = both;
    first.inputs.remove("draft");
    assert_eq!(draft_input(&first, &cited).unwrap(), id);
}

#[test]
fn the_report_policy_and_its_schema_refuse_alike() {
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let schema: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("schemas/report-task-policy-v1.json")).unwrap(),
    )
    .unwrap();
    let schema = jsonschema::options().build(&schema).unwrap();
    let accepted = serde_json::to_value(policy()).unwrap();
    policy().validate().unwrap();
    assert!(schema.is_valid(&accepted), "{accepted}");
    let rust = |value: &serde_json::Value| {
        serde_json::from_value::<ReportTaskPolicy>(value.clone())
            .is_ok_and(|policy| policy.validate().is_ok())
    };
    for (pointer, value) in [
        ("/schema", json!("af.document-task-policy/1")),
        ("/check_wall_ms", json!(0)),
        ("/check_wall_ms", json!(60001)),
        ("/max_document_bytes", json!(1048577)),
        ("/required_sections", json!([])),
        ("/require_repository_citations", json!("yes")),
    ] {
        let mut refused = accepted.clone();
        *refused.pointer_mut(pointer).unwrap() = value;
        assert!(!schema.is_valid(&refused), "{pointer}");
        assert!(!rust(&refused), "{pointer}");
    }
    let mut missing = accepted.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("require_repository_citations");
    assert!(!schema.is_valid(&missing) && !rust(&missing));
    let mut extra = accepted;
    extra["allowed_effects"] = json!(["write-source"]);
    assert!(!schema.is_valid(&extra) && !rust(&extra));
}

#[test]
fn a_bound_comparison_and_measurements_reach_a_report_worker_and_nothing_when_absent() {
    let (_directory, cas) = store();
    let tree = manifest(&cas, &[("README.md", EntryKind::File, b"a\n")]);
    let (measured, source) = snapshot(&cas, &tree);
    let contract = review_runner::task::WorkerContract::capture(
        &cas,
        json!({"type": "object"}),
        BTreeMap::from([("draft".into(), json!({"type": "object"}))]),
    )
    .unwrap();
    let put = |ty: &str, subject: Option<&str>, payload: serde_json::Value| {
        cas.put_artifact(ty, kernel(), vec![], subject.map(str::to_owned), payload)
            .unwrap()
            .0
    };
    let requirements = put("af/Requirements@1", None, json!({"text": "Report."}));
    let sources = put(
        REPORT_SOURCES_V1,
        None,
        serde_json::to_value(ReportSourcesV1::empty()).unwrap(),
    );
    let comparison = put(
        MEASUREMENT_COMPARISON_V1,
        Some(&measured),
        json!({"fixture": "comparison"}),
    );
    let first = put(MEASUREMENT_V1, Some(&measured), json!({"fixture": 1}));
    let second = put(MEASUREMENT_V1, Some(&measured), json!({"fixture": 2}));
    let mut inputs = BTreeMap::from([
        (
            "requirements".to_string(),
            one(requirements, "af/Requirements@1", None),
        ),
        ("source".to_string(), source),
        ("sources".to_string(), one(sources, REPORT_SOURCES_V1, None)),
    ]);
    let entries = |inputs: &BTreeMap<String, ArtifactInputV1>| -> Vec<(String, String)> {
        let invocation = TaskInvocationV1 {
            plan_id: format!("sha256:{}", "4".repeat(64)),
            node: "root.nodes.author".into(),
            inputs: inputs.clone(),
        };
        let id = contract
            .prepare(&cas, &invocation, &[], "Write the report.")
            .unwrap();
        let context = cas.get_artifact(&id).unwrap().payload;
        context["manifest"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["required_by"] == "declared input port")
            .map(|entry| {
                (
                    entry["name"].as_str().unwrap().to_owned(),
                    entry["artifact_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let absent = entries(&inputs);
    assert_eq!(
        absent
            .iter()
            .map(|(port, _)| port.as_str())
            .collect::<Vec<_>>(),
        ["requirements", "source", "sources"]
    );
    inputs.insert(
        "comparison".into(),
        one(
            comparison.clone(),
            MEASUREMENT_COMPARISON_V1,
            Some(&measured),
        ),
    );
    inputs.insert(
        "measurements".into(),
        ArtifactInputV1 {
            artifact_ids: vec![first.clone(), second.clone()],
            artifact_type: MEASUREMENT_V1.into(),
            cardinality: PortCardinality::Many,
            snapshot_id: Some(measured.clone()),
        },
    );
    let bound = entries(&inputs);
    for expected in [
        ("comparison".to_string(), comparison),
        ("measurements".to_string(), first),
        ("measurements".to_string(), second),
    ] {
        assert!(bound.contains(&expected), "{expected:?} in {bound:?}");
    }
    assert_eq!(bound.len(), absent.len() + 3);
}

#[test]
fn a_pipeline_that_binds_no_sources_seals_and_checks_against_the_empty_set() {
    let (_directory, cas) = store();
    let domain = domain(&cas);
    let tree = manifest(&cas, &[("src/lib.rs", EntryKind::File, b"a\nb\n")]);
    let (cited, source) = snapshot(&cas, &tree);
    // A draft that cites the repository only: with no sources there is nothing else to cite.
    let mut uncited = draft(&[cite("src/lib.rs", Some(1))]);
    uncited.citations.clear();
    let draft_id = cas
        .put_artifact(
            DOCUMENT_DRAFT_V2,
            attempt("root.nodes.author"),
            vec![],
            Some(cited.clone()),
            serde_json::to_value(uncited).unwrap(),
        )
        .unwrap()
        .0;
    let invocation = |node: &str, inputs: Vec<(&str, ArtifactInputV1)>| TaskInvocationV1 {
        plan_id: format!("sha256:{}", "2".repeat(64)),
        node: node.into(),
        inputs: inputs
            .into_iter()
            .map(|(port, value)| (port.to_owned(), value))
            .collect(),
    };
    let document = domain
        .sealed(
            &cas,
            &invocation(
                "root.nodes.seal",
                vec![
                    (
                        "draft",
                        one(draft_id.clone(), DOCUMENT_DRAFT_V2, Some(&cited)),
                    ),
                    ("source", source.clone()),
                ],
            ),
        )
        .unwrap();
    // The seal recorded the empty set it rendered with, and the document names it.
    let recorded: ReportSourcesV1 =
        serde_json::from_value(cas.get_json(&document.sources_id).unwrap()["payload"].clone())
            .unwrap();
    assert_eq!(recorded, ReportSourcesV1::empty());
    let document_id = cas
        .put_artifact(
            DOCUMENT_V1,
            kernel(),
            vec![],
            Some(cited.clone()),
            serde_json::to_value(&document).unwrap(),
        )
        .unwrap()
        .0;
    let receipt = domain
        .checks(
            &cas,
            &invocation(
                "root.nodes.checks",
                vec![
                    (
                        "document",
                        one(document_id.clone(), DOCUMENT_V1, Some(&cited)),
                    ),
                    ("source", source.clone()),
                ],
            ),
        )
        .unwrap();
    assert_eq!(receipt.sources_id, document.sources_id);
    assert_eq!(receipt.checks["source_locations"], ReceiptOutcomeV1::Passed);
    receipt.validate().unwrap();
    // A report sealed with sources cannot be checked as if it had none.
    let with_sources = cas
        .put_artifact(
            REPORT_SOURCES_V1,
            kernel(),
            vec![],
            None,
            serde_json::to_value(sources()).unwrap(),
        )
        .unwrap()
        .0;
    let sealed_with = domain
        .sealed(
            &cas,
            &invocation(
                "root.nodes.seal",
                vec![
                    ("draft", one(draft_id, DOCUMENT_DRAFT_V2, Some(&cited))),
                    ("sources", one(with_sources, REPORT_SOURCES_V1, None)),
                    ("source", source.clone()),
                ],
            ),
        )
        .unwrap();
    let sealed_with_id = cas
        .put_artifact(
            DOCUMENT_V1,
            kernel(),
            vec![],
            Some(cited.clone()),
            serde_json::to_value(&sealed_with).unwrap(),
        )
        .unwrap()
        .0;
    let refused = domain
        .checks(
            &cas,
            &invocation(
                "root.nodes.checks",
                vec![
                    ("document", one(sealed_with_id, DOCUMENT_V1, Some(&cited))),
                    ("source", source),
                ],
            ),
        )
        .unwrap_err();
    assert!(refused.contains("sealed with some"), "{refused}");
}
