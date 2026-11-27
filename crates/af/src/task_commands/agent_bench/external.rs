//! Frozen prompt-only phase plans, native smoke execution, and offline replay.
mod evaluator;
mod execution;
pub(in crate::task_commands) mod grading;
mod phase_evidence;
use super::*;
use af_runner_codex::navigation::ToolSet;
pub(crate) use evaluator::EvaluatorCheck;
pub(super) use evaluator::check_evaluator;

pub(crate) const KIND: &str = "agent-bench-external";
const PLAN: &str = "af/AgentBenchExternalPlan@1";
const PLAN_SCHEMA: &str = "af.agent-bench-external-plan/1";
const REPORT_SCHEMA: &str = "af.agent-bench-external-report/1";
const PHASE_EVIDENCE: &str = "af/AgentBenchExternalPhaseEvidence@1";
const SOURCE_REVISION: &str = "02460368505f33c60f37e5333a15ac3541ad14f0";
const FROZEN_SNAPSHOT: &str =
    "sha256:bdd25ef0e2b443c9084399171ea32b5ad3d39e8c2ca5a829a57d572ea6de6f69";
const BUNDLE_PIN: &str = "sha256:705aae65a71a8b9c4fdf0073ffa8f327c203352bb1331deaa9b6ad8d5c4b60df";
const SCHEDULE_PIN: &str =
    "sha256:cca5f0d3bad2e854888024c3d22564621f536bd3a0db688dd065d9da2d5b6a00";
const INPUT_LIMIT: u64 = 262_144;
const REPETITIONS: u32 = 3;
const CASES_PER_REPETITION: usize = 12;
const FULL_PHASE_SLOTS: usize = CASES_PER_REPETITION * REPETITIONS as usize * 2;

#[derive(Debug, Args)]
pub(super) struct Options {
    /// Original prompt-only subject package.
    #[arg(long, requires_all = ["schedule", "phase", "project_graph", "source_revision"])]
    pub(super) bundle: Option<PathBuf>,
    /// SHA-256 of the subject package (defaults to the frozen experiment pin).
    #[arg(long, requires = "bundle")]
    bundle_sha256: Option<String>,
    /// JSON file containing the ordered pair blocks for the external benchmark.
    #[arg(long, requires = "bundle")]
    schedule: Option<PathBuf>,
    /// SHA-256 of the ordered pair blocks (defaults to the frozen experiment pin).
    #[arg(long, requires = "bundle")]
    schedule_sha256: Option<String>,
    /// External benchmark phase to plan. Smoke takes the first two pairs.
    #[arg(long, requires = "bundle")]
    phase: Option<Phase>,
    /// External explicit Rust crate graph, separate from the prompts.
    #[arg(long, requires_all = ["bundle", "project_graph_sha256"])]
    project_graph: Option<PathBuf>,
    /// SHA-256 digest of the exact external crate graph file bytes.
    #[arg(long, requires = "project_graph")]
    project_graph_sha256: Option<String>,
    /// Strict JSON file declaring external smoke transport bindings.
    #[arg(long = "external-transport", requires_all = ["bundle", "transport_sha256"])]
    transport: Option<PathBuf>,
    /// SHA-256 of the external smoke transport declaration.
    #[arg(long = "external-transport-sha256", requires = "transport")]
    transport_sha256: Option<String>,
    /// Checked, accepted Task from the preceding phase in this Store.
    #[arg(long, requires = "bundle")]
    predecessor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Smoke,
    Aa,
    Ab,
}

impl Phase {
    fn predecessor(self) -> Option<Self> {
        match self {
            Self::Smoke => None,
            Self::Aa => Some(Self::Smoke),
            Self::Ab => Some(Self::Aa),
        }
    }

    fn slots(self) -> usize {
        if self == Self::Smoke {
            4
        } else {
            FULL_PHASE_SLOTS
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Aa => "aa",
            Self::Ab => "ab",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema: String,
    common_instruction: String,
    cases: Vec<PromptCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptCase {
    id: String,
    prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schedule {
    schema: String,
    status: String,
    seed: serde_json::Value,
    ordering: serde_json::Value,
    blocks: Vec<Block>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Block {
    pair_id: String,
    case_id: String,
    repetition: u32,
    order: [Variant; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Variant {
    V1,
    V2,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalSlot {
    index: usize,
    pair_id: String,
    case_id: String,
    repetition: u32,
    variant: Variant,
    tool_set_id: String,
    tools: Vec<String>,
    question: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalPlan {
    schema: String,
    phase: Phase,
    pins: Pins,
    common_instruction: String,
    slots: Vec<ExternalSlot>,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    predecessor: Option<Predecessor>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pins {
    source_revision: String,
    source_digest: String,
    bundle: InputPin,
    schedule: InputPin,
    project_graph: InputPin,
}

impl Pins {
    fn inputs(&self) -> [&str; 3] {
        [
            &self.bundle.object_id,
            &self.schedule.object_id,
            &self.project_graph.object_id,
        ]
    }

    fn validate(&self) -> Result<(), String> {
        if (self.source_revision == SOURCE_REVISION
            || self.bundle.sha256 == BUNDLE_PIN
            || self.schedule.sha256 == SCHEDULE_PIN)
            && (self.source_revision != SOURCE_REVISION
                || self.bundle.sha256 != BUNDLE_PIN
                || self.schedule.sha256 != SCHEDULE_PIN)
        {
            return Err("Frozen bundle, schedule and source revision must match their pins".into());
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Predecessor {
    task_id: String,
    revision_id: String,
    plan_id: String,
    result_id: String,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputPin {
    sha256: String,
    object_id: String,
}

fn capture_input(cas: &Cas, path: Option<&Path>, pin: &str) -> Result<InputPin, String> {
    let sha256 = if pin.starts_with("sha256:") {
        pin.to_owned()
    } else {
        format!("sha256:{pin}")
    };
    let bytes = read_bytes(
        path.ok_or("Missing external benchmark input")?,
        &sha256,
        INPUT_LIMIT,
    )?;
    let object_id = cas.put(&bytes).map_err(|e| e.to_string())?;
    Ok(InputPin { sha256, object_id })
}

fn read_input<T: serde::de::DeserializeOwned>(cas: &Cas, pin: &InputPin) -> Result<T, String> {
    let bytes = cas
        .get_bounded(&pin.object_id, INPUT_LIMIT)
        .map_err(|e| e.to_string())?;
    if file_sha256(&bytes) != pin.sha256 {
        return Err("Pin differs from captured external input bytes".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid external benchmark input: {e}"))
}

fn nonempty(text: &str, limit: usize) -> bool {
    !text.trim().is_empty() && text.len() <= limit && !text.contains('\0')
}

fn compile(
    cas: &Cas,
    manifest: &Manifest,
    phase: Phase,
    pins: &Pins,
) -> Result<(String, Vec<ExternalSlot>), String> {
    pins.validate()?;
    let inputs = pins.inputs();
    if pins.source_digest != manifest.content_digest()
        || manifest
            .entries
            .iter()
            .any(|entry| inputs.contains(&entry.content.as_str()))
    {
        return Err("External inputs must be separate from the exact captured source".into());
    }
    let project: Vec<semantic::Crate> = read_input(cas, &pins.project_graph)?;
    semantic::validate_project(&project, |path| {
        manifest
            .get(path)
            .is_some_and(|entry| entry.kind != EntryKind::Symlink)
    })
    .map_err(|_| "Invalid external Rust project graph")?;
    let bundle: Bundle = read_input(cas, &pins.bundle)?;
    let schedule: Schedule = read_input(cas, &pins.schedule)?;
    if !nonempty(&bundle.schema, 256)
        || !nonempty(&bundle.common_instruction, 65_536)
        || bundle.cases.len() != CASES_PER_REPETITION
    {
        return Err(
            "External subject package requires a common instruction and cases N01..N12".into(),
        );
    }
    let mut cases = BTreeMap::new();
    for case in bundle.cases {
        if !nonempty(&case.prompt, 65_536) || cases.insert(case.id, case.prompt).is_some() {
            return Err("External case prompts must be nonempty and case IDs unique".into());
        }
    }
    if (1..=CASES_PER_REPETITION).any(|index| !cases.contains_key(&format!("N{index:02}"))) {
        return Err("External subject case IDs must be N01..N12".into());
    }
    if !nonempty(&schedule.schema, 256)
        || !nonempty(&schedule.status, 256)
        || schedule.seed.is_null()
        || schedule.ordering.is_null()
        || schedule.blocks.len() != CASES_PER_REPETITION * REPETITIONS as usize
    {
        return Err("External schedule requires exactly 36 ordered blocks".into());
    }
    let mut pairs = BTreeSet::new();
    let mut case_repetitions = BTreeSet::new();
    let mut v1_first = [0; REPETITIONS as usize];
    for block in &schedule.blocks {
        if !af_contracts::task::is_name(&block.pair_id)
            || !pairs.insert(&block.pair_id)
            || !(1..=REPETITIONS).contains(&block.repetition)
            || !cases.contains_key(&block.case_id)
            || !case_repetitions.insert((&block.case_id, block.repetition))
            || block.order[0] == block.order[1]
        {
            return Err("Invalid external pair ID, case, repetition or variant order".into());
        }
        if block.order[0] == Variant::V1 {
            v1_first[block.repetition as usize - 1] += 1;
        }
    }
    if v1_first != [CASES_PER_REPETITION / 2; REPETITIONS as usize] {
        return Err("Each repetition requires six V1-first blocks".into());
    }

    let count = if phase == Phase::Smoke {
        2
    } else {
        schedule.blocks.len()
    };
    let mut slots = Vec::with_capacity(count * 2);
    for block in schedule.blocks.into_iter().take(count) {
        for variant in block.order {
            let tool_set = match (phase, variant) {
                (Phase::Smoke | Phase::Ab, Variant::V2) => ToolSet::Semantic,
                _ => ToolSet::Text,
            };
            let tools = tool_set.schemas();
            let tools = tools
                .as_array()
                .ok_or("Missing navigation tool catalog")?
                .iter()
                .map(|tool| {
                    tool["name"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or("Missing navigation tool name")
                })
                .collect::<Result<_, _>>()?;
            slots.push(ExternalSlot {
                index: slots.len(),
                pair_id: block.pair_id.clone(),
                question: cases[&block.case_id].clone(),
                case_id: block.case_id.clone(),
                repetition: block.repetition,
                variant,
                tool_set_id: tool_set.identity(),
                tools,
            });
        }
    }
    Ok((bundle.common_instruction, slots))
}

fn predecessor(
    cas: &Cas,
    store: &EventStore,
    phase: Phase,
    pins: &Pins,
    id: &str,
    bindings: Option<&execution::Configuration>,
) -> Result<(Predecessor, phase_evidence::Overhead), String> {
    let (previous, overhead, subject) = phase_evidence::lookup(cas, store, phase, pins, id)?;
    if let Some(bindings) = bindings {
        let checked = execution::read_configuration(cas, &subject)?
            .ok_or("Predecessor has no subject bindings")?;
        if !bindings.same_protocol(&checked) {
            return Err("Predecessor backend or pinned subject settings differ".into());
        }
        let homes = phase_evidence::ancestral_homes(cas, store, &previous)?;
        if bindings
            .auth_homes()
            .iter()
            .any(|home| homes.contains(home))
        {
            return Err("A new phase must use fresh native authentication homes".into());
        }
    }
    Ok((previous, overhead))
}

pub(super) fn plan(options: BenchmarkPlan) -> Result<i32, String> {
    let external = &options.external;
    let phase = external.phase.ok_or("Missing --phase")?;
    let (repo, state) = state_path(&options.repo, options.state.as_deref())?;
    crate::storage::owned::prepare(&state, options.state.is_some())?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let commit = capture_commit(&cas, &repo, options.source_revision.as_deref())?;
    let pins = Pins {
        source_revision: commit
            .source_revision
            .clone()
            .ok_or("Missing captured revision")?,
        source_digest: commit.manifest.content_digest(),
        bundle: capture_input(
            &cas,
            external.bundle.as_deref(),
            external.bundle_sha256.as_deref().unwrap_or(BUNDLE_PIN),
        )?,
        schedule: capture_input(
            &cas,
            external.schedule.as_deref(),
            external.schedule_sha256.as_deref().unwrap_or(SCHEDULE_PIN),
        )?,
        project_graph: capture_input(
            &cas,
            external.project_graph.as_deref(),
            external
                .project_graph_sha256
                .as_deref()
                .ok_or("Missing --project-graph-sha256")?,
        )?,
    };
    let (common_instruction, slots) = compile(&cas, &commit.manifest, phase, &pins)?;
    let configuration = if external.transport.is_some() {
        if phase.predecessor().is_some() && external.predecessor.is_none() {
            return Err("A measured phase requires a confirmed predecessor".into());
        }
        let configuration =
            execution::Configuration::capture(&cas, &options, &repo, &state, phase)?;
        if configuration.live() && pins.source_revision != SOURCE_REVISION {
            return Err("Codex Personal requires the frozen experiment source and prompts".into());
        }
        if slots
            .iter()
            .any(|slot| common_instruction.len() + 2 + slot.question.len() > 65_536)
        {
            return Err("External prompt exceeds the navigation transport bound".into());
        }
        Some(configuration)
    } else {
        if options.rust_analyzer.is_some()
            || options.rust_analyzer_sha256.is_some()
            || options.rust_analyzer_library_dir.is_some()
        {
            return Err("An offline external plan takes no runtime pins".into());
        }
        None
    };
    let (predecessor, prior_overhead) = external
        .predecessor
        .as_deref()
        .map(|id| predecessor(&cas, &store, phase, &pins, id, configuration.as_ref()))
        .transpose()?
        .map_or_else(
            || (None, Default::default()),
            |(previous, overhead)| (Some(previous), overhead),
        );
    let mut refs: Vec<_> = pins.inputs().into_iter().map(str::to_owned).collect();
    if let Some(previous) = &predecessor {
        refs.extend([
            previous.plan_id.clone(),
            previous.revision_id.clone(),
            previous.result_id.clone(),
        ]);
    }
    refs.sort();
    refs.dedup();
    let plan = ExternalPlan {
        schema: PLAN_SCHEMA.into(),
        phase,
        pins,
        common_instruction,
        slots,
        predecessor,
    };
    let source = source_input(
        &cas,
        &commit.manifest,
        commit.repository_id,
        commit.source_revision,
    )?;
    let snapshot = source.snapshot_id.clone();
    if configuration
        .as_ref()
        .is_some_and(execution::Configuration::live)
        && snapshot.as_deref() != Some(FROZEN_SNAPSHOT)
    {
        return Err("Codex Personal requires the frozen SourceTree Snapshot".into());
    }
    let phase_plan_id = put(&cas, PLAN, &plan, refs, snapshot.clone())?;
    let task_id = if configuration
        .as_ref()
        .is_some_and(execution::Configuration::live)
    {
        // A second live declaration cannot reopen this Store's frozen pilot with fresh spend.
        format!("agent-bench-external-personal-{}", phase.label())
    } else if let (Some(_), Some(previous)) = (&configuration, &plan.predecessor) {
        format!("{}-{}", previous.task_id, phase.label())
    } else {
        format!("agent-bench-{:016x}", opening_nonce())
    };
    let mut inputs = BTreeMap::from([("source".into(), source)]);
    let mut required_outputs = BTreeMap::new();
    let mut pipeline = None;
    let mut effects = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    let mut limits = TaskLimitsV1 {
        tokens: 0,
        max_attempts: 1,
        deadline_unix_ms: clock()?,
        verification: VerificationReserveV1 {
            tokens: 0,
            attempts: 0,
            wall_ms: 0,
        },
    };
    let evidence_type = match &configuration {
        Some(configuration) => {
            let bindings = put(
                &cas,
                execution::CONFIG,
                configuration,
                configuration.references(),
                snapshot.clone(),
            )?;
            inputs.insert(
                "bindings".into(),
                ArtifactInputV1 {
                    artifact_type: execution::CONFIG.into(),
                    cardinality: PortCardinality::One,
                    artifact_ids: vec![bindings],
                    snapshot_id: snapshot.clone(),
                },
            );
            for slot in &plan.slots {
                let name = node_name(slot.index);
                let id = put(
                    &cas,
                    execution::SLOT,
                    slot,
                    vec![phase_plan_id.clone()],
                    None,
                )?;
                inputs.insert(
                    name.clone(),
                    ArtifactInputV1 {
                        artifact_type: execution::SLOT.into(),
                        cardinality: PortCardinality::One,
                        artifact_ids: vec![id],
                        snapshot_id: None,
                    },
                );
                required_outputs.insert(
                    name,
                    RequiredOutputV1 {
                        artifact_type: execution::RECEIPT.into(),
                        cardinality: PortCardinality::One,
                    },
                );
            }
            pipeline = Some(PipelineChoiceV1 {
                name: PIPELINE.into(),
                fallback: PipelineFallbackV1::Refuse,
            });
            effects = BTreeSet::from(["read-source".into(), "execute-checks".into()]);
            destinations.insert(configuration.destination().into());
            let count = plan.slots.len() as u32;
            // The native reservations are independent of the inclusive, before-send pilot stop.
            limits = TaskLimitsV1 {
                tokens: execution::INCLUSIVE_LIMIT * u64::from(count),
                max_attempts: count,
                deadline_unix_ms: clock()?
                    .checked_add(execution::subject_wall_ms(phase) * u64::from(count) + WALL_MS)
                    .ok_or("External benchmark deadline overflow")?,
                verification: VerificationReserveV1 {
                    tokens: execution::INCLUSIVE_LIMIT * u64::from(count),
                    attempts: count,
                    wall_ms: execution::subject_wall_ms(phase) * u64::from(count),
                },
            };
            execution::RECEIPT
        }
        None => {
            required_outputs.insert(
                "phase-evidence".into(),
                RequiredOutputV1 {
                    artifact_type: PHASE_EVIDENCE.into(),
                    cardinality: PortCardinality::One,
                },
            );
            PHASE_EVIDENCE
        }
    };
    let revision = TaskRevisionV1 {
        task_id: task_id.clone(),
        revision: 1,
        previous_revision_id: None,
        kind: KIND.into(),
        goal: "Collect the frozen external benchmark phase's subject answers.".into(),
        inputs,
        required_outputs,
        acceptance: BTreeMap::from([(
            "collected".into(),
            AcceptanceObligationV1 {
                evidence_type: evidence_type.into(),
                verifier_policy: phase_plan_id.clone(),
            },
        )]),
        provenance: TaskProvenanceV1 {
            adapter_id: phase_plan_id.clone(),
            input_artifact_ids: vec![phase_plan_id.clone()],
        },
        authority: TaskAuthorityV1 {
            policy_id: phase_plan_id.clone(),
            allowed_effects: effects,
            data_destinations: destinations,
        },
        limits,
        strategy: "paired".into(),
        pipeline,
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
    let count = plan.slots.len();
    let prerequisite = prerequisite(&plan);
    let mut plan_id = phase_plan_id.clone();
    let mut mode = "unavailable";
    let compiled = if let Some(configuration) = configuration {
        mode = configuration.mode();
        let compiler = configuration.compiler(&cas, &phase_plan_id, phase)?;
        let (compiled, graph) = compiler.compile(&cas, &revision_id, PIPELINE)?;
        plan_id = put(
            &cas,
            EXECUTION_PLAN_V4,
            &compiled,
            vec![revision_id.clone()],
            None,
        )?;
        Some((
            compiler,
            execution::Domain {
                prior_overhead,
                plan,
                configuration,
                graph,
                database: state.join("events.sqlite"),
                repo,
            },
        ))
    } else {
        None
    };
    let writer = format!("cli-{}-{:016x}", writer_pid(), opening_nonce());
    let lease = store
        .open_task(&cas, &revision_id, &writer, recovery::lease_ms())
        .map_err(|e| e.to_string())?;
    let proposed = if let Some((compiler, domain)) = &compiled {
        let trusted = CapturedTaskAuthority::new(compiler, domain, &NoTaskDeveloper);
        store
            .propose_task_plan(&cas, &lease, &plan_id, &trusted)
            .map(|_| ())
            .map_err(|e| e.to_string())
    } else {
        Ok(())
    };
    release(&cas, &mut store, &lease, proposed)?;
    if options.json {
        println!(
            "{}",
            json!({
                "schema":if compiled.is_some() { "af.agent-bench-external-plan-summary/3" } else { "af.agent-bench-external-plan-summary/1" }, "task_id":task_id,
                "plan_id":plan_id, "phase":phase, "slots":count,
                "prerequisite": prerequisite, "execution":mode
            })
        );
    } else {
        println!(
            "Task {task_id}\nExternal plan {plan_id}\n{count} paired subject slots\nPrerequisite {prerequisite}\nExecution {mode}"
        );
    }
    Ok(0)
}

fn prerequisite(plan: &ExternalPlan) -> &'static str {
    match (plan.phase.predecessor(), &plan.predecessor) {
        (None, _) => "none",
        (Some(_), None) => "pending",
        (Some(_), Some(_)) => "accepted",
    }
}

fn read_plan(cas: &Cas, task: &TaskProjection) -> Result<ExternalPlan, String> {
    if task.revision.kind != KIND {
        return Err("Task is not an external benchmark phase".into());
    }
    let artifact_id = &task.revision.authority.policy_id;
    let plan: ExternalPlan = artifact(cas, artifact_id, PLAN)?;
    let source = task
        .revision
        .inputs
        .get("source")
        .ok_or("Missing benchmark SourceTree")?;
    let (_, snapshot, manifest) = af_runtime::task::source::source_snapshot(cas, source)?;
    let origin = af_source_git::task::read_origin(cas, &snapshot.origin_id)?;
    if plan.schema != PLAN_SCHEMA
        || origin.source_revision() != Some(&plan.pins.source_revision)
        || (plan.phase == Phase::Smoke && plan.predecessor.is_some())
    {
        return Err("External plan differs from its captured source or phase".into());
    }
    let (common_instruction, slots) = compile(cas, &manifest, plan.phase, &plan.pins)?;
    if plan.common_instruction != common_instruction || plan.slots != slots {
        return Err("External plan differs from its captured prompts or schedule".into());
    }
    Ok(plan)
}

pub(in crate::task_commands) fn require_run_confirmation(
    cas: &Cas,
    task: &TaskProjection,
    confirm: Option<&str>,
    retry_gate: bool,
) -> Result<(), String> {
    let plan = read_plan(cas, task)?;
    if (plan.phase.predecessor().is_some() && plan.predecessor.is_none())
        || !task.revision.inputs.contains_key("bindings")
        || task.plan_id.is_none()
    {
        return Err("External benchmark plans without admitted bindings are offline only.".into());
    }
    if retry_gate || confirm.is_none() {
        return Err(
            "External phase requires --confirm-plan with the exact captured execution plan".into(),
        );
    }
    Ok(())
}

pub(in crate::task_commands) fn restore(
    cas: &Cas,
    task: &TaskProjection,
    graph: CompiledTask,
    state: &Path,
    repo: PathBuf,
) -> Result<(TaskPlanCompiler, Box<dyn TaskDomain>), String> {
    let plan = read_plan(cas, task)?;
    let configuration =
        execution::read_configuration(cas, task)?.ok_or("Missing external bindings")?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let prior_overhead = check_predecessor(cas, &store, &plan, Some(&configuration))?;
    let compiler = configuration.compiler(cas, &task.revision.authority.policy_id, plan.phase)?;
    Ok((
        compiler,
        Box::new(execution::Domain {
            prior_overhead,
            plan,
            configuration,
            graph,
            database: state.join("events.sqlite"),
            repo,
        }),
    ))
}

fn check_predecessor(
    cas: &Cas,
    store: &EventStore,
    plan: &ExternalPlan,
    configuration: Option<&execution::Configuration>,
) -> Result<phase_evidence::Overhead, String> {
    if let Some(previous) = &plan.predecessor {
        let (checked, overhead) = predecessor(
            cas,
            store,
            plan.phase,
            &plan.pins,
            &previous.task_id,
            configuration,
        )?;
        if checked != *previous {
            return Err("Predecessor evidence differs from its checked Task result".into());
        }
        return Ok(overhead);
    } else if configuration.is_some() && plan.phase.predecessor().is_some() {
        return Err("A measured phase requires a confirmed predecessor".into());
    }
    Ok(Default::default())
}

pub(super) fn report(
    cas: &Cas,
    store: &EventStore,
    task: &TaskProjection,
    json: bool,
) -> Result<i32, String> {
    let plan = read_plan(cas, task)?;
    let configuration = execution::read_configuration(cas, task)?;
    check_predecessor(cas, store, &plan, configuration.as_ref())?;
    if let Some(configuration) = configuration {
        return execution::report(cas, store, task, &plan, &configuration, json);
    }
    if task.execution.is_some() || !task.execution_epochs.is_empty() {
        return Err("This report version requires an offline external phase plan".into());
    }
    if json {
        let slots = plan
            .slots
            .iter()
            .map(|slot| {
                let mut value = serde_json::to_value(slot).map_err(|e| e.to_string())?;
                let fields = value.as_object_mut().ok_or("Invalid external slot")?;
                fields.extend(BTreeMap::from([
                    ("status".into(), json!("unstarted")),
                    ("attempts".into(), json!([])),
                    ("receipt".into(), serde_json::Value::Null),
                    ("grade".into(), serde_json::Value::Null),
                    ("elapsed_ns".into(), serde_json::Value::Null),
                    ("inclusive_native_tokens".into(), serde_json::Value::Null),
                    ("charged_tokens".into(), serde_json::Value::Null),
                ]));
                Ok::<_, String>(value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        println!("{}", serde_json::to_string_pretty(&json!({
            "schema": REPORT_SCHEMA, "task_id": task.task_id, "plan_id": task.revision.authority.policy_id,
            "phase": plan.phase, "pins": plan.pins, "common_instruction": plan.common_instruction,
            "prerequisite": prerequisite(&plan), "predecessor": plan.predecessor,
            "execution": "unavailable", "slots": slots
        })).map_err(|e| e.to_string())?);
    } else {
        println!(
            "Task {} · external {:?} · prerequisite {} · execution unavailable",
            task.task_id,
            plan.phase,
            prerequisite(&plan)
        );
        for slot in plan.slots {
            println!(
                "{} {} {} {} {:?} unstarted",
                slot.index, slot.pair_id, slot.case_id, slot.repetition, slot.variant
            );
        }
    }
    Ok(0)
}
