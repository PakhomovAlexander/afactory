//! Capture one frozen paired schedule in the ordinary Task Store.
pub(super) mod external;
mod native_fixture;
mod subject;

use super::*;
use af_contracts::task::pipeline::{
    PipelineApplicabilityV1, PipelineContractV1, PipelineDefinition, PipelinePortV1,
    PipelineSchema, PortAffinityV1, TaskNodeV1, TaskOperatorV1, ValueRefV1,
};
use af_graph::task::{OperatorAttemptCost, OperatorSignature};
use af_pipeline_config::task::catalog::TaskPackagePin;
use af_sandbox::navigation::{self, semantic};
use clap::{ArgGroup, Args, Subcommand};
use sha2::{Digest, Sha256};

pub(super) const KIND: &str = "agent-bench";
const PIPELINE: &str = "af/agent-bench";
const POLICY: &str = "af/AgentBenchPolicy@2";
const SLOT: &str = "af/AgentBenchSlot@1";
const RECEIPT: &str = "af/AgentBenchReceipt@2";
const CASES: &str = "af/AgentBenchCases@1";
const ORACLE: &str = "af/AgentBenchOracle@1";
const WALL_MS: u64 = 35_000;
const FIXTURE_TOKENS: u64 = 2_000;
// The native compiler caps the graph at 64 nodes, including root inputs.
const MAX_CASES: usize = 10;
const MAX_SLOTS: usize = MAX_CASES * 2 * 3;

#[derive(Debug, Subcommand)]
pub(crate) enum BenchmarkCommand {
    /// Freeze a paired navigation schedule without starting any subjects
    Plan(Box<BenchmarkPlan>),
    /// Replay the captured schedule and retained observations
    Report {
        id: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        state: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Validate a private evaluator package against a captured external Task
    CheckEvaluator(external::EvaluatorCheck),
    /// Capture a synthetic blind grading plan for a finished external smoke Task
    PlanGrading(external::grading::GradingPlan),
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("input_mode").args(["smoke", "cases", "bundle"]).required(true)))]
pub(crate) struct BenchmarkPlan {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long)]
    state: Option<PathBuf>,
    #[arg(long)]
    smoke: bool,
    #[arg(long, requires_all = ["cases_sha256", "oracle", "oracle_sha256", "source_revision"])]
    cases: Option<PathBuf>,
    #[arg(long, requires = "cases")]
    cases_sha256: Option<String>,
    #[arg(long, requires = "cases")]
    oracle: Option<PathBuf>,
    #[arg(long, requires = "cases")]
    oracle_sha256: Option<String>,
    #[arg(long, conflicts_with = "smoke")]
    source_revision: Option<String>,
    #[command(flatten)]
    external: external::Options,
    #[arg(long, required_unless_present = "bundle")]
    rust_analyzer: Option<PathBuf>,
    #[arg(long, required_unless_present = "bundle")]
    rust_analyzer_sha256: Option<String>,
    /// Directory containing the two exact Linux Rust 1.88 analyzer runtime libraries.
    #[arg(long)]
    rust_analyzer_library_dir: Option<PathBuf>,
    /// Use the pinned Linux Codex app-server for the smoke prompts.
    #[arg(long, requires_all = ["smoke", "fixture_endpoint", "rust_analyzer_library_dir"])]
    native_fixture_codex: Option<PathBuf>,
    /// Credential-free Responses endpoint, restricted to 127.0.0.1.
    #[arg(long, requires = "native_fixture_codex")]
    fixture_endpoint: Option<std::net::SocketAddrV4>,
    #[arg(long)]
    json: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cases {
    schema: String,
    source_digest: String,
    project: Vec<semantic::Crate>,
    cases: Vec<Case>,
    repetitions: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    question: String,
    literal: String,
    position: semantic::Position,
    operation: semantic::Operation,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Arm {
    Baseline,
    Treatment,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Slot {
    index: usize,
    repetition: u32,
    arm: Arm,
    case: Case,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Oracle {
    schema: String,
    source_digest: String,
    #[serde(deserialize_with = "oracle_entries")]
    expected: BTreeMap<String, Vec<semantic::Anchor>>,
}

fn oracle_entries<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Vec<semantic::Anchor>>, D::Error> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = BTreeMap<String, Vec<semantic::Anchor>>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of unique oracle case IDs")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut input: A,
        ) -> Result<Self::Value, A::Error> {
            let mut entries = BTreeMap::new();
            while let Some((case, anchors)) = input.next_entry()? {
                if entries.insert(case, anchors).is_some() {
                    return Err(serde::de::Error::custom("Duplicate oracle case ID"));
                }
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_map(Entries)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    engine: String,
    source_digest: String,
    analyzer: String,
    analyzer_sha256: String,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    analyzer_libraries: Option<[String; 2]>,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    native_fixture: Option<native_fixture::Configuration>,
    semantic_config: String,
    common_config: String,
    cases: String,
    oracle: String,
    project: Vec<semantic::Crate>,
    slots: Vec<Slot>,
}

pub(crate) fn dispatch(command: BenchmarkCommand) -> Result<i32, String> {
    match command {
        BenchmarkCommand::Plan(options) => plan(*options),
        BenchmarkCommand::PlanGrading(options) => external::grading::plan(options),
        BenchmarkCommand::CheckEvaluator(options) => external::check_evaluator(options),
        BenchmarkCommand::Report {
            id,
            repo,
            state,
            json,
        } => report(&id, &repo, state.as_deref(), json),
    }
}

fn put<T: Serialize>(
    cas: &Cas,
    kind: &str,
    value: &T,
    refs: Vec<String>,
    snapshot: Option<String>,
) -> Result<String, String> {
    cas.put_artifact(
        kind,
        producer(),
        refs,
        snapshot,
        serde_json::to_value(value).map_err(|e| e.to_string())?,
    )
    .map(|(id, _)| id)
    .map_err(|e| e.to_string())
}

fn file_sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        af_contracts::hex::encode(&Sha256::digest(bytes))
    )
}

fn read_bytes(path: &Path, digest: &str, limit: u64) -> Result<Vec<u8>, String> {
    let bytes = af_store::local_fs::read_bounded(path, limit).map_err(|e| e.to_string())?;
    if file_sha256(&bytes) != digest {
        return Err(format!("Pin differs from bytes at {}", path.display()));
    }
    Ok(bytes)
}

struct CaptureInputs {
    manifest: Manifest,
    source: ArtifactInputV1,
    cases: Cases,
    oracle: Oracle,
    pins: Vec<String>,
}

fn capture(cas: &Cas, repo: &Path, options: &BenchmarkPlan) -> Result<CaptureInputs, String> {
    let (manifest, repository_id, revision, cases, oracle, pins) = if options.smoke {
        let (manifest, cases, oracle) = subject::smoke(cas)?;
        (
            manifest,
            "af-agent-bench-synthetic".into(),
            None,
            cases,
            oracle,
            vec![],
        )
    } else {
        let source = capture_commit(cas, repo, options.source_revision.as_deref())?;
        let cases_raw = read_bytes(
            options.cases.as_deref().ok_or("Missing --cases")?,
            options
                .cases_sha256
                .as_deref()
                .ok_or("Missing --cases-sha256")?,
            262_144,
        )?;
        let oracle_raw = read_bytes(
            options.oracle.as_deref().ok_or("Missing --oracle")?,
            options
                .oracle_sha256
                .as_deref()
                .ok_or("Missing --oracle-sha256")?,
            262_144,
        )?;
        let cases = serde_json::from_slice(&cases_raw).map_err(|e| e.to_string())?;
        let oracle = serde_json::from_slice(&oracle_raw).map_err(|e| e.to_string())?;
        let pins = [cases_raw, oracle_raw]
            .iter()
            .map(|bytes| cas.put(bytes).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        if source
            .manifest
            .entries
            .iter()
            .any(|e| pins.contains(&e.content))
        {
            return Err(
                "The case or oracle package is present in the proposed source Manifest".into(),
            );
        }
        (
            source.manifest,
            source.repository_id,
            source.source_revision,
            cases,
            oracle,
            pins,
        )
    };
    subject::validate_packages(cas, &manifest, &cases, &oracle)?;
    let input = source_input(cas, &manifest, repository_id, revision)?;
    Ok(CaptureInputs {
        manifest,
        source: input,
        cases,
        oracle,
        pins,
    })
}

fn capture_commit(
    cas: &Cas,
    repo: &Path,
    oid: Option<&str>,
) -> Result<af_source_git::Snapshot, String> {
    let oid = oid.ok_or("Missing --source-revision")?;
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Supply an exact Git commit ID in --source-revision".into());
    }
    let home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let git = af_source_git::Repo::open(repo, home.path());
    let source = af_source_git::Capture::new(&git, cas)
        .committed(oid)
        .map_err(|e| e.to_string())?;
    if !source.submodules.is_empty() || source.source_revision.as_deref() != Some(oid) {
        return Err("External source must be an exact commit without Git submodules".into());
    }
    Ok(source)
}

fn source_input(
    cas: &Cas,
    manifest: &Manifest,
    repository_id: String,
    revision: Option<String>,
) -> Result<ArtifactInputV1, String> {
    use af_source_git::task::{TASK_SOURCE_ORIGIN_SCHEMA, TaskSourceOrigin};
    let origin = af_source_git::task::write_origin(
        cas,
        &TaskSourceOrigin::Captured {
            schema: TASK_SOURCE_ORIGIN_SCHEMA.into(),
            repository_id,
            content_digest: manifest.content_digest(),
            source_revision: revision,
        },
    )?;
    let snapshot = af_source_git::task::capture_snapshot(cas, manifest, &origin, None)?;
    af_source_git::task::source_tree(cas, producer(), &snapshot, vec![])
}

fn port(kind: &str) -> PipelinePortV1 {
    PipelinePortV1 {
        artifact_type: kind.into(),
        cardinality: PortCardinality::One,
        optional: false,
        affinity: PortAffinityV1::Unbound {},
        root_default: None,
        brief: None,
        covers: BTreeSet::new(),
    }
}

fn node_name(index: usize) -> String {
    format!("slot-{index:03}")
}

fn compiler(cas: &Cas, policy_id: &str, policy: &Policy) -> Result<TaskPlanCompiler, String> {
    if policy.engine != engine(cas)?
        || policy.semantic_config != semantic::config_identity()
        || policy.common_config != navigation::Broker::conformance_identity()
        || policy.analyzer_libraries.as_ref().is_some_and(|libraries| {
            policy.analyzer_sha256 != semantic::LINUX_ANALYZER_SHA256
                || libraries.iter().any(|id| !af_contracts::is_digest(id))
        })
        || !(2..=MAX_SLOTS).contains(&policy.slots.len())
    {
        return Err("Benchmark requires its exact recorded engine, tools and schedule".into());
    }
    if let Some(fixture) = &policy.native_fixture {
        fixture.validate()?;
        if policy.analyzer_libraries.is_none() || policy.slots.len() != 6 {
            return Err("Native fixture requires the pinned runtime and six smoke slots".into());
        }
    }
    compile_slots(
        cas,
        &policy.engine,
        policy_id,
        SlotPipeline {
            kind: KIND,
            slot_type: SLOT,
            receipt_type: RECEIPT,
            obligation: "collected",
            bindings_type: None,
            count: policy.slots.len(),
            tokens: policy.fixture_tokens(),
            wall_ms: WALL_MS,
        },
    )
}

struct SlotPipeline<'a> {
    kind: &'a str,
    slot_type: &'a str,
    receipt_type: &'a str,
    obligation: &'a str,
    bindings_type: Option<&'a str>,
    count: usize,
    tokens: u64,
    wall_ms: u64,
}

fn compile_slots(
    cas: &Cas,
    engine_id: &str,
    policy_id: &str,
    slots: SlotPipeline<'_>,
) -> Result<TaskPlanCompiler, String> {
    let SlotPipeline {
        kind,
        slot_type,
        receipt_type,
        obligation,
        bindings_type,
        count,
        tokens,
        wall_ms,
    } = slots;
    let effects = BTreeSet::from(["read-source".into(), "execute-checks".into()]);
    let mut predecessor = port(receipt_type);
    predecessor.optional = true;
    let source = port(af_source_git::task::SOURCE_TREE_V1);
    let mut receipt = port(receipt_type);
    receipt.affinity = PortAffinityV1::SameAs {
        input: "source".into(),
    };
    let mut operator_inputs = BTreeMap::from([
        ("source".into(), source.clone()),
        ("slot".into(), port(slot_type)),
        ("previous".into(), predecessor),
    ]);
    if let Some(bindings) = bindings_type {
        operator_inputs.insert("bindings".into(), port(bindings));
    }
    let contract = PipelineContractV1 {
        inputs: operator_inputs,
        outputs: BTreeMap::from([("receipt".into(), receipt.clone())]),
    };
    let signature = OperatorSignature {
        contract,
        effects,
        evidence: BTreeMap::from([("receipt".into(), BTreeSet::from([policy_id.into()]))]),
        retains: BTreeMap::new(),
        roles: BTreeSet::new(),
        worker_input_type: None,
        worker_output_type: None,
        outcome_port: None,
        unbound_allowed: BTreeSet::from(["previous".into()]),
        attempt: Some(OperatorAttemptCost { tokens, wall_ms }),
    };
    let final_index = count - 1;
    let output_ref = |index| ValueRefV1::Node {
        node: node_name(index),
        port: "receipt".into(),
    };
    let input_ref = |port: &str| ValueRefV1::Input { port: port.into() };
    let mut inputs = BTreeMap::from([("source".into(), source)]);
    if let Some(bindings) = bindings_type {
        inputs.insert("bindings".into(), port(bindings));
    }
    let mut outputs = BTreeMap::new();
    let mut nodes = Vec::new();
    let mut public = BTreeMap::new();
    for index in 0..=final_index {
        let name = node_name(index);
        inputs.insert(name.clone(), port(slot_type));
        outputs.insert(name.clone(), output_ref(index));
        let mut output = receipt.clone();
        if index == final_index {
            output.covers.insert(obligation.into());
        }
        public.insert(name.clone(), output);
        let mut binds = BTreeMap::from([
            ("source".into(), input_ref("source")),
            ("slot".into(), input_ref(&name)),
        ]);
        if bindings_type.is_some() {
            binds.insert("bindings".into(), input_ref("bindings"));
        }
        if let Some(previous) = index.checked_sub(1) {
            binds.insert("previous".into(), output_ref(previous));
        }
        nodes.push(TaskNodeV1 {
            id: name,
            operator: TaskOperatorV1::AgentBenchSubject {},
            inputs: binds,
            when: None,
        });
    }
    let pipeline = PipelineDefinition {
        schema: PipelineSchema::V6,
        name: PIPELINE.into(),
        version: "1.0.0".into(),
        contract: PipelineContractV1 {
            inputs,
            outputs: public,
        },
        accepts: PipelineApplicabilityV1 {
            kinds: BTreeSet::from([kind.into()]),
            required_facts: BTreeMap::new(),
        },
        slots: BTreeMap::new(),
        nodes,
        outputs,
        coverage: BTreeMap::from([(obligation.into(), output_ref(final_index))]),
        max_attempts: count as u32,
        max_parallel: 1,
    };
    let mut compiler = TaskPlanCompiler::new(
        engine_id.into(),
        policy_id.into(),
        BTreeMap::from([("operator/agent-bench-subject".into(), signature)]),
        BTreeMap::from([(obligation.into(), node_name(final_index))]),
        Default::default(),
    )?;
    let files = BTreeMap::from([(
        "pipeline.toml".into(),
        toml::to_string(&pipeline)
            .map_err(|e| e.to_string())?
            .into_bytes(),
    )]);
    let digest = af_pipeline_config::lock::package_digest_from_files(&files);
    let project = files
        .into_iter()
        .map(|(name, data)| (format!("benchmark/{name}"), data))
        .collect();
    compiler.capture_package(
        cas,
        PIPELINE,
        &TaskPackagePin {
            path: "benchmark".into(),
            version: "1.0.0".into(),
            digest,
        },
        &project,
    )?;
    Ok(compiler)
}

impl Policy {
    fn fixture_tokens(&self) -> u64 {
        self.native_fixture.as_ref().map_or(0, |_| FIXTURE_TOKENS)
    }
}

fn schedule(cases: &Cases) -> Vec<Slot> {
    let mut slots = Vec::new();
    for repetition in 1..=cases.repetitions {
        let mut order: Vec<_> = cases.cases.iter().collect();
        order.sort_by_key(|case| {
            af_source_git::digest_bytes(
                format!(
                    "af-agent-bench-order/1\0{}\0{repetition}\0{}",
                    cases.source_digest, case.id
                )
                .as_bytes(),
            )
        });
        for (index, case) in order.into_iter().enumerate() {
            let mut arms = [Arm::Baseline, Arm::Treatment];
            if (index + repetition as usize).is_multiple_of(2) {
                arms.reverse();
            }
            for arm in arms {
                slots.push(Slot {
                    index: slots.len(),
                    repetition,
                    arm,
                    case: case.clone(),
                });
            }
        }
    }
    slots
}

fn plan(options: BenchmarkPlan) -> Result<i32, String> {
    if options.external.bundle.is_some() {
        return external::plan(options);
    }
    let analyzer_sha256 = options
        .rust_analyzer_sha256
        .as_deref()
        .ok_or("Missing analyzer pin")?;
    let native_fixture = options
        .native_fixture_codex
        .as_ref()
        .map(|path| native_fixture::Configuration::capture(path, options.fixture_endpoint))
        .transpose()?;
    let binary = read_bytes(
        options.rust_analyzer.as_deref().ok_or("Missing analyzer")?,
        analyzer_sha256,
        semantic::MAX_BINARY,
    )?;
    let (repo, state) = state_path(&options.repo, options.state.as_deref())?;
    crate::storage::owned::prepare(&state, options.state.is_some())?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let CaptureInputs {
        manifest,
        source,
        cases,
        oracle,
        pins,
    } = capture(&cas, &repo, &options)?;
    let slots = schedule(&cases);
    let snapshot = source.snapshot_id.clone();
    let cases_id = put(
        &cas,
        CASES,
        &cases,
        pins.first().cloned().into_iter().collect(),
        snapshot.clone(),
    )?;
    let oracle_id = put(
        &cas,
        ORACLE,
        &oracle,
        pins.get(1).cloned().into_iter().collect(),
        snapshot.clone(),
    )?;
    let binary_id = cas.put(&binary).map_err(|e| e.to_string())?;
    let analyzer_libraries = options
        .rust_analyzer_library_dir
        .map(|directory| -> Result<_, String> {
            if analyzer_sha256 != semantic::LINUX_ANALYZER_SHA256 {
                return Err("Analyzer libraries require the exact pinned Linux executable".into());
            }
            let [driver, llvm] = semantic::LINUX_LIBRARIES.map(|(name, digest, size)| {
                let bytes = read_bytes(&directory.join(name), digest, size)?;
                cas.put(&bytes).map_err(|e| e.to_string())
            });
            Ok([driver?, llvm?])
        })
        .transpose()?;
    let policy = Policy {
        engine: engine(&cas)?,
        source_digest: manifest.content_digest(),
        analyzer: binary_id,
        analyzer_sha256: analyzer_sha256.into(),
        analyzer_libraries,
        native_fixture,
        semantic_config: semantic::config_identity(),
        common_config: navigation::Broker::conformance_identity(),
        cases: cases_id,
        oracle: oracle_id,
        project: cases.project,
        slots,
    };
    let mut refs = vec![
        policy.cases.clone(),
        policy.oracle.clone(),
        policy.analyzer.clone(),
    ];
    refs.extend(policy.analyzer_libraries.iter().flatten().cloned());
    let policy_id = put(&cas, POLICY, &policy, refs.clone(), snapshot.clone())?;
    let mut inputs = BTreeMap::from([("source".into(), source)]);
    let mut required_outputs = BTreeMap::new();
    refs.extend(pins);
    for slot in &policy.slots {
        let name = node_name(slot.index);
        let slot_id = put(&cas, SLOT, slot, vec![policy.cases.clone()], None)?;
        inputs.insert(
            name.clone(),
            ArtifactInputV1 {
                artifact_type: SLOT.into(),
                cardinality: PortCardinality::One,
                artifact_ids: vec![slot_id],
                snapshot_id: None,
            },
        );
        required_outputs.insert(
            name,
            RequiredOutputV1 {
                artifact_type: RECEIPT.into(),
                cardinality: PortCardinality::One,
            },
        );
    }
    refs.sort();
    refs.dedup();
    let task_id = format!("agent-bench-{:016x}", opening_nonce());
    let tokens = policy.fixture_tokens() * policy.slots.len() as u64;
    let revision = TaskRevisionV1 {
        task_id: task_id.clone(),
        revision: 1,
        previous_revision_id: None,
        kind: KIND.into(),
        goal: match &policy.native_fixture {
            Some(_) => {
                "Collect paired smoke prompts with the pinned Codex app-server and dummy endpoint."
            }
            None => "Collect the frozen paired navigation schedule using synthetic inference.",
        }
        .into(),
        inputs,
        required_outputs,
        acceptance: BTreeMap::from([(
            "collected".into(),
            AcceptanceObligationV1 {
                evidence_type: RECEIPT.into(),
                verifier_policy: policy_id.clone(),
            },
        )]),
        provenance: TaskProvenanceV1 {
            adapter_id: policy_id.clone(),
            input_artifact_ids: refs,
        },
        authority: TaskAuthorityV1 {
            policy_id: policy_id.clone(),
            allowed_effects: BTreeSet::from(["read-source".into(), "execute-checks".into()]),
            data_destinations: policy
                .native_fixture
                .iter()
                .map(|_| "native-dummy-responses".into())
                .collect(),
        },
        limits: TaskLimitsV1 {
            tokens,
            max_attempts: policy.slots.len() as u32,
            deadline_unix_ms: clock()?
                .checked_add(WALL_MS * (policy.slots.len() as u64 + 1))
                .ok_or("Benchmark deadline overflow")?,
            verification: VerificationReserveV1 {
                tokens,
                attempts: policy.slots.len() as u32,
                wall_ms: WALL_MS * policy.slots.len() as u64,
            },
        },
        strategy: "paired".into(),
        pipeline: Some(PipelineChoiceV1 {
            name: PIPELINE.into(),
            fallback: PipelineFallbackV1::Refuse,
        }),
        facts: BTreeMap::new(),
    };
    revision.validate()?;
    let revision_id = put(
        &cas,
        af_contracts::task::TASK_REVISION_V1,
        &revision,
        vec![],
        None,
    )?;
    let compiler = compiler(&cas, &policy_id, &policy)?;
    let (plan, graph) = compiler.compile(&cas, &revision_id, PIPELINE)?;
    let plan_id = put(
        &cas,
        EXECUTION_PLAN_V4,
        &plan,
        vec![revision_id.clone()],
        None,
    )?;
    let domain = subject::Domain { policy, graph };
    let trusted = CapturedTaskAuthority::new(&compiler, &domain, &NoTaskDeveloper);
    let writer = format!("cli-{}-{:016x}", writer_pid(), opening_nonce());
    let lease = store
        .open_task(&cas, &revision_id, &writer, recovery::lease_ms())
        .map_err(|e| e.to_string())?;
    let proposed = store
        .propose_task_plan(&cas, &lease, &plan_id, &trusted)
        .map(|_| ())
        .map_err(|e| e.to_string());
    release(&cas, &mut store, &lease, proposed)?;
    if options.json {
        println!(
            "{}",
            json!({"schema":"af.agent-bench-plan/1", "task_id":task_id,
            "plan_id":plan_id, "schedule_id":policy_id, "slots":domain.policy.slots.len()})
        );
    } else {
        println!(
            "Task {task_id}\nPlan {plan_id}\n{} paired subject slots",
            domain.policy.slots.len()
        );
    }
    Ok(0)
}

pub(super) fn restore(
    cas: &Cas,
    revision: &TaskRevisionV1,
    graph: CompiledTask,
) -> Result<(TaskPlanCompiler, Box<dyn TaskDomain>), String> {
    let id = &revision.authority.policy_id;
    let policy: Policy = artifact(cas, id, POLICY)?;
    let compiler = compiler(cas, id, &policy)?;
    Ok((compiler, Box::new(subject::Domain { policy, graph })))
}

fn report(id: &str, repo: &Path, state: Option<&Path>, json: bool) -> Result<i32, String> {
    let (_repo, state) = state_path(repo, state)?;
    require_store(&state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let task = store
        .task_projection(&cas, id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| task_not_found(&state, id))?;
    if task.revision.kind == external::grading::KIND {
        return external::grading::report(&cas, &store, &task, json);
    }
    if task.revision.kind == external::KIND {
        return external::report(&cas, &store, &task, json);
    }
    if task.revision.kind != KIND {
        return Err("Task is not an agent-bench schedule".into());
    }
    let policy: Policy = artifact(&cas, &task.revision.authority.policy_id, POLICY)?;
    let report = subject::report(&cas, &policy, &task)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Task {id} · {}",
            report["inference"]
                .as_str()
                .ok_or("Missing inference mode")?
        );
        for slot in report["slots"]
            .as_array()
            .ok_or("Missing schedule report slots")?
        {
            println!(
                "{} {} {} {} {}",
                slot["index"], slot["case"], slot["arm"], slot["status"], slot["grade"]
            );
        }
    }
    Ok(0)
}
