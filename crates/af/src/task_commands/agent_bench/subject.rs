use super::*;
use af_contracts::task::execution::{TaskAttemptResultV1, TaskInvocationV1, TaskOutputV1};
use af_graph::task::{CompiledNode, CompiledOperator};
use af_graph::{NodeOutcome, RunReport};
use af_runtime::task::source::{invocation_producer, source_snapshot};
use af_runtime::task::{TaskOperatorHost, TaskWorkOutput};
use af_store::store::task::execution::{PreparedTaskAttempt, ReservedTaskAttempt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_ENTRY: u64 = 8 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Trace {
    tool: String,
    request: String,
    response: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    elapsed_ns: u64,
    source_digest: String,
    tools: Vec<String>,
    trace: Vec<Trace>,
    answer: Option<semantic::Reply>,
    failure: Option<String>,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    native_calls: Option<Vec<native_fixture::Send>>,
}

const COMMON_TOOLS: [&str; 3] = ["af_list_paths", "af_read_range", "af_search"];
const SEMANTIC_TOOLS: [&str; 3] = ["af_definition", "af_references", "af_implementation"];

fn tools(arm: Arm) -> Vec<String> {
    let semantic = if arm == Arm::Treatment {
        &SEMANTIC_TOOLS[..]
    } else {
        &[]
    };
    COMMON_TOOLS
        .iter()
        .chain(semantic)
        .map(|name| (*name).to_owned())
        .collect()
}

fn semantic_tool(operation: semantic::Operation) -> &'static str {
    match operation {
        semantic::Operation::Definition => SEMANTIC_TOOLS[0],
        semantic::Operation::References => SEMANTIC_TOOLS[1],
        semantic::Operation::Implementation => SEMANTIC_TOOLS[2],
    }
}

pub(super) struct Analyzer<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) digest: &'a str,
    pub(super) project: &'a [semantic::Crate],
    pub(super) libraries: Option<[&'a [u8]; 2]>,
}

pub(super) struct Tools<'a> {
    pub(super) arm: Arm,
    pub(super) broker: navigation::Broker,
    pub(super) deadline: Instant,
    pub(super) cancellation: Option<&'a AtomicBool>,
    pub(super) trace: &'a mut Vec<Trace>,
}

impl Tools<'_> {
    fn current(&self) -> Result<(), String> {
        if self.trace.len() >= 64 {
            return Err("Subject navigation observation limit reached".into());
        }
        if self
            .cancellation
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err("Subject cancelled".into());
        }
        if Instant::now() >= self.deadline {
            return Err("Subject deadline elapsed".into());
        }
        Ok(())
    }

    pub(super) fn text(
        &mut self,
        name: &str,
        request: &navigation::Request,
    ) -> Result<navigation::Response, String> {
        self.current()?;
        let response = self.broker.execute_until(request, self.deadline);
        self.trace.push(Trace {
            tool: name.into(),
            request: navigation::Broker::request_identity(request),
            response: response
                .as_ref()
                .ok()
                .map(navigation::Broker::response_identity),
        });
        response.map_err(|e| e.to_string())
    }

    pub(super) fn semantic(
        &mut self,
        operation: semantic::Operation,
        position: &semantic::Position,
        analyzer: &Analyzer<'_>,
    ) -> Result<semantic::Reply, String> {
        if self.arm != Arm::Treatment {
            return Err("Semantic tool is unavailable to the baseline".into());
        }
        self.current()?;
        let request =
            af_store::content_id(&json!([operation, position])).map_err(|e| e.to_string())?;
        let response = if let Some(libraries) = analyzer.libraries {
            self.broker.semantic_linux(
                &semantic::LinuxRuntime {
                    analyzer: analyzer.bytes,
                    libraries,
                },
                analyzer.project,
                operation,
                position,
                self.deadline,
            )
        } else {
            self.broker.semantic(
                analyzer.bytes,
                analyzer.digest,
                analyzer.project,
                operation,
                position,
                self.deadline,
            )
        };
        self.trace.push(Trace {
            tool: semantic_tool(operation).into(),
            request,
            response: response
                .as_ref()
                .ok()
                .and_then(|reply| af_store::content_id(&serde_json::to_value(reply).ok()?).ok()),
        });
        response.map_err(|e| e.to_string())
    }
}

// Fixed, synthetic inference. It receives only the case, registered tools and (for treatment)
// the analyzer's captured bytes and project configuration. No Oracle or Task Store handle.
fn answer(
    slot: &Slot,
    tools: &mut Tools<'_>,
    analyzer: Option<Analyzer<'_>>,
) -> Result<semantic::Reply, String> {
    use navigation::{Request, Response};
    tools.text(COMMON_TOOLS[0], &Request::ListPaths { offset: 0 })?;
    tools.text(
        COMMON_TOOLS[1],
        &Request::ReadRange {
            path: slot.case.position.path.clone(),
            offset: 0,
            length: 65_536,
        },
    )?;
    let Response::Search {
        hits, next_offset, ..
    } = tools.text(
        COMMON_TOOLS[2],
        &Request::Search {
            query: slot.case.literal.clone(),
            offset: 0,
        },
    )?
    else {
        return Err("Search returned a different tool reply".into());
    };
    if let Some(analyzer) = analyzer {
        return tools.semantic(slot.case.operation, &slot.case.position, &analyzer);
    }
    let mut anchors: Vec<_> = hits
        .into_iter()
        .map(|hit| semantic::Anchor {
            path: hit.path,
            line: hit.line,
        })
        .collect();
    let truncated = anchors.len() > semantic::MAX_ANCHORS || next_offset.is_some();
    anchors.truncate(semantic::MAX_ANCHORS);
    Ok(semantic::Reply { anchors, truncated })
}

fn source_text(cas: &Cas, manifest: &Manifest, path: &str) -> Result<String, String> {
    let entry = manifest
        .get(path)
        .ok_or("Anchor path is absent from the Manifest")?;
    if entry.kind == EntryKind::Symlink {
        return Err("An anchor may not refer to a symlink".into());
    }
    let bytes = cas
        .get_bounded(&entry.content, MAX_ENTRY)
        .map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|_| "An anchor must refer to UTF-8 source".into())
}

fn anchors_valid(
    cas: &Cas,
    manifest: &Manifest,
    anchors: &[semantic::Anchor],
) -> Result<(), String> {
    if anchors.len() > semantic::MAX_ANCHORS || !anchors.windows(2).all(|w| w[0] < w[1]) {
        return Err("Anchors must be ordered, unique and bounded".into());
    }
    let mut last_path = "";
    let mut lines = 0;
    for anchor in anchors {
        if !af_contracts::navigation::valid_path(&anchor.path) || anchor.line == 0 {
            return Err("Invalid source anchor".into());
        }
        if anchor.path != last_path {
            last_path = &anchor.path;
            lines = source_text(cas, manifest, last_path)?.split('\n').count();
        }
        if anchor.line as usize > lines {
            return Err("Anchor line is outside captured source".into());
        }
    }
    Ok(())
}

pub(super) fn validate_packages(
    cas: &Cas,
    manifest: &Manifest,
    cases: &Cases,
    oracle: &Oracle,
) -> Result<(), String> {
    if cases.schema != "af.agent-bench-native-cases/1"
        || oracle.schema != "af.agent-bench-oracle/1"
        || cases.source_digest != manifest.content_digest()
        || oracle.source_digest != cases.source_digest
        || !(1..=MAX_CASES).contains(&cases.cases.len())
        || !(1..=3).contains(&cases.repetitions)
    {
        return Err("Cases and Oracle must name the exact source and a bounded schedule".into());
    }
    semantic::validate_project(&cases.project, |path| {
        manifest.entries.iter().any(|entry| entry.path == path)
    })
    .map_err(|_| "Invalid explicit Rust crate graph")?;
    let mut ids = BTreeSet::new();
    for case in &cases.cases {
        if !is_name(&case.id)
            || !ids.insert(&case.id)
            || case.question.trim().is_empty()
            || case.question.len() > 4096
            || !af_contracts::navigation::valid_path(&case.position.path)
            || !case.position.path.ends_with(".rs")
        {
            return Err("Invalid or duplicate benchmark case".into());
        }
        navigation::Request::Search {
            query: case.literal.clone(),
            offset: 0,
        }
        .validate()
        .map_err(|e| e.to_string())?;
        let text = source_text(cas, manifest, &case.position.path)?;
        let line = text
            .split('\n')
            .nth(case.position.line as usize)
            .ok_or("Case line is outside source")?;
        let character = case.position.character as usize;
        if !line
            .chars()
            .scan(0, |offset, ch| {
                let start = *offset;
                *offset += ch.len_utf16();
                Some(start)
            })
            .take_while(|offset| *offset <= character)
            .any(|offset| offset == character)
        {
            return Err("Case position does not name a character in the frozen source".into());
        }
        let expected = oracle
            .expected
            .get(&case.id)
            .ok_or("Oracle omitted a case")?;
        if expected.is_empty() {
            return Err("Oracle must name at least one expected anchor".into());
        }
        anchors_valid(cas, manifest, expected)?;
    }
    if oracle.expected.len() != ids.len() {
        return Err("Oracle contains an unknown case".into());
    }
    Ok(())
}

pub(super) fn smoke(cas: &Cas) -> Result<(Manifest, Cases, Oracle), String> {
    let files: [(&str, &str); 2] = [
        (
            "library/lib.rs",
            "pub trait Shape { fn size(&self) -> u32; }\npub struct Rectangle;\nimpl Shape for Rectangle { fn size(&self) -> u32 { 4 } }\npub fn calculate() -> u32 { 4 }\n",
        ),
        (
            "app/lib.rs",
            "use library::{Shape, Rectangle};\npub fn answer() -> u32 { library::calculate() }\npub fn size() -> u32 { Rectangle.size() }\n",
        ),
    ];
    let entries = files
        .into_iter()
        .map(|(path, content)| {
            Ok(af_source_git::Entry {
                path: path.into(),
                kind: EntryKind::File,
                content: cas.put(content.as_bytes()).map_err(|e| e.to_string())?,
                size: content.len() as u64,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let manifest = Manifest::new(entries).map_err(|e| e.to_string())?;
    let source_digest = manifest.content_digest();
    let case =
        |id: &str, question: &str, literal: &str, path: &str, line, character, operation| Case {
            id: id.into(),
            question: question.into(),
            literal: literal.into(),
            position: semantic::Position {
                path: path.into(),
                line,
                character,
            },
            operation,
        };
    let find = |path: &str, line| semantic::Anchor {
        path: path.into(),
        line,
    };
    let cases = Cases {
        schema: "af.agent-bench-native-cases/1".into(),
        source_digest: source_digest.clone(),
        repetitions: 1,
        project: vec![
            semantic::Crate {
                root: "library/lib.rs".into(),
                edition: semantic::Edition::Rust2021,
                dependencies: vec![],
            },
            semantic::Crate {
                root: "app/lib.rs".into(),
                edition: semantic::Edition::Rust2021,
                dependencies: vec![semantic::Dependency {
                    index: 0,
                    name: "library".into(),
                }],
            },
        ],
        cases: vec![
            case(
                "definition",
                "Where is calculate defined?",
                "calculate",
                "app/lib.rs",
                1,
                36,
                semantic::Operation::Definition,
            ),
            case(
                "references",
                "Where is calculate declared and referenced?",
                "calculate",
                "library/lib.rs",
                3,
                8,
                semantic::Operation::References,
            ),
            case(
                "implementation",
                "Where is Shape implemented?",
                "Shape",
                "library/lib.rs",
                0,
                11,
                semantic::Operation::Implementation,
            ),
        ],
    };
    let oracle = Oracle {
        schema: "af.agent-bench-oracle/1".into(),
        source_digest,
        expected: BTreeMap::from([
            ("definition".into(), vec![find("library/lib.rs", 4)]),
            (
                "references".into(),
                vec![find("app/lib.rs", 2), find("library/lib.rs", 4)],
            ),
            ("implementation".into(), vec![find("library/lib.rs", 3)]),
        ]),
    };
    Ok((manifest, cases, oracle))
}

pub(super) struct Domain {
    pub(super) policy: Policy,
    pub(super) graph: CompiledTask,
}

impl Domain {
    fn slot(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<Slot, String> {
        let key = input
            .inputs
            .get("slot")
            .and_then(|p| p.artifact_ids.first())
            .ok_or("Missing benchmark slot")?;
        let slot: Slot = artifact(cas, key, SLOT)?;
        let expected = self
            .policy
            .slots
            .get(slot.index)
            .ok_or("Slot outside frozen schedule")?;
        if &slot != expected || !input.node.ends_with(&format!(".{}", node_name(slot.index))) {
            return Err("Invocation changed its frozen benchmark slot".into());
        }
        Ok(slot)
    }

    fn trial(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        cancellation: Option<&AtomicBool>,
        raw_artifact_ids: &mut Vec<String>,
    ) -> Result<ArtifactInputV1, String> {
        let slot = self.slot(cas, input)?;
        let start = Instant::now();
        let deadline = start
            + Duration::from_millis(
                attempt
                    .reservation()
                    .deadline_unix_ms
                    .saturating_sub(clock()?),
            );
        let source = input
            .inputs
            .get("source")
            .ok_or("Missing benchmark source")?;
        let (snapshot, _, manifest) = source_snapshot(cas, source)?;
        if manifest.content_digest() != self.policy.source_digest {
            return Err("Subject source differs from frozen schedule".into());
        }
        let mut trace = Vec::new();
        let mut native_calls = self.policy.native_fixture.as_ref().map(|_| Vec::new());
        let answer = (|| {
            let broker =
                navigation::Broker::from_manifest(&manifest, cas).map_err(|e| e.to_string())?;
            let mut tools = Tools {
                arm: slot.arm,
                broker,
                deadline,
                cancellation,
                trace: &mut trace,
            };
            let analyzer = (slot.arm == Arm::Treatment)
                .then(|| cas.get_bounded(&self.policy.analyzer, semantic::MAX_BINARY))
                .transpose()
                .map_err(|e| e.to_string())?;
            let libraries = self
                .policy
                .analyzer_libraries
                .as_ref()
                .filter(|_| slot.arm == Arm::Treatment)
                .map(|[driver, llvm]| -> Result<_, af_store::CasError> {
                    let [(_, _, driver_size), (_, _, llvm_size)] = semantic::LINUX_LIBRARIES;
                    Ok([
                        cas.get_bounded(driver, driver_size)?,
                        cas.get_bounded(llvm, llvm_size)?,
                    ])
                })
                .transpose()
                .map_err(|e| e.to_string())?;
            let semantic = analyzer.as_deref().map(|bytes| Analyzer {
                bytes,
                digest: &self.policy.analyzer,
                project: &self.policy.project,
                libraries: libraries
                    .as_ref()
                    .map(|[driver, llvm]| ([driver.as_slice(), llvm.as_slice()])),
            });
            let answer = if let Some(fixture) = &self.policy.native_fixture {
                let (sends, result) = fixture.collect(cas, &slot, &mut tools, attempt, semantic);
                raw_artifact_ids.extend(native_fixture::evidence(&sends).cloned());
                native_calls = Some(sends);
                result?
            } else {
                answer(&slot, &mut tools, semantic)?
            };
            anchors_valid(cas, &manifest, &answer.anchors)?;
            Ok(answer)
        })();
        let elapsed_ns = u64::try_from(start.elapsed().as_nanos()).map_err(|e| e.to_string())?;
        let (answer, failure) = match answer {
            Ok(answer) => (Some(answer), None),
            Err(error) => (None, Some(error)),
        };
        let receipt = Receipt {
            elapsed_ns,
            source_digest: manifest.content_digest(),
            tools: tools(slot.arm),
            trace,
            answer,
            failure,
            native_calls,
        };
        let refs = receipt_refs(input, &receipt);
        let id = cas
            .put_artifact(
                RECEIPT,
                invocation_producer(cas, input, Some(attempt))?,
                refs,
                Some(snapshot.clone()),
                serde_json::to_value(receipt).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        Ok(ArtifactInputV1 {
            artifact_type: RECEIPT.into(),
            cardinality: PortCardinality::One,
            artifact_ids: vec![id],
            snapshot_id: Some(snapshot),
        })
    }

    fn assess(&self, cas: &Cas, result: &mut TaskResultV1) -> Result<(), String> {
        let complete = result.execution == TaskExecutionV1::Completed
            && self
                .policy
                .slots
                .iter()
                .all(|slot| result.outputs.contains_key(&node_name(slot.index)));
        let mut failed = false;
        for port in result.outputs.values() {
            let id = port
                .artifact_ids
                .first()
                .ok_or("Benchmark result lost its receipt")?;
            let receipt: Receipt = artifact(cas, id, RECEIPT)?;
            failed |= receipt.failure.is_some();
        }
        let (acceptance, conclusion) = match (complete, failed) {
            (false, _) => (
                TaskAcceptanceV1::Inconclusive,
                "benchmark_collection_incomplete",
            ),
            (true, true) => (TaskAcceptanceV1::Unsatisfied, "benchmark_subject_failed"),
            (true, false) => (TaskAcceptanceV1::Satisfied, "benchmark_subjects_collected"),
        };
        result.acceptance = acceptance;
        result.domain_conclusion = conclusion.into();
        result.missing_obligations.clear();
        if acceptance != TaskAcceptanceV1::Satisfied {
            result.missing_obligations.insert("collected".into());
        }
        Ok(())
    }
}

impl TaskOperatorHost for Domain {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _: &CompiledNode,
        _: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.slot(cas, input)?;
        cas.put_json(&serde_json::to_value(input).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }

    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        definition: &CompiledNode,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&AtomicBool>,
    ) -> TaskWorkOutput {
        let mut raw_artifact_ids = vec![];
        let outputs = match (attempt, &definition.operator) {
            (
                Some(attempt),
                CompiledOperator::Primitive {
                    operator: TaskOperatorV1::AgentBenchSubject {},
                    ..
                },
            ) => self
                .trial(cas, input, attempt, cancellation, &mut raw_artifact_ids)
                .map(|port| BTreeMap::from([("receipt".into(), port)])),
            _ => Err("Benchmark subject requires an exact native reservation".into()),
        };
        TaskWorkOutput {
            outputs,
            notes: Default::default(),
            usage_observation: None,
            usage: None,
            charged_tokens: Some(0),
            raw_artifact_ids,
            usage_id: None,
            feedback_id: None,
            unknown_usage_cause: None,
            refused_input: false,
        }
    }
}

impl TaskDomain for Domain {
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _: &ReservedTaskAttempt,
        id: &str,
    ) -> Result<(), String> {
        self.slot(cas, input)?;
        if cas.get_json(id).map_err(|e| e.to_string())?
            != serde_json::to_value(input).map_err(|e| e.to_string())?
        {
            return Err("Benchmark context changed the exact invocation".into());
        }
        Ok(())
    }

    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        _: &ExecutionPlan,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        _: &CompiledNode,
    ) -> Result<(), String> {
        let slot = self.slot(cas, input)?;
        let receipt_id = output
            .outputs
            .get("receipt")
            .and_then(|port| port.artifact_ids.first())
            .ok_or("No subject receipt")?;
        let envelope = cas.get_artifact(receipt_id).map_err(|e| e.to_string())?;
        let (_, _, manifest) = source_snapshot(cas, &input.inputs["source"])?;
        let receipt = read_receipt(
            cas,
            receipt_id,
            &manifest,
            &slot,
            self.policy.native_fixture.is_some(),
        )?;
        if envelope.input_artifacts != receipt_refs(input, &receipt) {
            return Err("Subject receipt lost exact input provenance".into());
        }
        if let (Some(fixture), Some(sends)) = (&self.policy.native_fixture, &receipt.native_calls) {
            let Producer::Attempt { attempt_id, .. } = envelope.producer else {
                return Err("Native fixture requires an Attempt producer".into());
            };
            let context =
                af_store::content_id(&serde_json::to_value(input).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let tokens = fixture.inclusive(
                cas,
                &self.policy.source_digest,
                &slot,
                sends,
                native_fixture::attempt_binding(&task.task_id, &attempt_id, &context, &slot),
            )?;
            if receipt.answer.is_some() && tokens.is_none_or(|tokens| tokens > FIXTURE_TOKENS) {
                return Err("Native fixture answer lacks bounded usage".into());
            }
        }
        Ok(())
    }

    fn assemble_result(
        &self,
        cas: &Cas,
        task: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let mut result = collection_result(&self.graph, task, report)?;
        self.assess(cas, &mut result)?;
        Ok(result)
    }

    fn validate_result(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let mut expected = result.clone();
        self.assess(cas, &mut expected)?;
        if expected != *result {
            return Err("Benchmark conclusion differs from recorded collection".into());
        }
        Ok(())
    }
}

pub(super) fn collection_result(
    graph: &CompiledTask,
    task: &TaskProjection,
    report: &RunReport,
) -> Result<TaskResultV1, String> {
    let execution = task
        .execution
        .as_ref()
        .ok_or("Benchmark Task has no execution")?;
    let get = |a: &af_graph::task::Address| {
        execution
            .outputs
            .get(&a.node)
            .and_then(|(_, output)| output.outputs.get(&a.port))
    };
    Ok(TaskResultV1 {
        task_revision_id: task.revision_id.clone(),
        execution: if report
            .outcomes
            .iter()
            .any(|(_, o)| matches!(o, NodeOutcome::Failed { .. }))
        {
            TaskExecutionV1::Exhausted
        } else {
            TaskExecutionV1::Completed
        },
        acceptance: TaskAcceptanceV1::Inconclusive,
        domain_conclusion: String::new(),
        outputs: graph
            .outputs
            .iter()
            .filter_map(|(name, a)| get(a).map(|p| (name.clone(), p.clone())))
            .collect(),
        evidence: graph
            .coverage
            .values()
            .filter_map(get)
            .flat_map(|p| p.artifact_ids.clone())
            .collect(),
        missing_obligations: BTreeSet::new(),
    })
}

pub(super) fn input_refs(input: &TaskInvocationV1) -> Vec<String> {
    input
        .inputs
        .values()
        .flat_map(|port| port.artifact_ids.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn receipt_refs(input: &TaskInvocationV1, receipt: &Receipt) -> Vec<String> {
    let mut refs = input_refs(input);
    if let Some(calls) = &receipt.native_calls {
        refs.extend(native_fixture::evidence(calls).cloned());
        refs.sort();
        refs.dedup();
    }
    refs
}

fn read_receipt(
    cas: &Cas,
    id: &str,
    manifest: &Manifest,
    slot: &Slot,
    native: bool,
) -> Result<Receipt, String> {
    let receipt: Receipt = artifact(cas, id, RECEIPT)?;
    let synthetic_names = COMMON_TOOLS
        .iter()
        .copied()
        .chain((slot.arm == Arm::Treatment).then(|| semantic_tool(slot.case.operation)))
        .collect::<Vec<_>>();
    if receipt.source_digest != manifest.content_digest()
        || receipt.tools != tools(slot.arm)
        || receipt.answer.is_some() == receipt.failure.is_some()
        || receipt.native_calls.is_some() != native
        || receipt.trace.len() > if native { 64 } else { synthetic_names.len() }
        || receipt.trace.iter().enumerate().any(|(index, trace)| {
            (if native {
                !receipt.tools.contains(&trace.tool)
            } else {
                trace.tool != synthetic_names[index]
            }) || !af_contracts::is_digest(&trace.request)
                || trace
                    .response
                    .as_deref()
                    .is_some_and(|id| !af_contracts::is_digest(id))
        })
    {
        return Err(
            "Benchmark receipt changed its source, tool registration or observations".into(),
        );
    }
    if let Some(answer) = &receipt.answer {
        if (!native && receipt.trace.len() != synthetic_names.len())
            || receipt.trace.iter().any(|call| call.response.is_none())
        {
            return Err("Subject answer lost navigation observations".into());
        }
        anchors_valid(cas, manifest, &answer.anchors)?;
    }
    Ok(receipt)
}

fn grade(answer: &semantic::Reply, expected: &[semantic::Anchor]) -> &'static str {
    if answer.truncated {
        "ambiguous"
    } else if answer.anchors == expected {
        "correct"
    } else {
        "incorrect"
    }
}

pub(super) fn report(
    cas: &Cas,
    policy: &Policy,
    task: &TaskProjection,
) -> Result<serde_json::Value, String> {
    let source = task
        .revision
        .inputs
        .get("source")
        .ok_or("Missing benchmark source")?;
    let (snapshot, _, manifest) = source_snapshot(cas, source)?;
    let cases: Cases = artifact(cas, &policy.cases, CASES)?;
    let oracle: Oracle = artifact(cas, &policy.oracle, ORACLE)?;
    validate_packages(cas, &manifest, &cases, &oracle)?;
    if policy.source_digest != manifest.content_digest()
        || cases.project != policy.project
        || schedule(&cases) != policy.slots
    {
        return Err("Recorded schedule differs from the frozen case package".into());
    }
    let execution = task.execution.as_ref();
    let attempts = execution
        .map(|run| run.attempt_accounting())
        .unwrap_or_default();
    let mut slots = Vec::new();
    for slot in &policy.slots {
        let name = node_name(slot.index);
        let address = execution.and_then(|run| run.graph.outputs.get(&name));
        let output = execution
            .zip(address)
            .and_then(|(run, address)| run.outputs.get(&address.node));
        let receipt_id = output
            .and_then(|(_, out)| out.outputs.get("receipt"))
            .and_then(|p| p.artifact_ids.first());
        let receipt = receipt_id
            .map(|id| read_receipt(cas, id, &manifest, slot, policy.native_fixture.is_some()))
            .transpose()?;
        let mut rows = Vec::new();
        let mut charged = 0_u128;
        let mut started = 0;
        let mut failed = false;
        let mut fixture_tokens = None;
        let producer = receipt_id
            .map(|id| cas.get_artifact(id).map(|envelope| envelope.producer))
            .transpose()
            .map_err(|e| e.to_string())?;
        for attempt in attempts
            .iter()
            .filter(|attempt| address.is_some_and(|a| attempt.reservation.node == a.node))
        {
            started += usize::from(attempt.started);
            failed |= matches!(attempt.result, Some(TaskAttemptResultV1::Failed { .. }));
            charged += attempt.charged_tokens;
            if let (Some(fixture), Some(calls), Some(Producer::Attempt { attempt_id, .. })) = (
                &policy.native_fixture,
                receipt.as_ref().and_then(|row| row.native_calls.as_deref()),
                &producer,
            ) && attempt_id == &attempt.attempt_id
            {
                let context = attempt
                    .context_id
                    .as_deref()
                    .ok_or("Fixture lost its native context")?;
                if !native_fixture::evidence(calls).all(|id| attempt.raw_artifact_ids.contains(id))
                {
                    return Err("Fixture settlement lost exact call evidence".into());
                }
                fixture_tokens = fixture.inclusive(
                    cas,
                    &policy.source_digest,
                    slot,
                    calls,
                    native_fixture::attempt_binding(&task.task_id, attempt_id, context, slot),
                )?;
            }
            rows.push(json!({
                "attempt_id": attempt.attempt_id, "started": attempt.started,
                "context_id": attempt.context_id, "reserved_tokens": attempt.reservation.tokens,
                "af_charged_tokens": attempt.charged_tokens.to_string(), "settlement": attempt.result,
                "raw_artifact_ids": attempt.raw_artifact_ids,
                "usage_id": attempt.usage_id, "abandoned": attempt.abandoned.map(|cause| cause.as_str()),
            }));
        }
        let answer = receipt.as_ref().and_then(|row| row.answer.as_ref());
        let status = if answer.is_some() {
            "ok"
        } else if receipt.is_some() || failed {
            "failed"
        } else if started > 0 {
            "missing"
        } else {
            "unstarted"
        };
        let elapsed_ns = receipt
            .as_ref()
            .filter(|_| started == 1)
            .map(|row| row.elapsed_ns);
        // The grader gets only anchors and the corresponding answer key. Status, arm, tools,
        // ordering, timing and accounting are joined after grading.
        let verdict = answer.map(|answer| grade(answer, &oracle.expected[&slot.case.id]));
        slots.push(json!({
            "index":slot.index, "case":slot.case.id, "repetition":slot.repetition, "arm":slot.arm,
            "status":status, "receipt_id":receipt_id, "receipt":receipt, "grade":verdict,
            "inclusive_native_tokens":fixture_tokens.filter(|_| started == 1), "af_charged_tokens":charged.to_string(),
            "elapsed_ns":elapsed_ns, "attempts":rows,
        }));
    }
    let pairs = slots
        .chunks_exact(2)
        .map(|pair| {
            let (baseline, treatment) = if pair[0]["arm"] == "baseline" {
                (&pair[0], &pair[1])
            } else {
                (&pair[1], &pair[0])
            };
            let elapsed_delta_ns = treatment["elapsed_ns"]
                .as_i64()
                .zip(baseline["elapsed_ns"].as_i64())
                .map(|(treatment, baseline)| treatment - baseline);
            let token_delta = treatment["inclusive_native_tokens"]
                .as_i64()
                .zip(baseline["inclusive_native_tokens"].as_i64())
                .map(|(treatment, baseline)| treatment - baseline);
            json!({"case":baseline["case"], "repetition":baseline["repetition"],
            "baseline_slot":baseline["index"], "treatment_slot":treatment["index"],
            "elapsed_delta_ns":elapsed_delta_ns, "inclusive_native_token_delta":token_delta})
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema":"af.agent-bench-report/2", "task_id":task.task_id, "plan_id":task.plan_id,
        "schedule_id":task.revision.authority.policy_id, "source_snapshot_id":snapshot,
        "source_digest":policy.source_digest, "cases_id":policy.cases, "oracle_id":policy.oracle,
        "rust_analyzer_version":semantic::VERSION, "rust_analyzer_sha256":policy.analyzer_sha256,
        "semantic_config":policy.semantic_config, "text_config":policy.common_config,
        "inference": if policy.native_fixture.is_some() { "native_codex_dummy_endpoint" } else { "synthetic_fixture" },
        "grader":"synthetic_anchor_oracle", "conclusion":"no_improvement_claim",
        "slots":slots, "pairs":pairs,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_baseline_semantic_call_is_refused_before_backend_dispatch() {
        let mut trace = Vec::new();
        let broker = navigation::Broker::new(BTreeMap::from([(
            "src/lib.rs".into(),
            b"fn test() {}".to_vec(),
        )]))
        .unwrap();
        let mut tools = Tools {
            arm: Arm::Baseline,
            broker,
            deadline: Instant::now() + Duration::from_secs(1),
            cancellation: None,
            trace: &mut trace,
        };
        let position = semantic::Position {
            path: "src/lib.rs".into(),
            line: 0,
            character: 3,
        };
        let analyzer = Analyzer {
            bytes: &[],
            digest: "",
            project: &[],
            libraries: None,
        };
        assert_eq!(
            tools
                .semantic(semantic::Operation::Definition, &position, &analyzer)
                .unwrap_err(),
            "Semantic tool is unavailable to the baseline"
        );
        assert!(trace.is_empty());
    }

    #[test]
    fn smoke_oracle_is_separate_and_only_exact_source_anchors_can_grade_correct() {
        let temp = tempfile::tempdir().unwrap();
        let cas = Cas::open(temp.path()).unwrap();
        let (manifest, cases, mut oracle) = smoke(&cas).unwrap();
        validate_packages(&cas, &manifest, &cases, &oracle).unwrap();
        let expected = &oracle.expected["definition"];
        let answer = semantic::Reply {
            anchors: expected.clone(),
            truncated: false,
        };
        assert_eq!(grade(&answer, expected), "correct");
        assert_eq!(
            grade(
                &semantic::Reply {
                    truncated: true,
                    ..answer
                },
                expected
            ),
            "ambiguous"
        );
        oracle.expected.get_mut("definition").unwrap()[0].path = "oracle/private.json".into();
        assert!(
            validate_packages(&cas, &manifest, &cases, &oracle)
                .unwrap_err()
                .contains("absent")
        );
        assert!(
            manifest
                .entries
                .iter()
                .all(|entry| !entry.path.contains("oracle"))
        );
    }
}
