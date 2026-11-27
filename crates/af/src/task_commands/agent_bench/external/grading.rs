//! Blind grading of retained answers from a finished external smoke Task.
use super::execution::{self, Hooks, SendState};
use super::*;
use af_contracts::task::TASK_REVISION_V1;
use af_contracts::task::execution::{TaskAttemptResultV1, TaskInvocationV1, TaskOutputV1};
use af_contracts::task::usage::TaskUsageObservationV1;
use af_graph::{RunReport, task::CompiledNode};
use af_runner_codex::navigation as codex;
use af_runtime::task::source::{invocation_producer, source_snapshot};
use af_runtime::task::{TaskOperatorHost, TaskWorkOutput};
use af_store::store::task::execution::{PreparedTaskAttempt, ReservedTaskAttempt};
use af_store::store::{TaskAttemptWall, task::task_run_id};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Mutex, atomic::AtomicBool};
use std::time::{Duration, Instant};

pub(in crate::task_commands) const KIND: &str = "agent-bench-grading";
const POLICY: &str = "af/AgentBenchGradingPlan@2";
const STAGE: &str = "af/AgentBenchGradingStage@1";
const GRADE: &str = "af/AgentBenchGradingReceipt@3";
pub(super) const OBLIGATION: &str = "resolved";
const PROFILE: codex::WorkerProfile = codex::WorkerProfile::Grader;
const GRADERS: usize = 3;
pub(super) const OVERHEAD_TOKEN_LIMIT: u64 = 4_000_000;
pub(super) const OVERHEAD_WALL_MS: u64 = 440 * 60 * 1000;
const INSTRUCTION: &str = "Evaluate semantic equivalence, the required facts, abstention, citations, disqualifiers and unresolved ambiguity under the rubric. Equivalent resolving source evidence is allowed. Return one JSON object with verdict (correct, incorrect, disqualified or unresolved) and rationale (a bounded nonempty string). Do not quote the private rubric or oracle.";

#[derive(Debug, Args)]
pub(crate) struct GradingPlan {
    #[command(flatten)]
    subject: EvaluatorCheck,
    /// Exact native Linux Codex executable for all fresh grader sessions.
    #[arg(
        long,
        requires = "fixture_endpoint",
        conflicts_with = "grading_transport"
    )]
    native_fixture_codex: Option<PathBuf>,
    /// Inert Responses endpoint for credential-free grading.
    #[arg(
        long,
        requires = "native_fixture_codex",
        conflicts_with = "grading_transport"
    )]
    fixture_endpoint: Option<SocketAddrV4>,
    /// JSON declaration for official Codex Personal grading.
    #[arg(long, requires = "grading_transport_sha256")]
    grading_transport: Option<PathBuf>,
    /// SHA-256 of the exact grading transport declaration.
    #[arg(long, requires = "grading_transport")]
    grading_transport_sha256: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonalInput {
    executable: PathBuf,
    account_binding: String,
    auth_homes: Vec<PathBuf>,
    catalog: PathBuf,
    catalog_sha256: String,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum GradingBinding {
    LoopbackFixture {
        executable: PathBuf,
        endpoint: SocketAddrV4,
    },
    CodexPersonal {
        executable: PathBuf,
        account_binding: String,
        auth_homes: Vec<PathBuf>,
        catalog: InputPin,
    },
}

impl GradingBinding {
    fn live(&self) -> bool {
        matches!(self, Self::CodexPersonal { .. })
    }

    fn mode(&self) -> &'static str {
        if self.live() {
            "codex_personal"
        } else {
            "loopback_fixture"
        }
    }

    fn destination(&self) -> &'static str {
        if self.live() {
            execution::LIVE_DESTINATION
        } else {
            execution::FIXTURE_DESTINATION
        }
    }

    fn executable(&self) -> &Path {
        match self {
            Self::LoopbackFixture { executable, .. } | Self::CodexPersonal { executable, .. } => {
                executable
            }
        }
    }

    fn account(&self) -> &str {
        match self {
            Self::CodexPersonal {
                account_binding, ..
            } => account_binding,
            Self::LoopbackFixture { .. } => codex::FIXTURE_ACCOUNT,
        }
    }

    fn catalog_digest(&self) -> String {
        match self {
            Self::LoopbackFixture { .. } => codex::sha256(&PROFILE.fixture_catalog()),
            Self::CodexPersonal { catalog, .. } => catalog.sha256.clone(),
        }
    }

    fn catalog(&self, cas: &Cas) -> Result<Vec<u8>, String> {
        let bytes = match self {
            Self::LoopbackFixture { .. } => PROFILE.fixture_catalog(),
            Self::CodexPersonal { catalog, .. } => cas
                .get_bounded(&catalog.object_id, INPUT_LIMIT)
                .map_err(|e| e.to_string())?,
        };
        if codex::sha256(&bytes) != self.catalog_digest() {
            return Err("Grader model catalog differs from its captured bytes".into());
        }
        codex::validate_worker_catalog(&bytes, PROFILE)?;
        Ok(bytes)
    }

    fn validate(&self, count: usize) -> Result<(), String> {
        let valid = match self {
            Self::LoopbackFixture { endpoint, .. } => {
                *endpoint.ip() == Ipv4Addr::LOCALHOST && endpoint.port() != 0
            }
            Self::CodexPersonal {
                account_binding,
                auth_homes,
                ..
            } => {
                af_contracts::is_digest(account_binding)
                    && account_binding != codex::FIXTURE_ACCOUNT
                    && auth_homes.len() == count
                    && auth_homes.iter().all(|home| home.is_absolute())
                    && auth_homes.iter().collect::<BTreeSet<_>>().len() == count
            }
        };
        if !valid || !self.executable().is_absolute() {
            return Err("Grading requires a pinned native executable and a distinct home per live stage or IPv4 loopback".into());
        }
        Ok(())
    }

    fn validate_lineage(
        &self,
        cas: &Cas,
        store: &EventStore,
        plan: &ExternalPlan,
        subject: &execution::Configuration,
    ) -> Result<phase_evidence::Overhead, String> {
        let overhead = check_predecessor(cas, store, plan, Some(subject))?;
        let Self::CodexPersonal { auth_homes, .. } = self else {
            return Ok(overhead);
        };
        if auth_homes
            .iter()
            .any(|home| subject.auth_homes().contains(home))
        {
            return Err("Grading cannot use a subject's auth home".into());
        }
        if let Some(previous) = &plan.predecessor {
            let grade = store
                .task_projection(cas, &previous.task_id)
                .map_err(|e| e.to_string())?
                .ok_or("Missing predecessor grading Task")?;
            let prior = policy(cas, &grade)?.binding;
            let homes = phase_evidence::ancestral_homes(cas, store, previous)?;
            if !prior.live()
                || prior.account() != self.account()
                || prior.catalog_digest() != self.catalog_digest()
                || auth_homes.iter().any(|home| homes.contains(home))
            {
                return Err(
                    "Grading requires pinned pilot settings and fresh native authentication homes"
                        .into(),
                );
            }
        }
        Ok(overhead)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub(super) subject_task: String,
    subject_result: String,
    subject_revision: String,
    evaluator_sha256: String,
    source_digest: String,
    engine: String,
    transport: String,
    binding: GradingBinding,
    subject_tokens: Option<u64>,
    pub(super) prior_overhead_tokens: u64,
    pub(super) prior_overhead_ms: u64,
    cases: Vec<Case>,
}

/// This host-only association never enters a model message.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    association: String,
    receipt: Option<String>,
    case_id: String,
    family: String,
    subject_status: String,
    projected: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Correct,
    Incorrect,
    Disqualified,
    Unresolved,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Judgment {
    verdict: Verdict,
    rationale: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StageInput {
    index: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grade {
    index: usize,
    status: GradeStatus,
    judgment: Option<Judgment>,
    decision: Option<Verdict>,
    cumulative_tokens: Option<u64>,
    admission_stopped: bool,
    elapsed_ns: u64,
    calls: Vec<String>,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GradeStatus {
    Graded,
    Unavailable,
    NotRequired,
    Failed,
}

fn port_input(kind: &str, id: String) -> ArtifactInputV1 {
    ArtifactInputV1 {
        artifact_type: kind.into(),
        cardinality: PortCardinality::One,
        artifact_ids: vec![id],
        snapshot_id: None,
    }
}

impl Policy {
    fn compiler(&self, cas: &Cas, id: &str) -> Result<TaskPlanCompiler, String> {
        if self.engine != engine(cas)?
            || self.transport != PROFILE.config_identity()
            || ![Phase::Smoke.slots(), FULL_PHASE_SLOTS].contains(&self.cases.len())
        {
            return Err("Grading requires the captured engine, transport and phase slots".into());
        }
        self.binding.validate(self.cases.len() * GRADERS)?;
        self.binding.catalog(cas)?;
        compile_slots(
            cas,
            &self.engine,
            id,
            SlotPipeline {
                kind: KIND,
                slot_type: STAGE,
                receipt_type: GRADE,
                obligation: OBLIGATION,
                bindings_type: None,
                count: self.cases.len() * GRADERS,
                tokens: execution::INCLUSIVE_LIMIT,
                wall_ms: WALL_MS,
            },
        )
    }

    fn bindings(&self, prompt: &str, attempt: String) -> codex::Bindings {
        codex::Bindings {
            task_attempt_invocation: attempt,
            source: self.source_digest.clone(),
            prompt: PROFILE.prompt_identity(prompt),
            tools: ToolSet::Text.identity(),
            model_catalog: self.binding.catalog_digest(),
            executable: codex::LINUX_X86_64_SHA256.into(),
            account: self.binding.account().into(),
            protocol: codex::PROTOCOL.into(),
            config: self.transport.clone(),
        }
    }

    fn remaining_grader_allowance(&self, cumulative: Option<u64>) -> u64 {
        cumulative
            .and_then(|used| OVERHEAD_TOKEN_LIMIT.checked_sub(used))
            .unwrap_or(0)
            .min(execution::INCLUSIVE_LIMIT)
    }
}

fn project(
    question: &str,
    answer: &str,
    rubric: &str,
    oracle: &evaluator::EvaluatorCase,
) -> String {
    // Keep the actual answer and citations intact. The offline report flags that answer text
    // may itself reveal the treatment; an automatic rewrite could change the answer's meaning.
    json!({
        "instruction": INSTRUCTION,
        "question": question,
        "answer": answer,
        "rubric": rubric,
        "oracle": {
            "text": oracle.oracle_text_verbatim,
            "abstention": oracle.abstention,
        },
    })
    .to_string()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Projection {
    instruction: String,
    question: String,
    answer: String,
    rubric: String,
    oracle: OracleProjection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleProjection {
    text: String,
    abstention: serde_json::Value,
}

impl GradingPlan {
    fn capture_binding(
        &self,
        opened: &OpenedTask,
        repo: &Path,
        configuration: &execution::Configuration,
        count: usize,
    ) -> Result<GradingBinding, String> {
        let binding = match (
            self.grading_transport.as_deref(),
            self.grading_transport_sha256.as_deref(),
            self.native_fixture_codex.as_deref(),
            self.fixture_endpoint,
        ) {
            (Some(path), Some(pin), None, None) if configuration.live() => {
                let input: PersonalInput =
                    serde_json::from_slice(&read_bytes(path, pin, INPUT_LIMIT)?)
                        .map_err(|_| "Invalid Codex Personal grading declaration")?;
                for home in &input.auth_homes {
                    if configuration.auth_homes().contains(home) {
                        return Err("Grading cannot use a subject's auth home".into());
                    }
                    execution::auth_home(home, repo, &opened.state)?;
                }
                let catalog = read_bytes(&input.catalog, &input.catalog_sha256, INPUT_LIMIT)?;
                codex::validate_worker_catalog(&catalog, PROFILE)?;
                GradingBinding::CodexPersonal {
                    executable: input.executable,
                    account_binding: input.account_binding,
                    auth_homes: input.auth_homes,
                    catalog: InputPin {
                        sha256: input.catalog_sha256,
                        object_id: opened.cas.put(&catalog).map_err(|e| e.to_string())?,
                    },
                }
            }
            (None, None, Some(executable), Some(endpoint)) if !configuration.live() => {
                GradingBinding::LoopbackFixture {
                    executable: executable
                        .canonicalize()
                        .map_err(|_| "Grader executable unavailable")?,
                    endpoint,
                }
            }
            _ => return Err("Grading transport must match the retained subject's backend".into()),
        };
        binding.validate(count)?;
        let executable = binding.executable();
        if !std::fs::symlink_metadata(executable).is_ok_and(|meta| meta.is_file()) {
            return Err("Grading requires a regular native Codex executable".into());
        }
        read_bytes(executable, codex::LINUX_X86_64_SHA256, 512 * 1024 * 1024)?;
        Ok(binding)
    }
}

pub(in crate::task_commands) fn plan(options: GradingPlan) -> Result<i32, String> {
    let repo = options
        .subject
        .repo
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let opened = open_task(&options.subject.id, &repo, options.subject.state.as_deref())?;
    let plan = read_plan(&opened.cas, &opened.task)?;
    let configuration = execution::read_configuration(&opened.cas, &opened.task)?
        .ok_or("Grading requires a finished executable smoke Task")?;
    let TaskPhaseV1::Finished { result_id } = &opened.task.phase else {
        return Err("Grading requires a finished executable smoke Task".into());
    };
    let source = opened.task.revision.inputs["source"].clone();
    let snapshot = source.snapshot_id.clone();
    if configuration.live() != (plan.pins.source_revision == SOURCE_REVISION)
        || (configuration.live()
            && (snapshot.as_deref() != Some(FROZEN_SNAPSHOT)
                || !opened.task.admitted
                || opened.task.execution.is_none()))
    {
        return Err("Grading requires a finished subject Task in the same pilot lineage".into());
    }
    let binding =
        options.capture_binding(&opened, &repo, &configuration, plan.slots.len() * GRADERS)?;
    let prior = binding.validate_lineage(&opened.cas, &opened.store, &plan, &configuration)?;
    let evaluator = evaluator::load(&options.subject, &repo, &opened, &plan)?;
    let task_id = format!("{}-grading", opened.task.task_id);
    if let Some(existing) = opened
        .store
        .task_projection(&opened.cas, &task_id)
        .map_err(|e| e.to_string())?
    {
        let captured = policy(&opened.cas, &existing)?;
        if captured.subject_result != *result_id
            || captured.binding != binding
            || captured.evaluator_sha256 != options.subject.evaluator_sha256
        {
            return Err("Grading replan differs from the captured plan".into());
        }
        let plan_id = existing
            .plan_id
            .ok_or("Grading Task lacks a captured plan")?;
        println!("{}", json!({"task_id":task_id, "plan_id":plan_id}));
        return Ok(0);
    }
    let rows = execution::report_slots(
        &opened.cas,
        &opened.store,
        &opened.task,
        &plan,
        &configuration,
    )?;
    let prior = phase_evidence::before_grading(&opened.store, &opened.task, &rows, prior)?;
    let cas = Cas::open(opened.state.join("cas")).map_err(|e| e.to_string())?;
    let database = opened.state.join("events.sqlite");
    let mut store = EventStore::open(&database).map_err(|e| e.to_string())?;
    let result: TaskResultV1 = artifact(&opened.cas, result_id, TASK_RESULT_V1)?;
    let mut refs = vec![
        result_id.clone(),
        result.task_revision_id.clone(),
        opened.task.revision.authority.policy_id.clone(),
    ];
    if let GradingBinding::CodexPersonal { catalog, .. } = &binding {
        refs.push(catalog.object_id.clone());
    }
    let mut cases = Vec::new();
    let mut subject_tokens = Some(0_u64);
    for (slot, row) in plan.slots.iter().zip(&rows) {
        let status = row["status"]
            .as_str()
            .ok_or("Missing checked subject status")?;
        if status != "unstarted" {
            subject_tokens = subject_tokens
                .zip(row["inclusive_native_tokens"].as_u64())
                .and_then(|(total, count)| total.checked_add(count));
        }
        let receipt = row["receipt_id"].as_str().map(str::to_owned);
        refs.extend(receipt.iter().cloned());
        let oracle = evaluator
            .cases
            .iter()
            .find(|case| case.id == slot.case_id)
            .ok_or("Missing validated case oracle")?;
        let projected = if status == "collected" {
            let answer = row["receipt"]["answer"]
                .as_str()
                .ok_or("Missing retained answer")?;
            let prompt = project(
                &slot.question,
                answer,
                &evaluator.rubric_text_verbatim,
                oracle,
            );
            if prompt.len() > codex::MAX_PROMPT_BYTES {
                return Err("Projected grader context exceeds the native prompt bound".into());
            }
            Some(prompt)
        } else {
            None
        };
        cases.push(Case {
            association: format!("{:016x}{:016x}", opening_nonce(), opening_nonce()),
            receipt,
            case_id: slot.case_id.clone(),
            family: oracle.family.clone(),
            subject_status: status.into(),
            projected,
        });
    }
    let policy = Policy {
        subject_task: opened.task.task_id.clone(),
        subject_result: result_id.clone(),
        subject_revision: result.task_revision_id,
        evaluator_sha256: options.subject.evaluator_sha256,
        source_digest: plan.pins.source_digest,
        engine: engine(&cas)?,
        transport: PROFILE.config_identity(),
        binding,
        subject_tokens,
        prior_overhead_tokens: prior.tokens,
        prior_overhead_ms: prior.wall_ms,
        cases,
    };
    let policy_id = put(&cas, POLICY, &policy, refs, snapshot.clone())?;
    let compiler = policy.compiler(&cas, &policy_id)?;
    let destination = policy.binding.destination();
    let count = policy.cases.len() * GRADERS;
    let attempts = count as u32;
    let tokens = u64::from(attempts) * execution::INCLUSIVE_LIMIT;
    let wall_ms = u64::from(attempts) * WALL_MS;
    let mut inputs = BTreeMap::from([("source".into(), source)]);
    let mut required_outputs = BTreeMap::new();
    for index in 0..count {
        let name = node_name(index);
        let id = put(
            &cas,
            STAGE,
            &StageInput { index },
            vec![policy_id.clone()],
            None,
        )?;
        inputs.insert(name.clone(), port_input(STAGE, id));
        required_outputs.insert(
            name,
            RequiredOutputV1 {
                artifact_type: GRADE.into(),
                cardinality: PortCardinality::One,
            },
        );
    }
    let revision = TaskRevisionV1 {
        task_id: task_id.clone(),
        revision: 1,
        previous_revision_id: None,
        kind: KIND.into(),
        goal: "Independently grade retained external answers using fresh native sessions.".into(),
        inputs,
        required_outputs,
        acceptance: BTreeMap::from([(
            OBLIGATION.into(),
            AcceptanceObligationV1 {
                evidence_type: GRADE.into(),
                verifier_policy: policy_id.clone(),
            },
        )]),
        provenance: TaskProvenanceV1 {
            adapter_id: policy_id.clone(),
            input_artifact_ids: vec![policy_id.clone()],
        },
        authority: TaskAuthorityV1 {
            policy_id,
            allowed_effects: BTreeSet::from(["read-source".into(), "execute-checks".into()]),
            data_destinations: BTreeSet::from([destination.into()]),
        },
        limits: TaskLimitsV1 {
            tokens,
            max_attempts: attempts,
            deadline_unix_ms: clock()?
                .checked_add(wall_ms + WALL_MS)
                .ok_or("Grading deadline overflow")?,
            verification: VerificationReserveV1 {
                tokens,
                attempts,
                wall_ms,
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
    let (execution, graph) = compiler.compile(&cas, &revision_id, PIPELINE)?;
    let plan_id = put(
        &cas,
        EXECUTION_PLAN_V4,
        &execution,
        vec![revision_id.clone()],
        None,
    )?;
    let domain = Domain {
        policy,
        graph,
        database,
        repo,
    };
    let writer = format!("cli-{}-{:016x}", writer_pid(), opening_nonce());
    let lease = store
        .open_task(&cas, &revision_id, &writer, recovery::lease_ms())
        .map_err(|e| e.to_string())?;
    let authority = CapturedTaskAuthority::new(&compiler, &domain, &NoTaskDeveloper);
    let proposed = store
        .propose_task_plan(&cas, &lease, &plan_id, &authority)
        .map(|_| ())
        .map_err(|e| e.to_string());
    release(&cas, &mut store, &lease, proposed)?;
    println!("{}", json!({"task_id":task_id, "plan_id": plan_id}));
    Ok(0)
}

struct Domain {
    policy: Policy,
    graph: CompiledTask,
    database: PathBuf,
    repo: PathBuf,
}

#[derive(Clone, Copy)]
struct Stage {
    index: usize,
    prior: Option<u64>,
    admission_stopped: bool,
    status: GradeStatus,
    votes: [Option<Verdict>; 2],
}

impl Domain {
    fn stage(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<Stage, String> {
        let StageInput { index } = artifact(cas, &input.inputs["slot"].artifact_ids[0], STAGE)?;
        if index >= self.policy.cases.len() * GRADERS
            || !input.node.ends_with(&format!(".{}", node_name(index)))
        {
            return Err("Invocation differs from its grading stage".into());
        }
        let previous: Option<Grade> = input
            .inputs
            .get("previous")
            .map(|prev| artifact(cas, &prev.artifact_ids[0], GRADE))
            .transpose()?;
        if previous.as_ref().map(|grade| grade.index) != index.checked_sub(1) {
            return Err("Grading stage has the wrong predecessor".into());
        }
        let prior = previous
            .as_ref()
            .map_or(Some(self.policy.prior_overhead_tokens), |grade| {
                grade.cumulative_tokens
            });
        let admission_stopped = previous
            .as_ref()
            .is_some_and(|grade| grade.admission_stopped);
        let mut votes = [None; 2];
        let deciding = index % GRADERS == 2;
        if deciding {
            let previous = previous.ok_or("Missing second independent grade")?;
            let envelope = cas
                .get_artifact(&input.inputs["previous"].artifact_ids[0])
                .map_err(|e| e.to_string())?;
            let first_id = envelope
                .input_artifacts
                .iter()
                .find(|id| {
                    cas.get_optional_artifact(id)
                        .is_ok_and(|entry| entry.is_some_and(|entry| entry.artifact_type == GRADE))
                })
                .ok_or("Missing first independent grade link")?;
            let first: Grade = artifact(cas, first_id, GRADE)?;
            if first.index != index - 2 {
                return Err("Adjudication has the wrong first independent grade".into());
            }
            votes = [verdict(&first), verdict(&previous)];
        }
        let status = if admission_stopped || self.policy.cases[index / GRADERS].projected.is_none()
        {
            GradeStatus::Unavailable
        } else if deciding && !matches!(votes, [Some(a), Some(b)] if a != b) {
            GradeStatus::NotRequired
        } else if self.policy.remaining_grader_allowance(prior) == 0 {
            GradeStatus::Unavailable
        } else {
            GradeStatus::Graded
        };
        Ok(Stage {
            index,
            prior,
            admission_stopped,
            status,
            votes,
        })
    }

    fn context(&self, cas: &Cas, input: &TaskInvocationV1) -> Result<serde_json::Value, String> {
        let stage = self.stage(cas, input)?;
        let prompt = self.policy.cases[stage.index / GRADERS]
            .projected
            .as_deref();
        Ok(json!({
            "invocation": input,
            "prompt": prompt.filter(|_| stage.status == GradeStatus::Graded).map(|prompt| PROFILE.prompt_identity(prompt)),
            "transport": self.policy.transport,
            "source": self.policy.source_digest,
        }))
    }

    fn invoke(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &PreparedTaskAttempt,
        cancellation: Option<&AtomicBool>,
        observed: &mut SendState,
    ) -> Result<ArtifactInputV1, String> {
        let Stage {
            index,
            prior,
            admission_stopped,
            status,
            votes,
        } = self.stage(cas, input)?;
        let (snapshot, _, manifest) = source_snapshot(cas, &input.inputs["source"])?;
        let start = Instant::now();
        let mut grade = Grade {
            index,
            status,
            judgment: None,
            decision: None,
            cumulative_tokens: prior,
            admission_stopped,
            elapsed_ns: 0,
            calls: vec![],
        };
        if status == GradeStatus::Graded {
            let cap = self.policy.remaining_grader_allowance(prior);
            let started_unix_ms = clock()?;
            let store = EventStore::open_read_only(&self.database).map_err(|e| e.to_string())?;
            let task = store
                .task_projection(cas, attempt.task_id())
                .map_err(|e| e.to_string())?
                .ok_or("Grading has no admitted Task")?;
            let budget_deadline = task
                .clock_started_unix_ms()
                .ok_or("Grading lacks an admitted clock")?
                .checked_add(OVERHEAD_WALL_MS.saturating_sub(self.policy.prior_overhead_ms))
                .ok_or("Grading overhead deadline overflow")?;
            let deadline = start
                + Duration::from_millis(
                    attempt
                        .reservation()
                        .deadline_unix_ms
                        .min(budget_deadline)
                        .saturating_sub(started_unix_ms),
                );
            if deadline <= Instant::now() {
                return Err("Pilot grading wall allowance exhausted".into());
            }
            if attempt.reservation().tokens != execution::INCLUSIVE_LIMIT {
                return Err("Grader lacks the pinned native allowance".into());
            }
            let broker =
                navigation::Broker::from_manifest(&manifest, cas).map_err(|e| e.to_string())?;
            if broker.source_identity() != self.policy.source_digest {
                return Err("Grader source differs from the frozen subject source".into());
            }
            let mut trace = Vec::new();
            let mut tools = subject::Tools {
                arm: Arm::Baseline,
                broker,
                deadline,
                cancellation,
                trace: &mut trace,
            };
            let host = native_fixture::Host {
                tools: Mutex::new(&mut tools),
                analyzer: None,
            };
            let prompt = self.policy.cases[index / GRADERS]
                .projected
                .as_deref()
                .ok_or("Missing blind projection")?;
            let bindings = self.policy.bindings(
                prompt,
                execution::attempt_binding(attempt.task_id(), attempt.id(), attempt.context_id()),
            );
            let hooks = Hooks {
                cas,
                database: &self.database,
                attempt,
                bindings: &bindings,
                cancelled: cancellation,
                live: self.policy.binding.live(),
                cap,
                start,
                state: Mutex::new(SendState::default()),
                wall: TaskAttemptWall {
                    run_id: task_run_id(attempt.task_id()).map_err(|e| e.to_string())?,
                    attempt_id: attempt.id().into(),
                    node_id: attempt.node().into(),
                    round: 0,
                    epoch: u32::try_from(attempt.writer_epoch()).map_err(|e| e.to_string())?,
                    started_unix_ms,
                    elapsed_ms: 0,
                    usage: None,
                },
            };
            let mut request = codex::Request {
                profile: PROFILE,
                pin: codex::Pin {
                    executable: self.policy.binding.executable().to_owned(),
                    executable_sha256: codex::LINUX_X86_64_SHA256.into(),
                    catalog: self.policy.binding.catalog(cas)?,
                    catalog_sha256: bindings.model_catalog.clone(),
                },
                native_auth_home: PathBuf::new(),
                bindings: bindings.clone(),
                question: prompt.into(),
                tools: ToolSet::Text,
                deadline,
                max_inclusive_tokens: cap,
                max_sends: native_fixture::MAX_SENDS,
            };
            let outcome = match &self.policy.binding {
                GradingBinding::CodexPersonal { auth_homes, .. } => {
                    let home = auth_homes
                        .get(index)
                        .ok_or("Missing dedicated grader home")?;
                    execution::auth_home(
                        home,
                        &self.repo,
                        self.database.parent().ok_or("Missing Store directory")?,
                    )?;
                    request.native_auth_home = home.clone();
                    Ok(codex::run(request, &host, &hooks))
                }
                GradingBinding::LoopbackFixture { endpoint, .. } => {
                    codex::run_fixture(request, *endpoint, &host, &hooks)
                }
            };
            *observed = hooks.state.into_inner().unwrap();
            observed.usage.missing |= observed.pending.is_some();
            let used = observed.usage.complete();
            if observed.ordinal != 0 {
                grade.cumulative_tokens = prior
                    .zip(used)
                    .and_then(|(base, used)| base.checked_add(used));
                grade.admission_stopped = usage_stops_admission(&observed.usage, cap);
            }
            grade.calls.clone_from(&observed.ids);
            grade.elapsed_ns =
                u64::try_from(start.elapsed().as_nanos()).map_err(|e| e.to_string())?;
            grade.judgment = outcome
                .ok()
                .filter(|result| {
                    result.cleanup_confirmed
                        && result.failure.is_none()
                        && used.is_some()
                        && !grade.admission_stopped
                })
                .and_then(|result| result.answer)
                .and_then(|answer| serde_json::from_str::<Judgment>(&answer).ok())
                .filter(|judgment| nonempty(&judgment.rationale, 8192));
            if grade.judgment.is_none() {
                grade.status = GradeStatus::Failed;
            }
        }
        grade.decision = decision(votes[0], votes[1], verdict(&grade));
        let mut refs = subject::input_refs(input);
        refs.extend(grade.calls.iter().cloned());
        refs.push(attempt.context_id().to_owned());
        refs.sort();
        refs.dedup();
        let (id, _) = cas
            .put_artifact(
                GRADE,
                invocation_producer(cas, input, Some(attempt))?,
                refs,
                Some(snapshot.clone()),
                serde_json::to_value(grade).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        Ok(ArtifactInputV1 {
            snapshot_id: Some(snapshot),
            ..port_input(GRADE, id)
        })
    }
}

fn verdict(grade: &Grade) -> Option<Verdict> {
    grade.judgment.as_ref().map(|judgment| judgment.verdict)
}

fn usage_stops_admission(usage: &execution::Usage, cap: u64) -> bool {
    usage.complete().is_none_or(|used| used > cap)
}

fn decision(
    first: Option<Verdict>,
    second: Option<Verdict>,
    adjudication: Option<Verdict>,
) -> Option<Verdict> {
    match (first, second) {
        (Some(a), Some(b)) if a == b => Some(a),
        (Some(a), Some(b)) => Some(
            adjudication
                .filter(|third| *third == a || *third == b)
                .unwrap_or(Verdict::Unresolved),
        ),
        _ => None,
    }
}

impl TaskOperatorHost for Domain {
    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _: &CompiledNode,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.stage(cas, input)?;
        let store = EventStore::open_read_only(&self.database).map_err(|e| e.to_string())?;
        let projection = store
            .task_projection(cas, attempt.task_id())
            .map_err(|e| e.to_string())?
            .ok_or("Missing grading Task")?;
        if projection.execution.as_ref().is_none_or(|run| {
            run.attempt_accounting()
                .iter()
                .any(|row| row.started && row.reservation.node == input.node)
        }) {
            return Err("Grading stage is single-use".into());
        }
        cas.put_json(&self.context(cas, input)?)
            .map_err(|e| e.to_string())
    }

    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _: &CompiledNode,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&AtomicBool>,
    ) -> TaskWorkOutput {
        let mut observed = SendState::default();
        let outputs = attempt
            .ok_or("Grader requires a native Attempt".into())
            .and_then(|attempt| self.invoke(cas, input, attempt, cancellation, &mut observed))
            .map(|receipt| BTreeMap::from([("receipt".into(), receipt)]));
        let live = self.policy.binding.live();
        let sent = observed.ordinal != 0;
        let charge_complete = observed.usage.complete().is_some();
        TaskWorkOutput {
            outputs,
            notes: Default::default(),
            usage_observation: (live && sent).then(|| TaskUsageObservationV1 {
                charge_complete,
                reported_usage: observed.usage.counters.clone(),
            }),
            charged_tokens: (!live || !sent).then_some(0),
            usage: observed.usage.counters,
            raw_artifact_ids: observed.ids,
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
        if cas.get_json(id).map_err(|e| e.to_string())? != self.context(cas, input)? {
            return Err("Grading context differs from the captured invocation".into());
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
        let stage = self.stage(cas, input)?;
        let id = &output.outputs["receipt"].artifact_ids[0];
        let grade: Grade = artifact(cas, id, GRADE)?;
        let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
        let Producer::Attempt { attempt_id, .. } = envelope.producer else {
            return Err("Missing grader Attempt producer".into());
        };
        let context =
            af_store::content_id(&self.context(cas, input)?).map_err(|e| e.to_string())?;
        self.check_grade(
            cas,
            &task.task_id,
            &attempt_id,
            input,
            &context,
            &grade,
            stage,
            &envelope.input_artifacts,
        )
    }

    fn assemble_result(
        &self,
        cas: &Cas,
        task: &TaskProjection,
        report: &RunReport,
    ) -> Result<TaskResultV1, String> {
        let mut result = subject::collection_result(&self.graph, task, report)?;
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
            return Err("Grading result differs from the captured backend conclusion".into());
        }
        Ok(())
    }
}

impl Domain {
    fn assess(&self, cas: &Cas, result: &mut TaskResultV1) -> Result<(), String> {
        let mut resolved = 0;
        let mut known_budget_stop = false;
        for port in result.outputs.values() {
            let grade: Grade = artifact(cas, &port.artifact_ids[0], GRADE)?;
            if grade.index % GRADERS == GRADERS - 1
                && matches!(
                    grade.decision,
                    Some(Verdict::Correct | Verdict::Incorrect | Verdict::Disqualified)
                )
            {
                resolved += 1;
            }
            known_budget_stop |= grade
                .cumulative_tokens
                .is_some_and(|tokens| grade.admission_stopped || tokens >= OVERHEAD_TOKEN_LIMIT);
        }
        let complete = result.execution == TaskExecutionV1::Completed
            && result.outputs.len() == self.policy.cases.len() * GRADERS
            && resolved == self.policy.cases.len();
        result.missing_obligations.clear();
        result.acceptance = if complete {
            TaskAcceptanceV1::Satisfied
        } else {
            result.missing_obligations.insert(OBLIGATION.into());
            if known_budget_stop {
                result.execution = TaskExecutionV1::Exhausted;
            } else if result.execution == TaskExecutionV1::Completed {
                result.execution = TaskExecutionV1::Incomplete;
            }
            TaskAcceptanceV1::Inconclusive
        };
        result.domain_conclusion = match (self.policy.binding.live(), complete) {
            (true, true) => "external_live_grading_resolved",
            (false, true) => "external_fixture_grading_resolved",
            (true, false) => "external_live_grading_incomplete",
            (false, false) => "external_fixture_grading_incomplete",
        }
        .into();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn check_grade(
        &self,
        cas: &Cas,
        task: &str,
        attempt: &str,
        input: &TaskInvocationV1,
        context: &str,
        grade: &Grade,
        stage: Stage,
        references: &[String],
    ) -> Result<(), String> {
        let Stage {
            index,
            prior,
            admission_stopped,
            status: expected,
            votes,
        } = stage;
        let mut refs = subject::input_refs(input);
        refs.push(context.into());
        refs.extend(grade.calls.iter().cloned());
        refs.sort();
        refs.dedup();
        if grade.index != index
            || refs != references
            || grade.decision != decision(votes[0], votes[1], verdict(grade))
            || (grade.status != expected
                && !(expected == GradeStatus::Graded && grade.status == GradeStatus::Failed))
            || grade.judgment.is_some() != (grade.status == GradeStatus::Graded)
            || grade
                .judgment
                .as_ref()
                .is_some_and(|g| !nonempty(&g.rationale, 8192))
            || (expected != GradeStatus::Graded
                && (!grade.calls.is_empty() || grade.elapsed_ns != 0))
        {
            return Err("Grade differs from the captured grading stage".into());
        }
        let (cumulative, stopped) = if grade.calls.is_empty() {
            if grade.status == GradeStatus::Graded {
                return Err("Grade has no native sends".into());
            }
            (prior, admission_stopped)
        } else {
            let prompt = self.policy.cases[index / GRADERS]
                .projected
                .as_deref()
                .ok_or("Missing blind projection")?;
            let bindings = self
                .policy
                .bindings(prompt, execution::attempt_binding(task, attempt, context));
            let (calls, usage) = execution::replay_calls(
                cas,
                grade
                    .calls
                    .chunks(2)
                    .map(|ids| (ids[0].clone(), ids.get(1).cloned())),
                &bindings,
                self.policy.binding.live(),
            )?;
            let stopped =
                usage_stops_admission(&usage, self.policy.remaining_grader_allowance(prior));
            if grade.status == GradeStatus::Graded
                && (stopped || calls.iter().any(|call| !call["raw"]["failure"].is_null()))
            {
                return Err("Grade has incomplete or overshooting usage".into());
            }
            (
                prior
                    .zip(usage.complete())
                    .and_then(|(a, b)| a.checked_add(b)),
                stopped,
            )
        };
        if cumulative != grade.cumulative_tokens || stopped != grade.admission_stopped {
            return Err("Grading usage differs from its durable native sends".into());
        }
        Ok(())
    }
}

pub(super) fn policy(cas: &Cas, task: &TaskProjection) -> Result<Policy, String> {
    let id = &task.revision.authority.policy_id;
    let policy: Policy = artifact(cas, id, POLICY)?;
    let result: TaskResultV1 = artifact(cas, &policy.subject_result, TASK_RESULT_V1)?;
    let subject: TaskRevisionV1 = artifact(cas, &policy.subject_revision, TASK_REVISION_V1)?;
    let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
    let source = task
        .revision
        .inputs
        .get("source")
        .ok_or("Grading Task has no captured source")?;
    let destination = BTreeSet::from([policy.binding.destination().into()]);
    if task.revision.kind != KIND
        || task.revision.required_outputs.len() != policy.cases.len() * GRADERS
        || task
            .revision
            .required_outputs
            .values()
            .any(|output| output.artifact_type != GRADE)
        || task.revision.authority.data_destinations != destination
        || subject.authority.data_destinations != destination
        || subject.kind != super::KIND
        || subject.task_id != policy.subject_task
        || result.task_revision_id != policy.subject_revision
        || subject.inputs.get("source") != Some(source)
        || envelope.subject_snapshot_id != source.snapshot_id
        || ![Phase::Smoke.slots(), FULL_PHASE_SLOTS].contains(&policy.cases.len())
        || !af_contracts::is_digest(&policy.evaluator_sha256)
        || (policy.binding.live()
            && (source.snapshot_id.as_deref() != Some(FROZEN_SNAPSHOT)
                || policy.evaluator_sha256 != evaluator::FROZEN_EVALUATOR_SHA256))
        || !envelope.input_artifacts.contains(&policy.subject_result)
        || policy
            .cases
            .iter()
            .filter_map(|case| case.receipt.as_ref())
            .any(|id| !envelope.input_artifacts.contains(id))
    {
        return Err("Grading plan differs from its retained subject authority".into());
    }
    policy.binding.validate(policy.cases.len() * GRADERS)?;
    policy.binding.catalog(cas)?;
    Ok(policy)
}

pub(super) fn used_homes(cas: &Cas, task: &TaskProjection) -> Result<Vec<PathBuf>, String> {
    Ok(match policy(cas, task)?.binding {
        GradingBinding::CodexPersonal { auth_homes, .. } => auth_homes,
        GradingBinding::LoopbackFixture { .. } => vec![],
    })
}

fn checked_subject(
    cas: &Cas,
    store: &EventStore,
    policy: &Policy,
) -> Result<(TaskProjection, ExternalPlan, Vec<serde_json::Value>), String> {
    let task = store
        .task_projection(cas, &policy.subject_task)
        .map_err(|e| e.to_string())?
        .ok_or("Retained subject Task is missing")?;
    let plan = read_plan(cas, &task)?;
    let configuration =
        execution::read_configuration(cas, &task)?.ok_or("Missing retained subject bindings")?;
    if task.revision_id != policy.subject_revision
        || !matches!(&task.phase, TaskPhaseV1::Finished { result_id } if result_id == &policy.subject_result)
        || configuration.live() != policy.binding.live()
        || plan.slots.len() != policy.cases.len()
        || plan.pins.source_digest != policy.source_digest
    {
        return Err("Grading requires the exact finished subject and matching backend".into());
    }
    let prior = policy
        .binding
        .validate_lineage(cas, store, &plan, &configuration)?;
    if policy.binding.live() && (!task.admitted || task.execution.is_none()) {
        return Err("Grading requires a confirmed subject".into());
    }
    let rows = execution::report_slots(cas, store, &task, &plan, &configuration)?;
    let prior = phase_evidence::before_grading(store, &task, &rows, prior)?;
    if prior.tokens != policy.prior_overhead_tokens || prior.wall_ms != policy.prior_overhead_ms {
        return Err("Grading overhead differs from its checked pilot lineage".into());
    }
    let mut subject_tokens = Some(0_u64);
    for ((slot, case), row) in plan.slots.iter().zip(&policy.cases).zip(&rows) {
        let status = row["status"]
            .as_str()
            .ok_or("Missing checked subject status")?;
        if status != "unstarted" {
            subject_tokens = subject_tokens
                .zip(row["inclusive_native_tokens"].as_u64())
                .and_then(|(total, count)| total.checked_add(count));
        }
        if case.case_id != slot.case_id
            || case.subject_status != status
            || case.receipt.as_deref() != row["receipt_id"].as_str()
            || case.projected.is_some() != (status == "collected")
        {
            return Err("Grading changed its selected subject answer".into());
        }
        if let Some(prompt) = &case.projected {
            let prompt: Projection =
                serde_json::from_str(prompt).map_err(|_| "Invalid retained grading projection")?;
            if prompt.question != slot.question
                || Some(prompt.answer.as_str()) != row["receipt"]["answer"].as_str()
                || prompt.instruction != INSTRUCTION
                || !nonempty(&prompt.rubric, INPUT_LIMIT as usize)
                || !nonempty(&prompt.oracle.text, INPUT_LIMIT as usize)
                || !(prompt.oracle.abstention.is_boolean() || prompt.oracle.abstention.is_string())
            {
                return Err(
                    "Grading projection differs from the retained question or answer".into(),
                );
            }
        }
    }
    if policy.subject_tokens != subject_tokens {
        return Err("Grading changed its retained subject accounting".into());
    }
    Ok((task, plan, rows))
}

pub(in crate::task_commands) fn restore(
    cas: &Cas,
    task: &TaskProjection,
    graph: CompiledTask,
    state: &Path,
    repo: PathBuf,
) -> Result<(TaskPlanCompiler, Box<dyn TaskDomain>), String> {
    let policy = policy(cas, task)?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    checked_subject(cas, &store, &policy)?;
    let compiler = policy.compiler(cas, &task.revision.authority.policy_id)?;
    Ok((
        compiler,
        Box::new(Domain {
            policy,
            graph,
            database: state.join("events.sqlite"),
            repo,
        }),
    ))
}

pub(in crate::task_commands::agent_bench) fn report(
    cas: &Cas,
    store: &EventStore,
    task: &TaskProjection,
    as_json: bool,
) -> Result<i32, String> {
    let value = replay(cas, store, task)?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        );
    } else {
        let slots = value["slots"].as_array().ok_or("Missing grading slots")?;
        println!(
            "Task {} · {} external grading · {} subject slots",
            task.task_id,
            value["mode"].as_str().unwrap_or_default(),
            slots.len()
        );
        for slot in slots {
            println!(
                "Slot {} · {}",
                slot["slot"],
                slot["decision"].as_str().unwrap_or("unavailable")
            );
        }
    }
    Ok(0)
}

pub(super) fn replay(
    cas: &Cas,
    store: &EventStore,
    task: &TaskProjection,
) -> Result<serde_json::Value, String> {
    let policy = policy(cas, task)?;
    let (subject, subject_plan, subject_rows) = checked_subject(cas, store, &policy)?;
    let run_id = task_run_id(&task.task_id).map_err(|e| e.to_string())?;
    let mut slots = Vec::new();
    let mut grade_tokens = matches!(task.phase, TaskPhaseV1::Finished { .. }).then_some(0_u64);
    let mut known_grade_tokens = 0_u64;
    let mut cumulative = Some(policy.prior_overhead_tokens);
    let mut admission_stopped = false;
    let attempts = task
        .execution
        .as_ref()
        .map(|run| run.attempt_accounting())
        .unwrap_or_default();
    for (slot, case) in policy.cases.iter().enumerate() {
        let mut stages = Vec::new();
        let mut votes = [None; GRADERS];
        let mut sealed_decision = None;
        let mut decision_receipt = None;
        for (role, vote) in votes.iter_mut().enumerate() {
            let stage = node_name(slot * GRADERS + role);
            let mut row = json!({"status": "unstarted"});
            if let Some(run) = &task.execution {
                let node = &run
                    .graph
                    .outputs
                    .get(&stage)
                    .ok_or("Missing grading output binding")?
                    .node;
                let mut started = attempts
                    .iter()
                    .filter(|attempt| attempt.started && &attempt.reservation.node == node);
                if let Some(attempt) = started.next() {
                    if started.next().is_some() {
                        return Err("Report found multiple starts for one grading stage".into());
                    }
                    let retained = store
                        .task_attempt_sends(&run_id, &attempt.attempt_id)
                        .map_err(|e| e.to_string())?;
                    let (calls, usage) = if retained.is_empty() {
                        (vec![], execution::Usage::default())
                    } else {
                        let prompt = case
                            .projected
                            .as_deref()
                            .ok_or("Native send has no blind projection")?;
                        let context = attempt
                            .context_id
                            .as_deref()
                            .ok_or("Native send has no bound grader context")?;
                        let binding = policy.bindings(
                            prompt,
                            execution::attempt_binding(&task.task_id, &attempt.attempt_id, context),
                        );
                        execution::replay_calls(
                            cas,
                            retained.into_iter(),
                            &binding,
                            policy.binding.live(),
                        )?
                    };
                    execution::require_settlement_links(attempt, &calls)?;
                    if !calls.is_empty() {
                        let cap = policy.remaining_grader_allowance(cumulative);
                        if admission_stopped || cap == 0 {
                            return Err("Retained grading send exceeds batch admission".into());
                        }
                        admission_stopped = usage_stops_admission(&usage, cap);
                        cumulative = cumulative
                            .zip(usage.complete())
                            .and_then(|(a, b)| a.checked_add(b));
                        known_grade_tokens = known_grade_tokens
                            .checked_add(usage.inclusive)
                            .ok_or("Known grading usage overflow")?;
                        grade_tokens = grade_tokens
                            .zip(attempt.result.as_ref().and(usage.complete()))
                            .and_then(|(a, b)| a.checked_add(b));
                    }
                    let mut status = if attempt.result.is_some() {
                        "failed"
                    } else {
                        "missing"
                    };
                    let mut judgment = None;
                    let mut receipt = None;
                    let mut elapsed_ns = None;
                    if let Some(TaskAttemptResultV1::Succeeded { output_id, .. }) = &attempt.result
                    {
                        let output: TaskOutputV1 = artifact(
                            cas,
                            output_id,
                            af_contracts::task::execution::TASK_OUTPUT_V1,
                        )?;
                        let id = &output.outputs["receipt"].artifact_ids[0];
                        let grade: Grade = artifact(cas, id, GRADE)?;
                        let envelope = cas.get_artifact(id).map_err(|e| e.to_string())?;
                        if envelope.producer
                            != (Producer::Attempt {
                                run_id: run_id.clone(),
                                node_id: node.clone(),
                                attempt_id: attempt.attempt_id.clone(),
                            })
                            || grade.index != slot * GRADERS + role
                            || grade.cumulative_tokens != cumulative
                            || grade.admission_stopped != admission_stopped
                            || grade
                                .calls
                                .iter()
                                .map(String::as_str)
                                .ne(calls.iter().flat_map(|call| {
                                    ["intent_artifact_id", "raw_artifact_id"]
                                        .into_iter()
                                        .filter_map(|field| call[field].as_str())
                                }))
                        {
                            return Err("Grade receipt differs from its settled Attempt".into());
                        }
                        status = match grade.status {
                            GradeStatus::Graded => "graded",
                            GradeStatus::Unavailable => "unavailable",
                            GradeStatus::NotRequired => "not_required",
                            GradeStatus::Failed => "failed",
                        };
                        if role == GRADERS - 1 {
                            sealed_decision = grade.decision;
                            decision_receipt = Some(id.clone());
                        } else if grade.decision.is_some() {
                            return Err("Independent grader receipt contains a decision".into());
                        }
                        *vote = verdict(&grade);
                        judgment = grade.judgment;
                        elapsed_ns = Some(grade.elapsed_ns);
                        receipt = Some(id.clone());
                    }
                    row = json!({
                        "status":status, "judgment":judgment, "receipt":receipt, "elapsed_ns":elapsed_ns,
                        "attempt_id": attempt.attempt_id, "settlement":attempt.result,
                        "usage_id": attempt.usage_id, "unknown_usage": attempt.unknown_usage,
                        "known_inclusive_tokens":usage.counters.as_ref().map(|_| usage.inclusive),
                        "inclusive_native_tokens":usage.complete(),
                        "af_charged_tokens":attempt.result.as_ref().map(|_| attempt.charged_tokens.to_string()),
                        "calls":calls,
                    });
                }
            }
            row["role"] = json!(["first_grader", "second_grader", "adjudicator"][role]);
            stages.push(row);
        }
        if decision_receipt.is_some() && sealed_decision != decision(votes[0], votes[1], votes[2]) {
            return Err("Sealed decision differs from its independent grades".into());
        }
        slots.push(json!({
            "slot":slot, "association":case.association, "subject_status":case.subject_status,
            "case_id":case.case_id, "family":case.family,
            "subject_receipt":case.receipt, "stages":stages,
            "pair_id":subject_plan.slots[slot].pair_id,
            "variant":subject_plan.slots[slot].variant,
            "repetition":subject_plan.slots[slot].repetition,
            "subject_inclusive_tokens":subject_rows[slot]["inclusive_native_tokens"],
            "subject_elapsed_ns":subject_rows[slot]["elapsed_ns"],
            "answer_text_blinding_concern":case.projected.is_some(),
            "decision":sealed_decision, "decision_receipt":decision_receipt,
        }));
    }
    let phase =
        phase_evidence::summarize(store, task, &subject_plan, &slots, grade_tokens, &policy)?;
    Ok(json!({
        "schema":"af.agent-bench-grading-report/2", "mode":policy.binding.mode(),
        "task_id":task.task_id, "plan_id":task.plan_id, "subject_task":policy.subject_task,
        "subject_result":policy.subject_result, "evaluator_sha256":policy.evaluator_sha256,
        "subject_plan":subject.plan_id, "subject_phase_plan":subject.revision.authority.policy_id,
        "subject_revision":policy.subject_revision, "source_snapshot":task.revision.inputs["source"].snapshot_id,
        "subject_pins":subject_plan.pins, "predecessor":subject_plan.predecessor,
        "subject_inclusive_tokens":policy.subject_tokens, "grading_inclusive_tokens":grade_tokens,
        "known_grading_inclusive_tokens":known_grade_tokens,
        "phase_evidence":phase,
        "slots":slots,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grader_admission_stops_on_overshoot_or_unknown_usage() {
        let mut usage = execution::Usage {
            inclusive: 100_010,
            counters: Some(Default::default()),
            missing: false,
        };
        assert!(usage_stops_admission(&usage, 100_000));
        assert_eq!(usage.complete(), Some(100_010));
        usage.inclusive = 100_000;
        assert!(!usage_stops_admission(&usage, 100_000));
        assert!(usage_stops_admission(&usage, 99_999));
        usage.missing = true;
        assert!(usage_stops_admission(&usage, 100_000));
        assert_eq!(usage.complete(), None);
    }

    #[test]
    fn grading_plan_schema_keeps_live_and_fixture_bindings_distinct() {
        let digest = codex::sha256(b"fixture");
        let mut policy = Policy {
            subject_task: "smoke".into(),
            subject_result: digest.clone(),
            subject_revision: digest.clone(),
            evaluator_sha256: digest.clone(),
            source_digest: digest.clone(),
            engine: digest.clone(),
            transport: PROFILE.config_identity(),
            binding: GradingBinding::LoopbackFixture {
                executable: "/fixture/codex".into(),
                endpoint: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9090),
            },
            subject_tokens: Some(400_000),
            prior_overhead_tokens: 0,
            prior_overhead_ms: 0,
            cases: (0..4)
                .map(|index| Case {
                    association: format!("{index:032x}"),
                    receipt: None,
                    case_id: "N12".into(),
                    family: "family".into(),
                    subject_status: "unstarted".into(),
                    projected: None,
                })
                .collect(),
        };
        assert_eq!(policy.remaining_grader_allowance(Some(0)), 100_000);
        assert_eq!(policy.remaining_grader_allowance(Some(3_999_999)), 1);
        assert_eq!(policy.remaining_grader_allowance(Some(4_000_000)), 0);
        assert_eq!(policy.remaining_grader_allowance(None), 0);
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../schemas/agent-bench-grading-plan-v2.json"
        ))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let fixture = serde_json::to_value(&policy).unwrap();
        assert!(validator.is_valid(&fixture));
        assert!(policy.binding.validate(12).is_ok());
        policy.binding = GradingBinding::CodexPersonal {
            executable: "/fixture/codex".into(),
            account_binding: digest.clone(),
            auth_homes: (0..12)
                .map(|i| PathBuf::from(format!("/fixture/home-{i}")))
                .collect(),
            catalog: InputPin {
                sha256: digest.clone(),
                object_id: digest,
            },
        };
        let mut live = serde_json::to_value(&policy).unwrap();
        assert!(validator.is_valid(&live));
        assert!(policy.binding.validate(12).is_ok());
        TaskAuthorityV1 {
            policy_id: policy.subject_revision.clone(),
            allowed_effects: BTreeSet::from(["read-source".into(), "execute-checks".into()]),
            data_destinations: BTreeSet::from([policy.binding.destination().into()]),
        }
        .validate()
        .unwrap();
        live["binding"]["endpoint"] = json!("127.0.0.1:9090");
        assert!(!validator.is_valid(&live));
        assert!(serde_json::from_value::<Policy>(live).is_err());
    }

    #[test]
    fn projection_preserves_citations_and_contains_only_case_evidence() {
        let oracle = evaluator::EvaluatorCase {
            id: "N12".into(),
            family: "private-family".into(),
            abstention: json!(false),
            oracle_text_verbatim: "only this case".into(),
        };
        let projected = project(
            "original question",
            "alternative path.rs:11",
            "rubric β",
            &oracle,
        );
        let value: serde_json::Value = serde_json::from_str(&projected).unwrap();
        assert_eq!(
            value,
            json!({
                "instruction":INSTRUCTION, "question":"original question",
                "answer":"alternative path.rs:11", "rubric":"rubric β",
                "oracle":{"text":"only this case", "abstention":false},
            })
        );
    }

    #[test]
    fn sealed_grade_shape_and_decisions_preserve_unresolved_outcomes() {
        use Verdict::*;
        let grade = Grade {
            index: 2,
            status: GradeStatus::Graded,
            judgment: Some(Judgment {
                verdict: Unresolved,
                rationale: "conflicting evidence".into(),
            }),
            decision: decision(Some(Correct), Some(Incorrect), Some(Unresolved)),
            cumulative_tokens: Some(83),
            admission_stopped: false,
            elapsed_ns: 1_000_000,
            calls: vec![codex::sha256(b"fixture")],
        };
        assert!(grade.decision == Some(Unresolved));
        assert!(decision(Some(Correct), Some(Incorrect), Some(Correct)) == Some(Correct));
        assert!(decision(Some(Disqualified), Some(Disqualified), None) == Some(Disqualified));
        assert!(decision(Some(Correct), None, Some(Correct)).is_none());
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../schemas/agent-bench-grading-receipt-v3.json"
        ))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let mut wire = serde_json::to_value(grade).unwrap();
        assert!(validator.is_valid(&wire));
        assert!(serde_json::from_value::<Grade>(wire.clone()).is_ok());
        let mut retired = wire.clone();
        retired.as_object_mut().unwrap().remove("admission_stopped");
        assert!(!validator.is_valid(&retired));
        assert!(serde_json::from_value::<Grade>(retired).is_err());
        wire["unexpected"] = json!(true);
        assert!(!validator.is_valid(&wire));
        assert!(serde_json::from_value::<Grade>(wire).is_err());
    }
}
